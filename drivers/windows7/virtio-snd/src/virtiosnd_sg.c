/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_sg.h"
#include "virtiosnd_sg_core.h"

static NTSTATUS virtiosnd_sg_status_from_rc(int rc)
{
    switch (rc) {
    case VIRTIO_OK:
        return STATUS_SUCCESS;
    case VIRTIO_ERR_NOSPC:
        return STATUS_BUFFER_TOO_SMALL;
    case VIRTIO_ERR_RANGE:
    case VIRTIO_ERR_INVAL:
    default:
        return STATUS_INVALID_PARAMETER;
    }
}

ULONG VirtIoSndSgMaxElemsForMdlRegion(_In_ PMDL Mdl,
                                     _In_ ULONG BufferBytes,
                                     _In_ ULONG OffsetBytes,
                                     _In_ ULONG LengthBytes,
                                     _In_ BOOLEAN Wrap)
{
    ULONG mdl_byte_offset;
    ULONG mdl_byte_count;

    if (Mdl == NULL) {
        return 0;
    }

    mdl_byte_offset = MmGetMdlByteOffset(Mdl);
    mdl_byte_count = MmGetMdlByteCount(Mdl);

    return (ULONG)virtiosnd_sg_max_elems_for_region(mdl_byte_offset,
                                                    mdl_byte_count,
                                                    BufferBytes,
                                                    OffsetBytes,
                                                    LengthBytes,
                                                    Wrap ? VIRTIO_TRUE : VIRTIO_FALSE);
}

NTSTATUS VirtIoSndSgBuildFromMdlRegion(_In_ PMDL Mdl,
                                      _In_ ULONG BufferBytes,
                                      _In_ ULONG OffsetBytes,
                                      _In_ ULONG LengthBytes,
                                      _In_ BOOLEAN Wrap,
                                      _Out_writes_(MaxElems) virtio_sg_entry_t *Out,
                                      _In_ USHORT MaxElems,
                                      _Out_ USHORT *OutCount)
{
    ULONG mdl_byte_offset;
    ULONG mdl_byte_count;
    ULONGLONG span_bytes;
    ULONGLONG pfn_count_ull;
    uint32_t pfn_count;
    const PFN_NUMBER *pfns;
    int rc;

    if (OutCount != NULL) {
        *OutCount = 0;
    }

    if (Mdl == NULL || Out == NULL || OutCount == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    mdl_byte_offset = MmGetMdlByteOffset(Mdl);
    mdl_byte_count = MmGetMdlByteCount(Mdl);

    /*
     * Compute the PFN array length. An MDL maps `ByteCount` bytes starting at
     * `ByteOffset` into the first page.
     */
    span_bytes = (ULONGLONG)mdl_byte_offset + (ULONGLONG)mdl_byte_count;
    pfn_count_ull = (span_bytes + (ULONGLONG)VIRTIOSND_SG_PAGE_SIZE - 1u) >> VIRTIOSND_SG_PAGE_SHIFT;
    if (pfn_count_ull > 0xffffffffull) {
        return STATUS_INVALID_PARAMETER;
    }
    pfn_count = (uint32_t)pfn_count_ull;

    C_ASSERT(sizeof(PFN_NUMBER) == sizeof(uintptr_t));
    pfns = MmGetMdlPfnArray(Mdl);

    rc = virtiosnd_sg_build_from_pfn_array_region((const uintptr_t *)pfns,
                                                  pfn_count,
                                                  mdl_byte_offset,
                                                  mdl_byte_count,
                                                  BufferBytes,
                                                  OffsetBytes,
                                                  LengthBytes,
                                                  Wrap ? VIRTIO_TRUE : VIRTIO_FALSE,
                                                  Out,
                                                  (uint16_t)MaxElems,
                                                  (uint16_t *)OutCount);
    if (rc != VIRTIO_OK) {
        *OutCount = 0;
        return virtiosnd_sg_status_from_rc(rc);
    }

    /*
     * Make CPU writes visible to the device DMA engine (virtio OUT buffer).
     *
     * KeFlushIoBuffers operates on the whole MDL; subrange flush would require
     * constructing a partial MDL, which we avoid to keep this helper
     * DISPATCH_LEVEL-safe and allocation-free. Audio PCM buffers are small, so
     * flushing the full MDL is acceptable.
     */
    KeFlushIoBuffers(Mdl, FALSE /* ReadOperation */, TRUE /* DmaOperation */);

    return STATUS_SUCCESS;
}


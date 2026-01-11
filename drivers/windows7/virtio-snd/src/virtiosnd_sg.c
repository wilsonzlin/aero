/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_sg.h"
#include "virtiosnd_sg_core.h"

/* Ensure the SG builder's fixed 4KiB page assumptions match the OS. */
C_ASSERT(PAGE_SHIFT == VIRTIOSND_SG_PAGE_SHIFT);
C_ASSERT(PAGE_SIZE == VIRTIOSND_SG_PAGE_SIZE);

/*
 * Allow KeFlushIoBuffers to be stubbed in host/unit builds if desired.
 * The virtio-snd driver build uses the real WDK KeFlushIoBuffers.
 */
#ifndef VIRTIOSND_SG_FLUSH_IO_BUFFERS
#define VIRTIOSND_SG_FLUSH_IO_BUFFERS KeFlushIoBuffers
#endif

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

_Use_decl_annotations_
VOID VirtIoSndSgFlushIoBuffers(PMDL Mdl, BOOLEAN DeviceWrites)
{
    ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Mdl == NULL) {
        return;
    }

    /*
     * Cache coherency rules:
     *
     * - DeviceWrites == FALSE (TX / device reads from memory):
     *     Flush CPU writes before the device DMA engine reads the buffer.
     *     (ReadOperation=FALSE)
     *
     * - DeviceWrites == TRUE (RX / device writes to memory):
     *     Flush/invalidate CPU cache lines so dirty data won't later be written
     *     back on top of device-written bytes.
     *     (ReadOperation=TRUE)
     *
     * For RX buffers the caller must call this again after the device signals
     * completion, before reading device-written data.
     *
     * Note: KeFlushIoBuffers operates on the whole MDL. Subrange flushing would
     * require constructing a partial MDL, which we avoid to keep the helper
     * DISPATCH_LEVEL-safe and allocation-free. Audio PCM buffers are small, so
     * flushing the full MDL is acceptable.
     */
    VIRTIOSND_SG_FLUSH_IO_BUFFERS(Mdl, DeviceWrites ? TRUE : FALSE, TRUE /* DmaOperation */);
}

ULONG VirtIoSndSgMaxElemsForMdlRegion(_In_ PMDL Mdl,
                                      _In_ ULONG BufferBytes,
                                      _In_ ULONG OffsetBytes,
                                      _In_ ULONG LengthBytes,
                                      _In_ BOOLEAN Wrap)
{
    ULONG mdl_byte_offset;
    ULONG mdl_byte_count;

    ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

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

_Use_decl_annotations_
NTSTATUS VirtIoSndSgBuildFromMdlRegion(PMDL Mdl,
                                      ULONG BufferBytes,
                                      ULONG OffsetBytes,
                                      ULONG LengthBytes,
                                      BOOLEAN Wrap,
                                      virtio_sg_entry_t *Out,
                                      USHORT MaxElems,
                                      USHORT *OutCount)
{
    return VirtIoSndSgBuildFromMdlRegionEx(Mdl,
                                          BufferBytes,
                                          OffsetBytes,
                                          LengthBytes,
                                          Wrap,
                                          FALSE /* DeviceWrites (TX) */,
                                          Out,
                                          MaxElems,
                                          OutCount);
}

NTSTATUS VirtIoSndSgBuildFromMdlRegionEx(_In_ PMDL Mdl,
                                        _In_ ULONG BufferBytes,
                                        _In_ ULONG OffsetBytes,
                                        _In_ ULONG LengthBytes,
                                        _In_ BOOLEAN Wrap,
                                        _In_ BOOLEAN DeviceWrites,
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

    ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

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
                                                  DeviceWrites ? VIRTIO_TRUE : VIRTIO_FALSE,
                                                  Out,
                                                  (uint16_t)MaxElems,
                                                  (uint16_t *)OutCount);
    if (rc != VIRTIO_OK) {
        *OutCount = 0;
        return virtiosnd_sg_status_from_rc(rc);
    }

    /*
     * Flush caches for DMA (TX=device reads, RX=device writes).
     * For RX buffers, the caller must flush again after completion before
     * consuming device-written PCM data.
     */
    VirtIoSndSgFlushIoBuffers(Mdl, DeviceWrites);

    return STATUS_SUCCESS;
}

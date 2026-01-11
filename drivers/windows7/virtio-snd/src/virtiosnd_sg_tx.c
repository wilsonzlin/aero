/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_queue.h"
#include "virtiosnd_sg.h"
#include "virtiosnd_sg_tx.h"

/*
 * VIRTIOSND_SG (virtio-snd queue API) is layout-compatible with virtio_sg_entry_t
 * (virtio common library). For TX, both represent (addr,len,device_writes=FALSE).
 */
C_ASSERT(sizeof(VIRTIOSND_SG) == sizeof(virtio_sg_entry_t));
C_ASSERT(FIELD_OFFSET(VIRTIOSND_SG, addr) == FIELD_OFFSET(virtio_sg_entry_t, addr));
C_ASSERT(FIELD_OFFSET(VIRTIOSND_SG, len) == FIELD_OFFSET(virtio_sg_entry_t, len));
C_ASSERT(FIELD_OFFSET(VIRTIOSND_SG, write) == FIELD_OFFSET(virtio_sg_entry_t, device_writes));

_Use_decl_annotations_
ULONG VirtIoSndTxSgMaxElemsForMdlRegion(PMDL Mdl, ULONG BufferBytes, ULONG OffsetBytes, ULONG LengthBytes, BOOLEAN Wrap)
{
    return VirtIoSndSgMaxElemsForMdlRegion(Mdl, BufferBytes, OffsetBytes, LengthBytes, Wrap);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndTxSgBuildFromMdlRegion(PMDL Mdl,
                                        ULONG BufferBytes,
                                        ULONG OffsetBytes,
                                        ULONG LengthBytes,
                                        BOOLEAN Wrap,
                                        VIRTIOSND_SG *Out,
                                        USHORT MaxElems,
                                        USHORT *OutCount)
{
    return VirtIoSndSgBuildFromMdlRegion(
        Mdl, BufferBytes, OffsetBytes, LengthBytes, Wrap, (virtio_sg_entry_t *)Out, MaxElems, OutCount);
}


/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"

/*
 * Convenience wrappers for building virtio-snd TX (device reads guest memory)
 * SG lists
 * directly into VIRTIOSND_SG arrays.
 *
 * These wrap VirtIoSndSgBuildFromMdlRegion(), which emits virtio_sg_entry_t
 * (addr,len,device_writes). VIRTIOSND_SG is layout-compatible for TX usage
 * (BOOLEAN write == 0), so callers don't need to depend on the virtio common
 * SG type.
 */

#ifdef __cplusplus
extern "C" {
#endif

ULONG VirtIoSndTxSgMaxElemsForMdlRegion(_In_ PMDL Mdl,
                                       _In_ ULONG BufferBytes,
                                       _In_ ULONG OffsetBytes,
                                       _In_ ULONG LengthBytes,
                                       _In_ BOOLEAN Wrap);

NTSTATUS VirtIoSndTxSgBuildFromMdlRegion(_In_ PMDL Mdl,
                                         _In_ ULONG BufferBytes,
                                         _In_ ULONG OffsetBytes,
                                         _In_ ULONG LengthBytes,
                                         _In_ BOOLEAN Wrap,
                                         _Out_writes_(MaxElems) VIRTIOSND_SG *Out,
                                         _In_ USHORT MaxElems,
                                         _Out_ USHORT *OutCount);

#ifdef __cplusplus
} /* extern "C" */
#endif

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

/*
 * Use the Aero Windows 7 virtio common SG entry shape (virtio_sg_entry_t).
 *
 * Note: This header name conflicts with the newer virtqueue implementation
 * under drivers/windows/virtio/common. Use an explicit relative include to
 * avoid accidental header resolution changes when include paths vary between
 * drivers.
 */
#include "../../virtio/common/include/virtqueue_split.h"

/*
 * DISPATCH_LEVEL-safe helpers for converting an MDL-backed circular PCM buffer
 * region into a compact virtio scatter/gather list.
 *
 * The virtio-snd TX (device reads from guest memory) and RX/capture (device
 * writes to guest memory) paths require a (phys,len) list that:
 *  - respects ring wrap-around,
 *  - splits on page boundaries,
 *  - coalesces physically contiguous pages into larger segments,
 *  - performs cache maintenance for DMA buffers.
 */

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Returns a conservative upper bound on SG entries required for the described
 * region. Returns 0 on invalid parameters.
 */
ULONG VirtIoSndSgMaxElemsForMdlRegion(_In_ PMDL Mdl,
                                      _In_ ULONG BufferBytes,
                                      _In_ ULONG OffsetBytes,
                                      _In_ ULONG LengthBytes,
                                      _In_ BOOLEAN Wrap);

/*
 * Flush/invalidate CPU caches for an MDL used as a DMA buffer.
 *
 * DeviceWrites follows the virtio convention:
 *  - FALSE: device reads from guest memory (TX / OUT descriptors).
 *  - TRUE:  device writes to guest memory (RX / IN descriptors).
 *
 * This helper must be callable at DISPATCH_LEVEL and does not allocate.
 *
 * Note: For DeviceWrites == TRUE, callers must invoke this again after the
 * device completes (before reading device-written bytes).
 */
VOID VirtIoSndSgFlushIoBuffers(_In_ PMDL Mdl, _In_ BOOLEAN DeviceWrites);

/*
 * Builds a virtio scatter/gather list for the described region.
 *
 * On success, Out[0..*OutCount) contains the SG entries and *OutCount is set.
 * On failure, *OutCount is set to 0.
 *
 * This helper also calls VirtIoSndSgFlushIoBuffers(Mdl, DeviceWrites) before
 * returning. For DeviceWrites == TRUE, callers must flush again after DMA
 * completion (before reading captured audio samples).
 */
NTSTATUS VirtIoSndSgBuildFromMdlRegion(_In_ PMDL Mdl,
                                        _In_ ULONG BufferBytes,
                                        _In_ ULONG OffsetBytes,
                                        _In_ ULONG LengthBytes,
                                        _In_ BOOLEAN Wrap,
                                        _In_ BOOLEAN DeviceWrites,
                                        _Out_writes_(MaxElems) virtio_sg_entry_t *Out,
                                        _In_ USHORT MaxElems,
                                        _Out_ USHORT *OutCount);

#ifdef __cplusplus
} /* extern "C" */
#endif

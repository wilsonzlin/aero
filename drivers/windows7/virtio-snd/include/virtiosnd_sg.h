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
 * The virtio-snd TX path (device-readable buffers) requires a (phys,len) list
 * that:
 *  - respects ring wrap-around,
 *  - splits on page boundaries,
 *  - coalesces physically contiguous pages into larger segments,
 *  - flushes CPU caches for device-readable (OUT) DMA buffers.
 */

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
 * Builds a virtio scatter/gather list for the described region.
 *
 * On success, Out[0..*OutCount) contains the SG entries and *OutCount is set.
 * On failure, *OutCount is set to 0.
 */
NTSTATUS VirtIoSndSgBuildFromMdlRegion(_In_ PMDL Mdl,
                                      _In_ ULONG BufferBytes,
                                      _In_ ULONG OffsetBytes,
                                      _In_ ULONG LengthBytes,
                                      _In_ BOOLEAN Wrap,
                                      _Out_writes_(MaxElems) virtio_sg_entry_t *Out,
                                      _In_ USHORT MaxElems,
                                      _Out_ USHORT *OutCount);

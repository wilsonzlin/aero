#ifndef VIRTIO_KMDF_COMMON_VIRTQUEUE_RING_H_
#define VIRTIO_KMDF_COMMON_VIRTQUEUE_RING_H_

/*
 * Split virtqueue ring allocation/layout helpers for KMDF virtio drivers.
 *
 * This module is intentionally limited to:
 *  - computing split ring layout (desc/avail/used) and required alignments
 *  - allocating one contiguous DMA-safe common buffer for the ring
 *  - returning CPU pointers and device DMA addresses for each sub-structure
 *
 * It does NOT implement descriptor management or request tracking.
 */

#include "virtio_dma.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Virtqueue ring memory barriers.
 *
 * Publishing buffers to the device (driver -> device):
 *   1) Write/initialize descriptor(s) and any referenced data buffers.
 *   2) Write avail->ring[slot] = head_desc_index.
 *   3) VirtqWmb();  // ensure ring entry is visible before idx update
 *   4) Write avail->idx = new_idx.
 *
 * Consuming completions from the device (device -> driver):
 *   1) Read used->idx into new_idx.
 *   2) VirtqRmb();  // ensure used ring entries are visible after idx read
 *   3) Read used->ring[old_idx..new_idx-1].
 *
 * Note: KeMemoryBarrier() is available on Windows 7 and provides a full barrier.
 */
#define VirtqWmb() KeMemoryBarrier()
#define VirtqRmb() KeMemoryBarrier()

/*
 * Virtio 1.0 "split virtqueue" structures.
 *
 * These use host-endian integer types. Virtio fields are little-endian on the
 * wire; Windows 7 x86/x64 are little-endian so the layout matches the spec.
 */

#include <pshpack1.h>

struct virtq_desc {
    UINT64 addr;
    UINT32 len;
    UINT16 flags;
    UINT16 next;
};

struct virtq_avail {
    UINT16 flags;
    UINT16 idx;
    UINT16 ring[ANYSIZE_ARRAY]; /* queueSize entries, then optional used_event */
};

struct virtq_used_elem {
    UINT32 id;
    UINT32 len;
};

struct virtq_used {
    UINT16 flags;
    UINT16 idx;
    struct virtq_used_elem ring[ANYSIZE_ARRAY]; /* queueSize entries, then optional avail_event */
};

#include <poppack.h>

/* Compile-time validation of virtq_desc layout (required by the virtio spec). */
C_ASSERT(sizeof(struct virtq_desc) == 16);
C_ASSERT(FIELD_OFFSET(struct virtq_desc, addr) == 0);
C_ASSERT(FIELD_OFFSET(struct virtq_desc, len) == 8);
C_ASSERT(FIELD_OFFSET(struct virtq_desc, flags) == 12);
C_ASSERT(FIELD_OFFSET(struct virtq_desc, next) == 14);
C_ASSERT(sizeof(struct virtq_used_elem) == 8);

typedef struct _VIRTQUEUE_RING_LAYOUT {
    SIZE_T DescSize;
    SIZE_T AvailSize;
    SIZE_T UsedSize;

    SIZE_T DescOffset;  /* aligned to 16 */
    SIZE_T AvailOffset; /* aligned to 2 */
    SIZE_T UsedOffset;  /* aligned to 4 */

    SIZE_T TotalSize;
} VIRTQUEUE_RING_LAYOUT;

typedef struct _VIRTQUEUE_RING_DMA {
    volatile struct virtq_desc* Desc;  /* CPU VA */
    volatile struct virtq_avail* Avail; /* CPU VA */
    volatile struct virtq_used* Used;  /* CPU VA */

    UINT64 DescDma;
    UINT64 AvailDma;
    UINT64 UsedDma;

    USHORT QueueSize;

    VIRTIO_COMMON_BUFFER CommonBuffer;
} VIRTQUEUE_RING_DMA;

_Must_inspect_result_
NTSTATUS
VirtqueueRingLayoutCompute(
    _In_ USHORT QueueSize,
    _In_ BOOLEAN EventIdxEnabled,
    _Out_ VIRTQUEUE_RING_LAYOUT* Layout);

/*
 * Allocate a single contiguous DMA common buffer for a split virtqueue ring.
 *
 * The allocation is attempted with PAGE_SIZE alignment first (recommended). If
 * that is not supported by the DMA enabler, the implementation may fall back
 * to 16-byte alignment (minimum required by the virtio split ring descriptor
 * table).
 */
_Must_inspect_result_
NTSTATUS
VirtqueueRingDmaAlloc(
    _In_ VIRTIO_DMA_CONTEXT* DmaCtx,
    _In_opt_ WDFOBJECT ParentObject,
    _In_ USHORT QueueSize,
    _In_ BOOLEAN EventIdxEnabled,
    _Out_ VIRTQUEUE_RING_DMA* Ring);

/*
 * Free ring DMA allocation (PASSIVE_LEVEL).
 *
 * This function may be called from EvtDeviceReleaseHardware.
 */
VOID
VirtqueueRingDmaFree(
    _Inout_ VIRTQUEUE_RING_DMA* Ring);

#if DBG
VOID
VirtqueueRingDmaSelfTest(
    _In_ const VIRTQUEUE_RING_DMA* Ring);
#endif

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* VIRTIO_KMDF_COMMON_VIRTQUEUE_RING_H_ */

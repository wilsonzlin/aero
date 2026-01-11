/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"
#include "virtiosnd_dma.h"

/*
 * Shared split-ring virtqueue implementation (drivers/windows7/virtio/common).
 *
 * This header is intentionally included via a relative path to avoid
 * accidentally picking up the unrelated `drivers/windows/virtio/common`
 * implementation when multiple virtio trees are on the include path.
 */
#include "../../virtio/common/include/virtqueue_split.h"

/*
 * Indirect descriptor sizing (Aero contract v1).
 *
 * virtio-snd requests are submitted as:
 *   header + payload SG elements + response/status
 *
 * Contract v1 uses 16-entry indirect tables:
 *   1 header + up to 14 payload SG elements + 1 response/status.
 *
 * The driver allocates one indirect table per ring entry so the maximum number
 * of in-flight requests equals the ring size.
 */
#define VIRTIOSND_QUEUE_SPLIT_INDIRECT_MAX_DESC 16u

typedef struct _VIRTIOSND_QUEUE_SPLIT {
    USHORT QueueIndex;
    USHORT QueueSize;

    virtqueue_split_t Vq;

    /*
     * Protects all access to Vq (descriptor free list, avail/used indices, etc).
     *
     * Submit/PopUsed/Kick are expected to be callable at IRQL <= DISPATCH_LEVEL.
     * The implementation uses KeAcquireSpinLock when called below DISPATCH_LEVEL,
     * and KeAcquireSpinLockAtDpcLevel when already at DISPATCH_LEVEL.
    */
    KSPIN_LOCK Lock;

    /*
     * virtqueue_split uses the generic virtio OS shim. virtio-snd provides a
     * small per-queue shim context so the shared code can allocate DMA-able
     * buffers via virtiosnd_dma.
     *
     * The backing allocations are tracked internally by the virtiosnd queue
     * implementation; callers should treat this as opaque.
     */
    struct _VIRTIOSND_QUEUE_SPLIT_OS_CTX {
        PVIRTIOSND_DMA_CONTEXT DmaCtx;
        LIST_ENTRY DmaAllocs;
    } OsCtx;

    /* Split ring (desc + avail + used) memory (DMA-safe). */
    virtio_dma_buffer_t Ring;

    /* Precomputed virtio-pci modern notify MMIO address for this queue. */
    volatile UINT16* NotifyAddr;
} VIRTIOSND_QUEUE_SPLIT, *PVIRTIOSND_QUEUE_SPLIT;

#ifdef __cplusplus
extern "C" {
#endif

_Must_inspect_result_ NTSTATUS
VirtioSndQueueSplitCreate(
    _In_ PVIRTIOSND_DMA_CONTEXT DmaCtx,
    _Inout_ VIRTIOSND_QUEUE_SPLIT* qs,
    _In_ USHORT queue_index,
    _In_ USHORT queue_size,
    _In_ BOOLEAN event_idx,
    _In_ BOOLEAN indirect,
    _In_ volatile UINT16* notify_addr,
    /*out*/ _Out_ VIRTIOSND_QUEUE* out_queue,
    /*out*/ _Out_ UINT64* out_desc_pa,
    /*out*/ _Out_ UINT64* out_avail_pa,
    /*out*/ _Out_ UINT64* out_used_pa);

VOID
VirtioSndQueueSplitDestroy(_In_ PVIRTIOSND_DMA_CONTEXT DmaCtx, _Inout_ VIRTIOSND_QUEUE_SPLIT* qs);

typedef VOID EVT_VIRTIOSND_QUEUE_SPLIT_USED(
    _In_ USHORT QueueIndex,
    _In_opt_ void* Cookie,
    _In_ UINT32 UsedLen,
    _In_opt_ void* Context);

/*
 * Drains all currently used entries from the queue, intended for DPC context.
 * The callback is invoked once per completed buffer.
 */
VOID
VirtioSndQueueSplitDrainUsed(_Inout_ VIRTIOSND_QUEUE_SPLIT* qs,
                             _In_ EVT_VIRTIOSND_QUEUE_SPLIT_USED* Callback,
                              _In_opt_ void* Context);
#ifdef __cplusplus
} /* extern "C" */
#endif

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"
#include "virtiosnd_dma.h"

#include "virtqueue_split.h"

/*
 * Indirect descriptor sizing (Aero contract v1).
 *
 * virtio-snd requests are submitted as:
 *   header + payload SG elements + response/status
 *
 * 16 descriptors covers the expected maximum shape:
 *   1 header + up to 14 payload SG elements + 1 response/status.
 *
 * The driver allocates one indirect table per ring entry so the maximum number
 * of in-flight requests equals the ring size.
 */
#define VIRTIOSND_QUEUE_SPLIT_INDIRECT_MAX_DESC 16u
#define VIRTIOSND_QUEUE_SPLIT_INDIRECT_TABLE_COUNT(_qsz) (_qsz)

typedef struct _VIRTIOSND_QUEUE_SPLIT {
    USHORT QueueIndex;
    USHORT QueueSize;

    VIRTQ_SPLIT* Vq;

    /*
     * Protects all access to Vq (descriptor free list, avail/used indices, etc).
     *
     * Submit/PopUsed/Kick are expected to be callable at IRQL <= DISPATCH_LEVEL.
     * The implementation uses KeAcquireSpinLock when called below DISPATCH_LEVEL,
     * and KeAcquireSpinLockAtDpcLevel when already at DISPATCH_LEVEL.
     */
    KSPIN_LOCK Lock;

    /* Transport-supplied virtio-pci modern notify information. */
    volatile UCHAR* NotifyBase;
    ULONG NotifyOffMultiplier;
    SIZE_T NotifyLength;
    USHORT QueueNotifyOff;

    /* Optional precomputed notify register address (notify_base + off * mult). */
    volatile UINT16* NotifyAddr;

    /* Split ring (desc + avail + used) memory (DMA-safe). */
    VIRTIOSND_DMA_BUFFER Ring;

    /* Optional indirect descriptor table pool (DMA-safe). */
    VIRTIOSND_DMA_BUFFER IndirectPool;
    USHORT IndirectTableCount;
    USHORT IndirectMaxDesc;
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
    _In_ volatile UCHAR* notify_base,
    _In_ ULONG notify_off_multiplier,
    _In_ SIZE_T notify_length,
    _In_ USHORT queue_notify_off,
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

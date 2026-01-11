#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"

#include "virtqueue_split.h"

typedef struct _VIRTIOSND_QUEUE_SPLIT {
    VIRTQ_SPLIT* Vq;

    /*
     * Protects all access to Vq (descriptor free list, avail/used indices, etc).
     *
     * Submit/PopUsed/Kick are expected to be callable at IRQL <= DISPATCH_LEVEL.
     * The implementation uses KeAcquireSpinLock when called below DISPATCH_LEVEL,
     * and KeAcquireSpinLockAtDpcLevel when already at DISPATCH_LEVEL.
     */
    KSPIN_LOCK Lock;

    USHORT QueueIndex;

    /* Transport-supplied virtio-pci modern notify information. */
    volatile UCHAR* NotifyBase;
    ULONG NotifyOffMultiplier;
    USHORT QueueNotifyOff;

    /* Optional precomputed notify register address. */
    volatile ULONG* NotifyAddr;

    /* Split ring (desc + avail + used) memory, physically contiguous. */
    PVOID RingVa;
    PHYSICAL_ADDRESS RingPa;
    SIZE_T RingBytes;

    /* Optional physically contiguous indirect descriptor table pool. */
    PVOID IndirectPoolVa;
    PHYSICAL_ADDRESS IndirectPoolPa;
    SIZE_T IndirectPoolBytes;
} VIRTIOSND_QUEUE_SPLIT, *PVIRTIOSND_QUEUE_SPLIT;

_Must_inspect_result_ NTSTATUS
VirtioSndQueueSplitCreate(
    _Inout_ VIRTIOSND_QUEUE_SPLIT* qs,
    _In_ USHORT queue_index,
    _In_ USHORT queue_size,
    _In_ BOOLEAN event_idx,
    _In_ BOOLEAN indirect,
    _In_ volatile UCHAR* notify_base,
    _In_ ULONG notify_off_multiplier,
    _In_ USHORT queue_notify_off,
    /*out*/ _Out_ VIRTIOSND_QUEUE* out_queue,
    /*out*/ _Out_ UINT64* out_desc_pa,
    /*out*/ _Out_ UINT64* out_avail_pa,
    /*out*/ _Out_ UINT64* out_used_pa);

VOID
VirtioSndQueueSplitDestroy(_Inout_ VIRTIOSND_QUEUE_SPLIT* qs);


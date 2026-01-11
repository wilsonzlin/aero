/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd.h"
#include "virtiosnd_queue_split.h"

typedef struct _VIRTIOSND_QUEUE_SPLIT_LOCK_STATE {
    KIRQL OldIrql;
    BOOLEAN AtDpcLevel;
} VIRTIOSND_QUEUE_SPLIT_LOCK_STATE;

static __forceinline VOID
VirtioSndQueueSplitLock(_Inout_ VIRTIOSND_QUEUE_SPLIT* qs, _Out_ VIRTIOSND_QUEUE_SPLIT_LOCK_STATE* state)
{
    KIRQL irql;

    irql = KeGetCurrentIrql();
    ASSERT(irql <= DISPATCH_LEVEL);

    state->AtDpcLevel = (irql >= DISPATCH_LEVEL);
    state->OldIrql = irql;

    if (state->AtDpcLevel) {
        KeAcquireSpinLockAtDpcLevel(&qs->Lock);
    } else {
        KeAcquireSpinLock(&qs->Lock, &state->OldIrql);
    }
}

static __forceinline VOID
VirtioSndQueueSplitUnlock(_Inout_ VIRTIOSND_QUEUE_SPLIT* qs, _In_ const VIRTIOSND_QUEUE_SPLIT_LOCK_STATE* state)
{
    if (state->AtDpcLevel) {
        KeReleaseSpinLockFromDpcLevel(&qs->Lock);
    } else {
        KeReleaseSpinLock(&qs->Lock, state->OldIrql);
    }
}

static NTSTATUS
VirtioSndQueueSplitSubmit(
    _In_ void* ctx,
    _In_reads_(sg_count) const VIRTIOSND_SG* sg,
    _In_ USHORT sg_count,
    _In_opt_ void* cookie)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    UINT16 head;
    NTSTATUS status;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || qs->Vq == NULL || sg == NULL || sg_count == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    C_ASSERT(sizeof(VIRTIOSND_SG) == sizeof(VIRTQ_SG));
    C_ASSERT(offsetof(VIRTIOSND_SG, addr) == offsetof(VIRTQ_SG, addr));
    C_ASSERT(offsetof(VIRTIOSND_SG, len) == offsetof(VIRTQ_SG, len));
    C_ASSERT(offsetof(VIRTIOSND_SG, write) == offsetof(VIRTQ_SG, write));

    VirtioSndQueueSplitLock(qs, &lock_state);

    status = VirtqSplitAddBuffer(qs->Vq, (const VIRTQ_SG*)sg, sg_count, cookie, &head);
    if (NT_SUCCESS(status)) {
        VirtqSplitPublish(qs->Vq, head);
    }

    VirtioSndQueueSplitUnlock(qs, &lock_state);
    return status;
}

static BOOLEAN
VirtioSndQueueSplitPopUsed(_In_ void* ctx, _Out_ void** cookie_out, _Out_ UINT32* used_len_out)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    NTSTATUS status;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || qs->Vq == NULL || cookie_out == NULL || used_len_out == NULL) {
        return FALSE;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);

    if (!VirtqSplitHasUsed(qs->Vq)) {
        VirtioSndQueueSplitUnlock(qs, &lock_state);
        return FALSE;
    }

    status = VirtqSplitGetUsed(qs->Vq, cookie_out, used_len_out);

    VirtioSndQueueSplitUnlock(qs, &lock_state);

    if (!NT_SUCCESS(status)) {
        *cookie_out = NULL;
        *used_len_out = 0;
        return FALSE;
    }

    return TRUE;
}

static VOID
VirtioSndQueueSplitKick(_In_ void* ctx)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    volatile ULONG* addr;
    BOOLEAN should_kick;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || qs->Vq == NULL) {
        return;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);

    should_kick = VirtqSplitKickPrepare(qs->Vq);

    if (should_kick) {
        /*
         * Ensure all ring writes (including the avail->idx update performed by
         * VirtqSplitPublish) are globally visible before issuing the MMIO notify.
         */
        KeMemoryBarrier();

        addr = qs->NotifyAddr;
        if (addr == NULL && qs->NotifyBase != NULL && qs->NotifyOffMultiplier != 0) {
            addr = (volatile ULONG*)(qs->NotifyBase + (ULONG)qs->QueueNotifyOff * qs->NotifyOffMultiplier);
        }

        if (addr != NULL) {
            WRITE_REGISTER_ULONG((volatile ULONG*)addr, (ULONG)qs->QueueIndex);
        }
    }

    /* Reset batching bookkeeping even if notification is suppressed. */
    VirtqSplitKickCommit(qs->Vq);

    VirtioSndQueueSplitUnlock(qs, &lock_state);
}

static const VIRTIOSND_QUEUE_OPS g_VirtioSndQueueSplitOps = {
    VirtioSndQueueSplitSubmit,
    VirtioSndQueueSplitPopUsed,
    VirtioSndQueueSplitKick,
};

_Use_decl_annotations_
NTSTATUS
VirtioSndQueueSplitCreate(
    PVIRTIOSND_DMA_CONTEXT DmaCtx,
    VIRTIOSND_QUEUE_SPLIT* qs,
    USHORT queue_index,
    USHORT queue_size,
    BOOLEAN event_idx,
    BOOLEAN indirect,
    volatile UCHAR* notify_base,
    ULONG notify_off_multiplier,
    USHORT queue_notify_off,
    VIRTIOSND_QUEUE* out_queue,
    UINT64* out_desc_pa,
    UINT64* out_avail_pa,
    UINT64* out_used_pa)
{
    NTSTATUS status;
    SIZE_T state_bytes;
    SIZE_T ring_bytes;
    USHORT indirect_table_count;
    USHORT indirect_max_desc;

    if (out_queue != NULL) {
        out_queue->Ops = NULL;
        out_queue->Ctx = NULL;
    }
    if (out_desc_pa != NULL) {
        *out_desc_pa = 0;
    }
    if (out_avail_pa != NULL) {
        *out_avail_pa = 0;
    }
    if (out_used_pa != NULL) {
        *out_used_pa = 0;
    }

    if (DmaCtx == NULL || qs == NULL || out_queue == NULL || out_desc_pa == NULL || out_avail_pa == NULL || out_used_pa == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        return STATUS_INVALID_DEVICE_STATE;
    }

    RtlZeroMemory(qs, sizeof(*qs));
    KeInitializeSpinLock(&qs->Lock);

    qs->QueueIndex = queue_index;
    qs->NotifyBase = notify_base;
    qs->NotifyOffMultiplier = notify_off_multiplier;
    qs->QueueNotifyOff = queue_notify_off;

    if (notify_base != NULL && notify_off_multiplier != 0) {
        qs->NotifyAddr = (volatile ULONG*)(notify_base + (ULONG)queue_notify_off * notify_off_multiplier);
    }

    ring_bytes = VirtqSplitRingMemSize(queue_size, PAGE_SIZE, event_idx);
    if (ring_bytes == 0) {
        status = STATUS_INVALID_PARAMETER;
        goto Fail;
    }

    status = VirtIoSndAllocCommonBuffer(DmaCtx, ring_bytes, FALSE, &qs->Ring);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    /*
     * This DMA buffer is shared with the (potentially untrusted) device; clear it
     * to avoid leaking stale kernel memory.
     */
    RtlZeroMemory(qs->Ring.Va, ring_bytes);

    state_bytes = VirtqSplitStateSize(queue_size);
    qs->Vq = (VIRTQ_SPLIT*)ExAllocatePoolWithTag(NonPagedPool, state_bytes, VIRTIOSND_POOL_TAG);
    if (qs->Vq == NULL) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Fail;
    }

    indirect_table_count = 0;
    indirect_max_desc = 0;
    if (indirect) {
        /* One indirect table per potential in-flight request (best-effort). */
        indirect_table_count = queue_size;
        indirect_max_desc = (queue_size < 32) ? queue_size : 32;

        if (indirect_table_count != 0 && indirect_max_desc != 0) {
            SIZE_T indirect_bytes = sizeof(VIRTQ_DESC) * (SIZE_T)indirect_table_count * (SIZE_T)indirect_max_desc;

            status = VirtIoSndAllocCommonBuffer(DmaCtx, indirect_bytes, FALSE, &qs->IndirectPool);
            if (NT_SUCCESS(status)) {
                if ((((ULONG_PTR)qs->IndirectPool.Va) & 0xFu) != 0 || (qs->IndirectPool.DmaAddr & 0xFu) != 0) {
                    VirtIoSndFreeCommonBuffer(DmaCtx, &qs->IndirectPool);
                    indirect_table_count = 0;
                    indirect_max_desc = 0;
                } else {
                    /*
                     * This DMA buffer is shared with the (potentially untrusted) device; clear it
                     * to avoid leaking stale kernel memory.
                     */
                    RtlZeroMemory(qs->IndirectPool.Va, indirect_bytes);
                }
            } else {
                indirect_table_count = 0;
                indirect_max_desc = 0;
            }
        }
    }

    status = VirtqSplitInit(
        qs->Vq,
        queue_size,
        event_idx,
        indirect,
        qs->Ring.Va,
        qs->Ring.DmaAddr,
        PAGE_SIZE,
        qs->IndirectPool.Va,
        qs->IndirectPool.DmaAddr,
        indirect_table_count,
        indirect_max_desc);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    out_queue->Ops = &g_VirtioSndQueueSplitOps;
    out_queue->Ctx = qs;

    *out_desc_pa = qs->Vq->desc_pa;
    *out_avail_pa = qs->Vq->avail_pa;
    *out_used_pa = qs->Vq->used_pa;

    return STATUS_SUCCESS;

Fail:
    VirtioSndQueueSplitDestroy(DmaCtx, qs);
    return status;
}

_Use_decl_annotations_
VOID
VirtioSndQueueSplitDestroy(PVIRTIOSND_DMA_CONTEXT DmaCtx, VIRTIOSND_QUEUE_SPLIT* qs)
{
    if (qs == NULL) {
        return;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        return;
    }

    VirtIoSndFreeCommonBuffer(DmaCtx, &qs->IndirectPool);
    VirtIoSndFreeCommonBuffer(DmaCtx, &qs->Ring);

    if (qs->Vq != NULL) {
        ExFreePoolWithTag(qs->Vq, VIRTIOSND_POOL_TAG);
        qs->Vq = NULL;
    }

    RtlZeroMemory(qs, sizeof(*qs));
}

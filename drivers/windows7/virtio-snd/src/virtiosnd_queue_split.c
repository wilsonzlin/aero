/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtio_pci_modern_wdm.h"
#include "virtiosnd_queue_split.h"

/*
 * For simplicity, place the used ring on a page boundary.
 *
 * Contract v1 only requires 16/2/4-byte alignment for desc/avail/used, but
 * PAGE_SIZE keeps the layout conservative and matches the original driver
 * contract guidance for split rings.
 */
#define VIRTIOSND_SPLIT_RING_ALIGN PAGE_SIZE

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
    volatile UINT16* addr;
    BOOLEAN should_kick;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || qs->Vq == NULL) {
        return;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);

    /*
     * Contract v1 uses "always notify" semantics (EVENT_IDX is not offered).
     *
     * Even if the device sets VIRTQ_USED_F_NO_NOTIFY, Aero drivers still notify
     * after publishing new available entries to keep behavior deterministic and
     * avoid relying on suppression bits that are out of scope for the contract.
     */
    should_kick = (qs->Vq->num_added != 0);
    if (qs->Vq->event_idx) {
        /* If EVENT_IDX is enabled, respect the standard virtio suppression logic. */
        should_kick = VirtqSplitKickPrepare(qs->Vq);
    }

    if (should_kick) {
        /*
         * Ensure all ring writes (including the avail->idx update performed by
         * VirtqSplitPublish) are globally visible before issuing the MMIO notify.
         */
        KeMemoryBarrier();

        addr = qs->NotifyAddr;
        if (addr == NULL && qs->NotifyBase != NULL && qs->NotifyOffMultiplier != 0) {
            ULONGLONG offset64;
            ULONG_PTR offset;

            offset64 = (ULONGLONG)qs->QueueNotifyOff * (ULONGLONG)qs->NotifyOffMultiplier;
            if (qs->NotifyLength != 0 && offset64 + sizeof(UINT16) <= (ULONGLONG)qs->NotifyLength) {
                offset = (ULONG_PTR)offset64;
                addr = (volatile UINT16*)(qs->NotifyBase + offset);
            }
        }
        if (addr != NULL) {
            WRITE_REGISTER_USHORT((volatile USHORT*)addr, qs->QueueIndex);
        }
    }

    /* Reset batching bookkeeping even if notification is suppressed. */
    VirtqSplitKickCommit(qs->Vq);

    VirtioSndQueueSplitUnlock(qs, &lock_state);
}

static VOID
VirtioSndQueueSplitDisableInterrupts(_In_ void* ctx)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || qs->Vq == NULL) {
        return;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);
    VirtqSplitDisableInterrupts(qs->Vq);
    VirtioSndQueueSplitUnlock(qs, &lock_state);
}

static BOOLEAN
VirtioSndQueueSplitEnableInterrupts(_In_ void* ctx)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    BOOLEAN ok;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || qs->Vq == NULL) {
        return FALSE;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);
    ok = VirtqSplitEnableInterrupts(qs->Vq);
    VirtioSndQueueSplitUnlock(qs, &lock_state);
    return ok;
}

static const VIRTIOSND_QUEUE_OPS g_VirtioSndQueueSplitOps = {
    VirtioSndQueueSplitSubmit,
    VirtioSndQueueSplitPopUsed,
    VirtioSndQueueSplitKick,
    VirtioSndQueueSplitDisableInterrupts,
    VirtioSndQueueSplitEnableInterrupts,
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
    SIZE_T notify_length,
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
    qs->QueueSize = queue_size;
    qs->NotifyBase = notify_base;
    qs->NotifyOffMultiplier = notify_off_multiplier;
    qs->NotifyLength = notify_length;
    qs->QueueNotifyOff = queue_notify_off;

    if (event_idx) {
        /* Aero contract v1 does not negotiate EVENT_IDX. */
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }
    if (!indirect) {
        /* Aero contract v1 requires INDIRECT_DESC. */
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }

    if (notify_base != NULL && notify_off_multiplier != 0) {
        ULONGLONG offset64;
        ULONG_PTR offset;

        offset64 = (ULONGLONG)queue_notify_off * (ULONGLONG)notify_off_multiplier;
        if (notify_length == 0 || offset64 + sizeof(UINT16) > (ULONGLONG)notify_length) {
            status = STATUS_DEVICE_CONFIGURATION_ERROR;
            goto Fail;
        }

        offset = (ULONG_PTR)offset64;
        qs->NotifyAddr = (volatile UINT16*)(notify_base + offset);
    } else {
        status = STATUS_INVALID_DEVICE_STATE;
        goto Fail;
    }

    ring_bytes = VirtqSplitRingMemSize(queue_size, VIRTIOSND_SPLIT_RING_ALIGN, event_idx);
    if (ring_bytes == 0) {
        status = STATUS_INVALID_PARAMETER;
        goto Fail;
    }

    status = VirtIoSndAllocCommonBuffer(DmaCtx, ring_bytes, FALSE, &qs->Ring);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    if ((((ULONG_PTR)qs->Ring.Va) & 0xFu) != 0 || (qs->Ring.DmaAddr & 0xFu) != 0) {
        status = STATUS_DATATYPE_MISALIGNMENT;
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
        SIZE_T indirect_bytes;

        indirect_table_count = (USHORT)VIRTIOSND_QUEUE_SPLIT_INDIRECT_TABLE_COUNT(queue_size);
        indirect_max_desc = (USHORT)VIRTIOSND_QUEUE_SPLIT_INDIRECT_MAX_DESC;
        indirect_bytes = sizeof(VIRTQ_DESC) * (SIZE_T)indirect_table_count * (SIZE_T)indirect_max_desc;

        status = VirtIoSndAllocCommonBuffer(DmaCtx, indirect_bytes, FALSE, &qs->IndirectPool);
        if (!NT_SUCCESS(status)) {
            goto Fail;
        }

        if ((((ULONG_PTR)qs->IndirectPool.Va) & 0xFu) != 0 || (qs->IndirectPool.DmaAddr & 0xFu) != 0) {
            status = STATUS_DATATYPE_MISALIGNMENT;
            goto Fail;
        }

        /*
         * This DMA buffer is shared with the (potentially untrusted) device; clear it
         * to avoid leaking stale kernel memory.
         */
        RtlZeroMemory(qs->IndirectPool.Va, indirect_bytes);
        qs->IndirectTableCount = indirect_table_count;
        qs->IndirectMaxDesc = indirect_max_desc;
    }

    status = VirtqSplitInit(
        qs->Vq,
        queue_size,
        event_idx,
        indirect,
        qs->Ring.Va,
        qs->Ring.DmaAddr,
        VIRTIOSND_SPLIT_RING_ALIGN,
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

_Use_decl_annotations_
VOID
VirtioSndQueueSplitDrainUsed(VIRTIOSND_QUEUE_SPLIT* qs,
                             EVT_VIRTIOSND_QUEUE_SPLIT_USED* Callback,
                             void* Context)
{
    typedef struct _USED_ENTRY {
        void* Cookie;
        UINT32 Len;
    } USED_ENTRY;

    USED_ENTRY used[VIRTIOSND_QUEUE_SIZE_TXQ];
    ULONG count;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;

    if (qs == NULL || qs->Vq == NULL || Callback == NULL) {
        return;
    }

    count = 0;

    VirtioSndQueueSplitLock(qs, &lock_state);

    for (;;) {
        void* cookie;
        UINT32 len;
        NTSTATUS status;

        cookie = NULL;
        len = 0;

        status = VirtqSplitGetUsed(qs->Vq, &cookie, &len);
        if (status == STATUS_NOT_FOUND) {
            break;
        }
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("queue[%u] VirtqSplitGetUsed failed: 0x%08X\n", (UINT)qs->QueueIndex, (UINT)status);
            break;
        }

        if (count >= RTL_NUMBER_OF(used)) {
            VIRTIOSND_TRACE_ERROR("queue[%u] used drain overflow\n", (UINT)qs->QueueIndex);
            break;
        }

        used[count].Cookie = cookie;
        used[count].Len = len;
        count++;
    }

    VirtioSndQueueSplitUnlock(qs, &lock_state);

    {
        ULONG i;
        for (i = 0; i < count; i++) {
            Callback(qs->QueueIndex, used[i].Cookie, used[i].Len, Context);
        }
    }
}

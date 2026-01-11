/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_queue_split.h"

/*
 * Split-ring alignment.
 *
 * virtio 1.0 only requires desc/avail/used addresses to satisfy 16/2/4-byte
 * alignment, but the shared virtqueue_split implementation also takes a
 * "queue_align" parameter (historically used by virtio-pci legacy). Use 16 to
 * keep the ring layout simple while satisfying all split-ring alignment rules.
 */
#define VIRTIOSND_SPLIT_RING_ALIGN 16u

typedef struct _VIRTIOSND_QUEUE_SPLIT_DMA_ALLOC {
    LIST_ENTRY Link;
    VIRTIOSND_DMA_BUFFER Buf;
} VIRTIOSND_QUEUE_SPLIT_DMA_ALLOC;
typedef VIRTIOSND_QUEUE_SPLIT_DMA_ALLOC *PVIRTIOSND_QUEUE_SPLIT_DMA_ALLOC;

static void *
VirtioSndVqAlloc(_In_ void *ctx, _In_ size_t size, _In_ virtio_os_alloc_flags_t flags)
{
    POOL_TYPE pool;
    void *ptr;

    UNREFERENCED_PARAMETER(ctx);

    if (size == 0) {
        return NULL;
    }

    pool = (flags & VIRTIO_OS_ALLOC_PAGED) ? PagedPool : NonPagedPool;
    ptr = ExAllocatePoolWithTag(pool, size, VIRTIOSND_POOL_TAG);
    if (ptr == NULL) {
        return NULL;
    }

    if (flags & VIRTIO_OS_ALLOC_ZERO) {
        RtlZeroMemory(ptr, size);
    }

    return ptr;
}

static void
VirtioSndVqFree(_In_ void *ctx, _In_opt_ void *ptr)
{
    UNREFERENCED_PARAMETER(ctx);

    if (ptr == NULL) {
        return;
    }

    ExFreePoolWithTag(ptr, VIRTIOSND_POOL_TAG);
}

static virtio_bool_t
VirtioSndVqAllocDma(_In_ void *ctx, _In_ size_t size, _In_ size_t alignment, _Out_ virtio_dma_buffer_t *out)
{
    NTSTATUS status;
    PVIRTIOSND_QUEUE_SPLIT_DMA_ALLOC alloc;
    struct _VIRTIOSND_QUEUE_SPLIT_OS_CTX *os;

    if (out == NULL) {
        return VIRTIO_FALSE;
    }
    RtlZeroMemory(out, sizeof(*out));

    os = (struct _VIRTIOSND_QUEUE_SPLIT_OS_CTX *)ctx;
    if (os == NULL || os->DmaCtx == NULL || size == 0) {
        return VIRTIO_FALSE;
    }

    alloc = (PVIRTIOSND_QUEUE_SPLIT_DMA_ALLOC)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), VIRTIOSND_POOL_TAG);
    if (alloc == NULL) {
        return VIRTIO_FALSE;
    }
    RtlZeroMemory(alloc, sizeof(*alloc));

    status = VirtIoSndAllocCommonBuffer(os->DmaCtx, size, FALSE, &alloc->Buf);
    if (!NT_SUCCESS(status)) {
        ExFreePoolWithTag(alloc, VIRTIOSND_POOL_TAG);
        return VIRTIO_FALSE;
    }

    if (alignment != 0 && ((alloc->Buf.DmaAddr & ((UINT64)alignment - 1u)) != 0)) {
        VirtIoSndFreeCommonBuffer(os->DmaCtx, &alloc->Buf);
        ExFreePoolWithTag(alloc, VIRTIOSND_POOL_TAG);
        return VIRTIO_FALSE;
    }

    /* This buffer is shared with the device; always clear it. */
    RtlZeroMemory(alloc->Buf.Va, size);

    InsertTailList(&os->DmaAllocs, &alloc->Link);

    out->vaddr = alloc->Buf.Va;
    out->paddr = alloc->Buf.DmaAddr;
    out->size = alloc->Buf.Size;
    return VIRTIO_TRUE;
}

static void
VirtioSndVqFreeDma(_In_ void *ctx, _Inout_ virtio_dma_buffer_t *buf)
{
    struct _VIRTIOSND_QUEUE_SPLIT_OS_CTX *os;
    LIST_ENTRY *entry;

    os = (struct _VIRTIOSND_QUEUE_SPLIT_OS_CTX *)ctx;
    if (os == NULL || os->DmaCtx == NULL || buf == NULL) {
        return;
    }

    if (buf->vaddr == NULL || buf->size == 0) {
        return;
    }

    entry = os->DmaAllocs.Flink;
    while (entry != &os->DmaAllocs) {
        PVIRTIOSND_QUEUE_SPLIT_DMA_ALLOC alloc;

        alloc = CONTAINING_RECORD(entry, VIRTIOSND_QUEUE_SPLIT_DMA_ALLOC, Link);
        entry = entry->Flink;

        if (alloc->Buf.Va != buf->vaddr) {
            continue;
        }

        RemoveEntryList(&alloc->Link);
        VirtIoSndFreeCommonBuffer(os->DmaCtx, &alloc->Buf);
        ExFreePoolWithTag(alloc, VIRTIOSND_POOL_TAG);
        RtlZeroMemory(buf, sizeof(*buf));
        return;
    }
}

static void
VirtioSndVqMemoryBarrier(_In_ void *ctx)
{
    UNREFERENCED_PARAMETER(ctx);
    KeMemoryBarrier();
}

static const virtio_os_ops_t g_VirtioSndQueueOsOps = {
    VirtioSndVqAlloc,
    VirtioSndVqFree,
    VirtioSndVqAllocDma,
    VirtioSndVqFreeDma,
    /*virt_to_phys=*/NULL,
    /*log=*/NULL,
    /*mb=*/VirtioSndVqMemoryBarrier,
    /*rmb=*/VirtioSndVqMemoryBarrier,
    /*wmb=*/VirtioSndVqMemoryBarrier,
    /*spinlock_create=*/NULL,
    /*spinlock_destroy=*/NULL,
    /*spinlock_acquire=*/NULL,
    /*spinlock_release=*/NULL,
    /*read_io8=*/NULL,
    /*read_io16=*/NULL,
    /*read_io32=*/NULL,
    /*write_io8=*/NULL,
    /*write_io16=*/NULL,
    /*write_io32=*/NULL,
};

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
    virtio_sg_entry_t vsg[VIRTIOSND_QUEUE_SPLIT_INDIRECT_MAX_DESC];
    USHORT i;
    uint16_t head;
    int rc;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || sg == NULL || sg_count == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (sg_count > (USHORT)RTL_NUMBER_OF(vsg)) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);

    for (i = 0; i < sg_count; i++) {
        vsg[i].addr = sg[i].addr;
        vsg[i].len = sg[i].len;
        vsg[i].device_writes = sg[i].write ? VIRTIO_TRUE : VIRTIO_FALSE;
    }

    head = 0;
    rc = virtqueue_split_add_sg(&qs->Vq, vsg, sg_count, cookie, /*use_indirect=*/VIRTIO_TRUE, &head);

    VirtioSndQueueSplitUnlock(qs, &lock_state);

    switch (rc) {
    case VIRTIO_OK:
        return STATUS_SUCCESS;
    case VIRTIO_ERR_NOSPC:
        return STATUS_INSUFFICIENT_RESOURCES;
    case VIRTIO_ERR_NOMEM:
        return STATUS_INSUFFICIENT_RESOURCES;
    case VIRTIO_ERR_RANGE:
        return STATUS_INVALID_PARAMETER;
    case VIRTIO_ERR_IO:
        return STATUS_IO_DEVICE_ERROR;
    case VIRTIO_ERR_INVAL:
    default:
        return STATUS_INVALID_PARAMETER;
    }
}

static BOOLEAN
VirtioSndQueueSplitPopUsed(_In_ void* ctx, _Out_ void** cookie_out, _Out_ UINT32* used_len_out)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    virtio_bool_t ok;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL || cookie_out == NULL || used_len_out == NULL) {
        return FALSE;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);

    *cookie_out = NULL;
    *used_len_out = 0;
    ok = virtqueue_split_pop_used(&qs->Vq, cookie_out, used_len_out);

    VirtioSndQueueSplitUnlock(qs, &lock_state);

    return ok ? TRUE : FALSE;
}

static VOID
VirtioSndQueueSplitKick(_In_ void* ctx)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    volatile UINT16* addr;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL) {
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
    if (qs->Vq.avail_idx != qs->Vq.last_kick_avail) {
        /*
         * Ensure all ring writes are globally visible before issuing the MMIO notify.
         */
        KeMemoryBarrier();

        addr = qs->NotifyAddr;
        if (addr != NULL) {
            WRITE_REGISTER_USHORT((volatile USHORT*)addr, qs->QueueIndex);
        }

        /* Keep batching state consistent even if NotifyAddr is NULL. */
        qs->Vq.last_kick_avail = qs->Vq.avail_idx;
    }

    VirtioSndQueueSplitUnlock(qs, &lock_state);
}

static VOID
VirtioSndQueueSplitDisableInterrupts(_In_ void* ctx)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL) {
        return;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);
    if (qs->Vq.avail != NULL) {
        qs->Vq.avail->flags |= VRING_AVAIL_F_NO_INTERRUPT;
        KeMemoryBarrier();
    }
    VirtioSndQueueSplitUnlock(qs, &lock_state);
}

static BOOLEAN
VirtioSndQueueSplitEnableInterrupts(_In_ void* ctx)
{
    VIRTIOSND_QUEUE_SPLIT* qs;
    VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;
    uint16_t used_idx;

    qs = (VIRTIOSND_QUEUE_SPLIT*)ctx;
    if (qs == NULL) {
        return FALSE;
    }

    VirtioSndQueueSplitLock(qs, &lock_state);
    if (qs->Vq.avail == NULL || qs->Vq.used == NULL) {
        VirtioSndQueueSplitUnlock(qs, &lock_state);
        return FALSE;
    }

    qs->Vq.avail->flags &= (uint16_t)~VRING_AVAIL_F_NO_INTERRUPT;

    /* Avoid missing an interrupt between enabling and checking used->idx. */
    KeMemoryBarrier();
    used_idx = qs->Vq.used->idx;
    VirtioSndQueueSplitUnlock(qs, &lock_state);
    return (used_idx == qs->Vq.last_used_idx) ? TRUE : FALSE;
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
    volatile UINT16* notify_addr,
    VIRTIOSND_QUEUE* out_queue,
    UINT64* out_desc_pa,
    UINT64* out_avail_pa,
    UINT64* out_used_pa)
{
    NTSTATUS status;
    int rc;
    ULONGLONG descPa, availPa, usedPa;
    ULONG_PTR availOff, usedOff;

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
    InitializeListHead(&qs->OsCtx.DmaAllocs);
    qs->OsCtx.DmaCtx = DmaCtx;

    qs->NotifyAddr = notify_addr;
    if (notify_addr == NULL) {
        status = STATUS_INVALID_DEVICE_STATE;
        goto Fail;
    }

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

    rc = virtqueue_split_alloc_ring(&g_VirtioSndQueueOsOps,
                                    &qs->OsCtx,
                                    queue_size,
                                    VIRTIOSND_SPLIT_RING_ALIGN,
                                    /*event_idx=*/VIRTIO_FALSE,
                                    &qs->Ring);
    if (rc != VIRTIO_OK) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Fail;
    }

    rc = virtqueue_split_init(&qs->Vq,
                              &g_VirtioSndQueueOsOps,
                              &qs->OsCtx,
                              queue_index,
                              queue_size,
                              VIRTIOSND_SPLIT_RING_ALIGN,
                              &qs->Ring,
                              /*event_idx=*/VIRTIO_FALSE,
                              /*indirect_desc=*/VIRTIO_TRUE,
                              (uint16_t)VIRTIOSND_QUEUE_SPLIT_INDIRECT_MAX_DESC);
    if (rc != VIRTIO_OK) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Fail;
    }

    out_queue->Ops = &g_VirtioSndQueueSplitOps;
    out_queue->Ctx = qs;

    descPa = (ULONGLONG)qs->Ring.paddr;

    availOff = (ULONG_PTR)((PUCHAR)qs->Vq.avail - (PUCHAR)qs->Ring.vaddr);
    usedOff = (ULONG_PTR)((PUCHAR)qs->Vq.used - (PUCHAR)qs->Ring.vaddr);

    availPa = descPa + (ULONGLONG)availOff;
    usedPa = descPa + (ULONGLONG)usedOff;

    *out_desc_pa = descPa;
    *out_avail_pa = availPa;
    *out_used_pa = usedPa;

    return STATUS_SUCCESS;

Fail:
    VirtioSndQueueSplitDestroy(DmaCtx, qs);
    return status;
}

_Use_decl_annotations_
VOID
VirtioSndQueueSplitDestroy(PVIRTIOSND_DMA_CONTEXT DmaCtx, VIRTIOSND_QUEUE_SPLIT* qs)
{
    struct _VIRTIOSND_QUEUE_SPLIT_OS_CTX *os;

    if (qs == NULL) {
        return;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        return;
    }

    virtqueue_split_destroy(&qs->Vq);

    os = &qs->OsCtx;
    if (os->DmaCtx == NULL) {
        os->DmaCtx = DmaCtx;
    }

    /* Allow safe teardown of a zero-initialized qs (e.g. StopHardware cleanup). */
    if (os->DmaAllocs.Flink == NULL || os->DmaAllocs.Blink == NULL) {
        InitializeListHead(&os->DmaAllocs);
    }

    virtqueue_split_free_ring(&g_VirtioSndQueueOsOps, os, &qs->Ring);

    /*
     * Defensive cleanup: in case virtqueue_split_init failed mid-way and left
     * some DMA allocations outstanding, free any remaining tracked buffers.
     */
    while (!IsListEmpty(&os->DmaAllocs)) {
        PVIRTIOSND_QUEUE_SPLIT_DMA_ALLOC alloc;
        LIST_ENTRY *entry;

        entry = RemoveHeadList(&os->DmaAllocs);
        alloc = CONTAINING_RECORD(entry, VIRTIOSND_QUEUE_SPLIT_DMA_ALLOC, Link);
        VirtIoSndFreeCommonBuffer(os->DmaCtx, &alloc->Buf);
        ExFreePoolWithTag(alloc, VIRTIOSND_POOL_TAG);
    }

    RtlZeroMemory(qs, sizeof(*qs));
}

_Use_decl_annotations_
VOID
VirtioSndQueueSplitDrainUsed(VIRTIOSND_QUEUE_SPLIT* qs,
                              EVT_VIRTIOSND_QUEUE_SPLIT_USED* Callback,
                              void* Context)
{
    if (qs == NULL || qs->Vq.desc == NULL || Callback == NULL) {
        return;
    }

    for (;;) {
        void* cookie;
        UINT32 len;
        virtio_bool_t ok;
        VIRTIOSND_QUEUE_SPLIT_LOCK_STATE lock_state;

        cookie = NULL;
        len = 0;

        VirtioSndQueueSplitLock(qs, &lock_state);
        ok = virtqueue_split_pop_used(&qs->Vq, &cookie, &len);
        VirtioSndQueueSplitUnlock(qs, &lock_state);

        if (ok == VIRTIO_FALSE) {
            break;
        }

        Callback(qs->QueueIndex, cookie, len, Context);
    }
}

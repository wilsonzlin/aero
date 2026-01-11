/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_rx.h"

static __forceinline ULONG VirtioSndRxHdrBytes(VOID) { return (ULONG)sizeof(VIRTIO_SND_TX_HDR); }
static __forceinline ULONG VirtioSndRxStatusBytes(VOID) { return (ULONG)sizeof(VIRTIO_SND_PCM_STATUS); }
static __forceinline ULONG VirtioSndRxFrameSizeBytes(VOID) { return 2u; }

typedef struct _VIRTIOSND_RX_COMPLETION_ENTRY {
    void* UserCookie;
    NTSTATUS CompletionStatus;
    ULONG VirtioStatus;
    ULONG LatencyBytes;
    ULONG PayloadBytes;
    UINT32 UsedLen;
} VIRTIOSND_RX_COMPLETION_ENTRY;

static VOID VirtIoSndRxFreeRequests(_Inout_ VIRTIOSND_RX_ENGINE* Rx)
{
    ULONG i;

    if (Rx->Requests == NULL) {
        return;
    }

    for (i = 0; i < Rx->RequestCount; ++i) {
        VirtIoSndFreeCommonBuffer(Rx->DmaCtx, &Rx->Requests[i].Allocation);
    }

    ExFreePoolWithTag(Rx->Requests, VIRTIOSND_POOL_TAG);
    Rx->Requests = NULL;
    Rx->RequestCount = 0;
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndRxInit(VIRTIOSND_RX_ENGINE* Rx, PVIRTIOSND_DMA_CONTEXT DmaCtx, const VIRTIOSND_QUEUE* Queue, ULONG RequestCount)
{
    NTSTATUS status;
    ULONG i;
    ULONG totalBytes;
    PUCHAR baseVa;
    VIRTIO_SND_TX_HDR* hdr;

    NT_ASSERT(Rx != NULL);
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Rx == NULL || Queue == NULL || Queue->Ops == NULL || Queue->Ctx == NULL || Queue->Ops->Submit == NULL || Queue->Ops->PopUsed == NULL ||
        Queue->Ops->Kick == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (DmaCtx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (RequestCount == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Rx, sizeof(*Rx));

    KeInitializeSpinLock(&Rx->Lock);
    InitializeListHead(&Rx->FreeList);
    InitializeListHead(&Rx->InflightList);

    Rx->Queue = Queue;
    Rx->DmaCtx = DmaCtx;
    Rx->NextSequence = 1;

    Rx->Requests = (VIRTIOSND_RX_REQUEST*)ExAllocatePoolWithTag(NonPagedPool, sizeof(VIRTIOSND_RX_REQUEST) * RequestCount, VIRTIOSND_POOL_TAG);
    if (Rx->Requests == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(Rx->Requests, sizeof(VIRTIOSND_RX_REQUEST) * RequestCount);
    Rx->RequestCount = RequestCount;

    totalBytes = VirtioSndRxHdrBytes() + VirtioSndRxStatusBytes();
    if (totalBytes < VirtioSndRxHdrBytes()) {
        VirtIoSndRxFreeRequests(Rx);
        return STATUS_INVALID_PARAMETER;
    }

    for (i = 0; i < RequestCount; ++i) {
        status = VirtIoSndAllocCommonBuffer(Rx->DmaCtx, totalBytes, FALSE, &Rx->Requests[i].Allocation);
        if (!NT_SUCCESS(status)) {
            goto Fail;
        }

        baseVa = (PUCHAR)Rx->Requests[i].Allocation.Va;
        RtlZeroMemory(baseVa, totalBytes);

        Rx->Requests[i].HdrVa = (VIRTIO_SND_TX_HDR*)baseVa;
        Rx->Requests[i].HdrDma = Rx->Requests[i].Allocation.DmaAddr;

        Rx->Requests[i].StatusVa = (VIRTIO_SND_PCM_STATUS*)(baseVa + VirtioSndRxHdrBytes());
        Rx->Requests[i].StatusDma = Rx->Requests[i].Allocation.DmaAddr + VirtioSndRxHdrBytes();

        Rx->Requests[i].PayloadBytes = 0;
        Rx->Requests[i].Sequence = 0;
        Rx->Requests[i].Cookie = NULL;
        Rx->Requests[i].Inflight = FALSE;

        hdr = Rx->Requests[i].HdrVa;
        hdr->stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
        hdr->reserved = 0;

        InsertTailList(&Rx->FreeList, &Rx->Requests[i].Link);
        Rx->FreeCount++;
    }

    return STATUS_SUCCESS;

Fail:
    VirtIoSndRxFreeRequests(Rx);
    return status;
}

_Use_decl_annotations_
VOID
VirtIoSndRxUninit(VIRTIOSND_RX_ENGINE* Rx)
{
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    if (Rx == NULL) {
        return;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }

    VirtIoSndRxFreeRequests(Rx);

    RtlZeroMemory(Rx, sizeof(*Rx));
}

_Use_decl_annotations_
VOID
VirtIoSndRxSetCompletionCallback(VIRTIOSND_RX_ENGINE* Rx, EVT_VIRTIOSND_RX_COMPLETION* Callback, void* Context)
{
    KIRQL oldIrql;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Rx == NULL) {
        return;
    }

    KeAcquireSpinLock(&Rx->Lock, &oldIrql);
    Rx->CompletionCallback = Callback;
    Rx->CompletionCallbackContext = Context;
    KeReleaseSpinLock(&Rx->Lock, oldIrql);
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndRxSubmitSg(VIRTIOSND_RX_ENGINE* Rx, const VIRTIOSND_RX_SEGMENT* Segments, USHORT SegmentCount, void* Cookie)
{
    KIRQL oldIrql;
    LIST_ENTRY* entry;
    VIRTIOSND_RX_REQUEST* req;
    NTSTATUS status;
    VIRTIOSND_SG sg[VIRTIOSND_RX_MAX_PAYLOAD_SG + 2];
    USHORT sgCount;
    ULONG payloadBytes;
    USHORT i;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Rx == NULL || Rx->Queue == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Segments == NULL || SegmentCount == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    if (SegmentCount > VIRTIOSND_RX_MAX_PAYLOAD_SG) {
        return STATUS_INVALID_PARAMETER;
    }

    payloadBytes = 0;
    for (i = 0; i < SegmentCount; i++) {
        ULONG len = (ULONG)Segments[i].len;
        if (len == 0) {
            return STATUS_INVALID_PARAMETER;
        }
        if (payloadBytes + len < payloadBytes) {
            return STATUS_INTEGER_OVERFLOW;
        }
        payloadBytes += len;
    }

    if ((payloadBytes % VirtioSndRxFrameSizeBytes()) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    KeAcquireSpinLock(&Rx->Lock, &oldIrql);

    if (Rx->FatalError) {
        KeReleaseSpinLock(&Rx->Lock, oldIrql);
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Rx->FreeCount == 0 || IsListEmpty(&Rx->FreeList)) {
        Rx->DroppedDueToNoRequests++;
        KeReleaseSpinLock(&Rx->Lock, oldIrql);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    entry = RemoveHeadList(&Rx->FreeList);
    Rx->FreeCount--;
    req = CONTAINING_RECORD(entry, VIRTIOSND_RX_REQUEST, Link);
    KeReleaseSpinLock(&Rx->Lock, oldIrql);

    req->PayloadBytes = payloadBytes;
    req->Cookie = Cookie;

    RtlZeroMemory(req->StatusVa, sizeof(*req->StatusVa));

    sg[0].addr = req->HdrDma;
    sg[0].len = (UINT32)VirtioSndRxHdrBytes();
    sg[0].write = FALSE;

    for (i = 0; i < SegmentCount; i++) {
        sg[1 + i].addr = Segments[i].addr;
        sg[1 + i].len = Segments[i].len;
        sg[1 + i].write = TRUE;
    }

    sg[1 + SegmentCount].addr = req->StatusDma;
    sg[1 + SegmentCount].len = (UINT32)VirtioSndRxStatusBytes();
    sg[1 + SegmentCount].write = TRUE;

    sgCount = (USHORT)(SegmentCount + 2);

    KeAcquireSpinLock(&Rx->Lock, &oldIrql);

    req->Sequence = Rx->NextSequence++;

    /* Ensure the header/status writes are visible before publishing descriptors. */
    KeMemoryBarrier();

    status = VirtioSndQueueSubmit(Rx->Queue, sg, sgCount, req);

    if (!NT_SUCCESS(status)) {
        InsertTailList(&Rx->FreeList, &req->Link);
        Rx->FreeCount++;
        KeReleaseSpinLock(&Rx->Lock, oldIrql);
        return status;
    }

    InsertTailList(&Rx->InflightList, &req->Link);
    Rx->InflightCount++;
    req->Inflight = TRUE;
    Rx->SubmittedBuffers++;

    KeReleaseSpinLock(&Rx->Lock, oldIrql);

    VirtioSndQueueKick(Rx->Queue);

    return STATUS_SUCCESS;
}

static VOID VirtIoSndRxHandleUsedLocked(
    _Inout_ VIRTIOSND_RX_ENGINE* Rx,
    _Inout_ VIRTIOSND_RX_REQUEST* Req,
    _In_ UINT32 UsedLen,
    _Out_ VIRTIOSND_RX_COMPLETION_ENTRY* Completion);

_Use_decl_annotations_
ULONG
VirtIoSndRxDrainCompletions(VIRTIOSND_RX_ENGINE* Rx, EVT_VIRTIOSND_RX_COMPLETION* Callback, void* Context)
{
    KIRQL oldIrql;
    ULONG drained;
    VOID* ctx;
    UINT32 usedLen;
    EVT_VIRTIOSND_RX_COMPLETION* cb;
    void* cbCtx;
    VIRTIOSND_RX_COMPLETION_ENTRY completions[VIRTIOSND_QUEUE_SIZE_RXQ];
    ULONG completionCount;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Rx == NULL || Rx->Queue == NULL) {
        return 0;
    }

    drained = 0;
    completionCount = 0;

    KeAcquireSpinLock(&Rx->Lock, &oldIrql);

    if (Callback != NULL) {
        cb = Callback;
        cbCtx = Context;
    } else {
        cb = Rx->CompletionCallback;
        cbCtx = Rx->CompletionCallbackContext;
    }

    while (VirtioSndQueuePopUsed(Rx->Queue, &ctx, &usedLen)) {
        if (ctx != NULL && completionCount < RTL_NUMBER_OF(completions)) {
            VirtIoSndRxHandleUsedLocked(Rx, (VIRTIOSND_RX_REQUEST*)ctx, usedLen, &completions[completionCount]);
            completionCount++;
        }
        drained++;
    }

    KeReleaseSpinLock(&Rx->Lock, oldIrql);

    if (cb != NULL) {
        ULONG i;
        for (i = 0; i < completionCount; i++) {
            cb(completions[i].UserCookie,
               completions[i].CompletionStatus,
               completions[i].VirtioStatus,
               completions[i].LatencyBytes,
               completions[i].PayloadBytes,
               completions[i].UsedLen,
               cbCtx);
        }
    }

    return drained;
}

static VOID VirtIoSndRxReturnToFreeListLocked(_Inout_ VIRTIOSND_RX_ENGINE* Rx, _Inout_ VIRTIOSND_RX_REQUEST* Req)
{
    if (Req->Inflight) {
        RemoveEntryList(&Req->Link);
        Rx->InflightCount--;
        Req->Inflight = FALSE;
    }

    InsertTailList(&Rx->FreeList, &Req->Link);
    Rx->FreeCount++;
}

static VOID VirtIoSndRxHandleUsedLocked(
    _Inout_ VIRTIOSND_RX_ENGINE* Rx,
    _Inout_ VIRTIOSND_RX_REQUEST* Req,
    _In_ UINT32 UsedLen,
    _Out_ VIRTIOSND_RX_COMPLETION_ENTRY* Completion)
{
    ULONG st;
    ULONG latency;
    void* userCookie;
    ULONG payloadBytesRequested;
    ULONG payloadBytesCaptured;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Rx == NULL || Req == NULL || Completion == NULL) {
        return;
    }

    /*
     * Ensure device writes are visible before reading response bytes.
     *
     * Note: VirtqSplitGetUsed issues a VIRTIO_RMB after observing used->idx, so
     * this is largely redundant, but keeps the RX path consistent with TX and
     * protects against alternate queue implementations.
     */
    KeMemoryBarrier();

    st = VIRTIO_SND_S_BAD_MSG;
    latency = 0;
    payloadBytesCaptured = 0;
    if (UsedLen >= VirtioSndRxStatusBytes() && Req->StatusVa != NULL) {
        st = Req->StatusVa->status;
        latency = Req->StatusVa->latency_bytes;
        payloadBytesCaptured = (ULONG)(UsedLen - VirtioSndRxStatusBytes());
    }

    Rx->LastVirtioStatus = st;
    Rx->LastLatencyBytes = latency;

    Rx->CompletedBuffers++;

    if (st <= VIRTIO_SND_S_IO_ERR) {
        Rx->CompletedByStatus[st]++;
        if (st == VIRTIO_SND_S_BAD_MSG || st == VIRTIO_SND_S_NOT_SUPP) {
            Rx->FatalError = TRUE;
        }
    } else {
        Rx->CompletedUnknownStatus++;
    }

    userCookie = Req->Cookie;
    payloadBytesRequested = Req->PayloadBytes;
    if (payloadBytesCaptured > payloadBytesRequested) {
        payloadBytesCaptured = payloadBytesRequested;
    }

    Req->Cookie = NULL;
    Req->PayloadBytes = 0;
    Req->Sequence = 0;

    VirtIoSndRxReturnToFreeListLocked(Rx, Req);

    Completion->UserCookie = userCookie;
    Completion->CompletionStatus = VirtioSndStatusToNtStatus(st);
    Completion->VirtioStatus = st;
    Completion->LatencyBytes = latency;
    Completion->PayloadBytes = payloadBytesCaptured;
    Completion->UsedLen = UsedLen;
}

_Use_decl_annotations_
VOID
VirtIoSndRxOnUsed(VIRTIOSND_RX_ENGINE* Rx, void* Cookie, UINT32 UsedLen)
{
    KIRQL oldIrql;
    VIRTIOSND_RX_REQUEST* req;
    EVT_VIRTIOSND_RX_COMPLETION* cb;
    void* cbCtx;
    VIRTIOSND_RX_COMPLETION_ENTRY completion;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Rx == NULL || Cookie == NULL) {
        return;
    }
    if (Rx->Queue == NULL) {
        return;
    }

    req = (VIRTIOSND_RX_REQUEST*)Cookie;

    KeAcquireSpinLock(&Rx->Lock, &oldIrql);

    cb = Rx->CompletionCallback;
    cbCtx = Rx->CompletionCallbackContext;

    VirtIoSndRxHandleUsedLocked(Rx, req, UsedLen, &completion);

    KeReleaseSpinLock(&Rx->Lock, oldIrql);

    if (cb != NULL) {
        cb(completion.UserCookie,
           completion.CompletionStatus,
           completion.VirtioStatus,
           completion.LatencyBytes,
           completion.PayloadBytes,
           completion.UsedLen,
           cbCtx);
    }
}

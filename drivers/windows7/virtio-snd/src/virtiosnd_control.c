/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd_control.h"
#include "virtiosnd_control_proto.h"

#define VIRTIOSND_CTRL_REQ_TAG 'rCSV' /* 'VSCr' */
#define VIRTIOSND_CTRL_TIMEOUT_DEFAULT_MS 1000u

/*
 * WinDDK 7600 headers predate ALIGN_UP_BY(). Provide a local fallback to keep
 * the virtio-snd driver building against both WinDDK 7600 and newer WDKs.
 */
#ifndef ALIGN_UP_BY
#define ALIGN_UP_BY(_length, _alignment) (((_length) + ((_alignment) - 1)) & ~((_alignment) - 1))
#endif

/*
 * Per-request context + DMA buffers.
 *
 * A control request is submitted as a 2-descriptor chain:
 *  - request header (device-readable)
 *  - response/status buffer (device-writable)
 *
 * We allocate the entire request context as a single physically-contiguous DMA
 * buffer so the virtqueue SG list can be built using a simple base+offset
 * translation (no MmGetPhysicalAddress-based per-page splitting).
 *
 * Lifetime:
 *  - One reference is owned by the sending thread.
 *  - One reference is owned by the virtqueue cookie and released on completion.
 *
 * STOP_DEVICE must cancel/drain all active requests before releasing the DMA
 * adapter so common buffers can be freed safely.
 */
typedef struct _VIRTIOSND_CTRL_REQUEST {
    LIST_ENTRY Link;
    LIST_ENTRY InflightLink;
    VIRTIOSND_CONTROL* Owner;
    volatile NTSTATUS CompletionStatus;

    /*
     * Common buffers are allocated at PASSIVE_LEVEL. Some WDKs do not document
     * FreeCommonBuffer as DISPATCH_LEVEL-safe, and control requests can time out
     * (send thread drops its ref, then the completion path runs later in a DPC).
     *
     * To keep teardown deterministic, we free request DMA buffers at PASSIVE_LEVEL
     * by queuing a work item if the last reference is dropped at DISPATCH_LEVEL.
     */
    WORK_QUEUE_ITEM FreeWorkItem;

    VIRTIOSND_DMA_BUFFER DmaBuf;
    ULONG ReqOffset;
    ULONG RespOffset;

    /* 0 = in-flight (queue ref not released), 1 = completed/canceled. */
    volatile LONG Completed;

    LONG RefCount;
    KEVENT Event;

    ULONG Code;

    PUCHAR ReqBuf;
    ULONG ReqLen;

    PUCHAR RespBuf;
    ULONG RespCap;

    volatile ULONG UsedLen;
    volatile ULONG VirtioStatus;
} VIRTIOSND_CTRL_REQUEST;

static VOID VirtioSndCtrlRequestDestroy(_In_ VIRTIOSND_CTRL_REQUEST* Req)
{
    VIRTIOSND_CONTROL* ctrl;
    KIRQL oldIrql;
    BOOLEAN maybeEmpty;

    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    ctrl = Req->Owner;
    maybeEmpty = FALSE;
    if (ctrl != NULL) {
        KeAcquireSpinLock(&ctrl->ReqLock, &oldIrql);
        RemoveEntryList(&Req->Link);
        maybeEmpty = IsListEmpty(&ctrl->ReqList);
        KeReleaseSpinLock(&ctrl->ReqLock, oldIrql);
    }

    VirtIoSndFreeCommonBuffer(ctrl ? ctrl->DmaCtx : NULL, &Req->DmaBuf);

    /*
     * ReqIdleEvent is used by STOP/REMOVE teardown to wait until all request DMA
     * buffers have been freed (while the DMA adapter is still valid). Signal the
     * event only after freeing the common buffer to avoid races where the wait
     * returns while a request still needs to call FreeCommonBuffer().
     */
    if (ctrl != NULL && maybeEmpty) {
        KeAcquireSpinLock(&ctrl->ReqLock, &oldIrql);
        if (IsListEmpty(&ctrl->ReqList)) {
            KeSetEvent(&ctrl->ReqIdleEvent, IO_NO_INCREMENT, FALSE);
        }
        KeReleaseSpinLock(&ctrl->ReqLock, oldIrql);
    }
}

static VOID VirtioSndCtrlRequestFreeWorkItem(_In_ PVOID Context)
{
    VirtioSndCtrlRequestDestroy((VIRTIOSND_CTRL_REQUEST*)Context);
}

static __forceinline VOID
VirtioSndCtrlRequestRelease(_In_ VIRTIOSND_CTRL_REQUEST* Req)
{
    if (InterlockedDecrement(&Req->RefCount) == 0) {
        if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
            VirtioSndCtrlRequestDestroy(Req);
        } else {
            ExInitializeWorkItem(&Req->FreeWorkItem, VirtioSndCtrlRequestFreeWorkItem, Req);
            ExQueueWorkItem(&Req->FreeWorkItem, DelayedWorkQueue);
        }
    }
}

static __forceinline BOOLEAN
VirtioSndCtrlRequestTryAddRef(_Inout_ VIRTIOSND_CTRL_REQUEST* Req)
{
    LONG old;
    LONG newValue;

    for (;;) {
        old = Req->RefCount;
        if (old == 0) {
            return FALSE;
        }
        newValue = old + 1;
        if (InterlockedCompareExchange(&Req->RefCount, newValue, old) == old) {
            return TRUE;
        }
    }
}

static VOID
VirtioSndCtrlCompleteRequest(_Inout_ VIRTIOSND_CTRL_REQUEST* Req, _In_ ULONG UsedLen)
{
    KIRQL oldIrql;
    ULONG virtioStatus;

    if (InterlockedCompareExchange(&Req->Completed, 1, 0) != 0) {
        return;
    }

    if (Req->Owner != NULL) {
        InterlockedIncrement(&Req->Owner->Stats.RequestsCompleted);
    }

    Req->UsedLen = UsedLen;

    /*
     * Ensure device writes are visible before reading response bytes.
     *
     * This matches the TX/RX completion handling (used-entry handlers) and
     * protects against stale reads on alternate virtqueue implementations.
     */
    KeMemoryBarrier();

    virtioStatus = 0xFFFFFFFFu;
    if (UsedLen >= sizeof(ULONG) && Req->RespBuf != NULL) {
        virtioStatus = *(UNALIGNED const ULONG*)Req->RespBuf;
    }
    Req->VirtioStatus = virtioStatus;

    Req->CompletionStatus = STATUS_SUCCESS;

    /* Remove from the control engine's inflight list (best-effort). */
    if (Req->Owner != NULL) {
        KeAcquireSpinLock(&Req->Owner->InflightLock, &oldIrql);
        if (!IsListEmpty(&Req->InflightLink)) {
            RemoveEntryList(&Req->InflightLink);
            InitializeListHead(&Req->InflightLink);
        }
        KeReleaseSpinLock(&Req->Owner->InflightLock, oldIrql);
    }

    VIRTIOSND_TRACE(
        "ctrlq complete code=0x%08lx status=0x%08lx(%s) len=%lu\n",
        Req->Code,
        virtioStatus,
        VirtioSndStatusToString(virtioStatus),
        UsedLen);

    KeMemoryBarrier();
    KeSetEvent(&Req->Event, IO_NO_INCREMENT, FALSE);

    /* Drop the queue-owned reference. */
    VirtioSndCtrlRequestRelease(Req);
}

static VOID
VirtioSndCtrlCancelRequest(_Inout_ VIRTIOSND_CTRL_REQUEST* Req)
{
    KIRQL oldIrql;

    if (InterlockedCompareExchange(&Req->Completed, 1, 0) != 0) {
        return;
    }

    Req->CompletionStatus = STATUS_CANCELLED;
    Req->UsedLen = sizeof(ULONG);
    Req->VirtioStatus = VIRTIO_SND_S_IO_ERR;

    if (Req->RespBuf != NULL && Req->RespCap >= sizeof(ULONG)) {
        *(UNALIGNED ULONG*)Req->RespBuf = VIRTIO_SND_S_IO_ERR;
    }

    /* Remove from the control engine's inflight list (best-effort). */
    if (Req->Owner != NULL) {
        KeAcquireSpinLock(&Req->Owner->InflightLock, &oldIrql);
        if (!IsListEmpty(&Req->InflightLink)) {
            RemoveEntryList(&Req->InflightLink);
            InitializeListHead(&Req->InflightLink);
        }
        KeReleaseSpinLock(&Req->Owner->InflightLock, oldIrql);
    }

    KeMemoryBarrier();
    KeSetEvent(&Req->Event, IO_NO_INCREMENT, FALSE);

    /* Drop the queue-owned reference. */
    VirtioSndCtrlRequestRelease(Req);
}

VOID
VirtioSndCtrlInit(_Out_ VIRTIOSND_CONTROL* Ctrl, _In_ PVIRTIOSND_DMA_CONTEXT DmaCtx, _In_ VIRTIOSND_QUEUE* ControlQ)
{
    RtlZeroMemory(Ctrl, sizeof(*Ctrl));
    Ctrl->DmaCtx = DmaCtx;
    Ctrl->ControlQ = ControlQ;

    KeInitializeSpinLock(&Ctrl->InflightLock);
    InitializeListHead(&Ctrl->InflightList);

    ExInitializeFastMutex(&Ctrl->Mutex);

    KeInitializeSpinLock(&Ctrl->ReqLock);
    InitializeListHead(&Ctrl->ReqList);
    KeInitializeEvent(&Ctrl->ReqIdleEvent, NotificationEvent, TRUE);
    Ctrl->Stopping = 0;

    Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] = VirtioSndStreamStateIdle;
    Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] = VirtioSndStreamStateIdle;
    RtlZeroMemory(&Ctrl->Params, sizeof(Ctrl->Params));
}

_Use_decl_annotations_
VOID
VirtioSndCtrlUninit(VIRTIOSND_CONTROL* Ctrl)
{
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    if (Ctrl == NULL) {
        return;
    }

    InterlockedExchange(&Ctrl->Stopping, 1);

    /*
     * Device is expected to be reset/stopped by the caller before uninit so no
     * further DMA is in flight. Drain any pending used entries, then complete
     * and cancel any remaining requests.
     */
    VirtioSndCtrlProcessUsed(Ctrl);

    for (;;) {
        PLIST_ENTRY entry;
        VIRTIOSND_CTRL_REQUEST* req;
        KIRQL oldIrql;

        req = NULL;

        KeAcquireSpinLock(&Ctrl->ReqLock, &oldIrql);
        for (entry = Ctrl->ReqList.Flink; entry != &Ctrl->ReqList; entry = entry->Flink) {
            VIRTIOSND_CTRL_REQUEST* candidate = CONTAINING_RECORD(entry, VIRTIOSND_CTRL_REQUEST, Link);
            if (InterlockedCompareExchange(&candidate->Completed, 0, 0) == 0) {
                if (VirtioSndCtrlRequestTryAddRef(candidate)) {
                    req = candidate;
                    break;
                }
            }
        }
        KeReleaseSpinLock(&Ctrl->ReqLock, oldIrql);

        if (req == NULL) {
            break;
        }

        VirtioSndCtrlCancelRequest(req);
        VirtioSndCtrlRequestRelease(req);
    }

    (VOID)KeWaitForSingleObject(&Ctrl->ReqIdleEvent, Executive, KernelMode, FALSE, NULL);

    Ctrl->DmaCtx = NULL;
    Ctrl->ControlQ = NULL;
    Ctrl->Stopping = 0;
    Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] = VirtioSndStreamStateIdle;
    Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] = VirtioSndStreamStateIdle;
    RtlZeroMemory(&Ctrl->Params, sizeof(Ctrl->Params));
}

VOID
VirtioSndCtrlCancelAll(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ NTSTATUS CancelStatus)
{
    KIRQL oldIrql;

    if (Ctrl == NULL) {
        return;
    }

    /*
     * Drain any already-completed used entries before canceling in-flight requests.
     *
     * If a request times out (send thread drops its ref) and then completes later,
     * the queue-owned reference might be the last one keeping the request context
     * alive. Cancelling and releasing that reference while a cookie is still
     * present in the used ring can lead to a stale cookie / use-after-free when
     * the used entry is processed later.
     */
    VirtioSndCtrlProcessUsed(Ctrl);

    KeAcquireSpinLock(&Ctrl->InflightLock, &oldIrql);
    while (!IsListEmpty(&Ctrl->InflightList)) {
        LIST_ENTRY* entry;
        VIRTIOSND_CTRL_REQUEST* req;

        entry = RemoveHeadList(&Ctrl->InflightList);
        req = CONTAINING_RECORD(entry, VIRTIOSND_CTRL_REQUEST, InflightLink);
        InitializeListHead(&req->InflightLink);

        if (InterlockedCompareExchange(&req->Completed, 1, 0) != 0) {
            /* Already completed/canceled; keep existing completion status. */
            KeMemoryBarrier();
            KeSetEvent(&req->Event, IO_NO_INCREMENT, FALSE);
            continue;
        }

        req->CompletionStatus = CancelStatus;

        KeMemoryBarrier();
        KeSetEvent(&req->Event, IO_NO_INCREMENT, FALSE);

        /* Drop the queue-owned reference; no completion will arrive after reset. */
        VirtioSndCtrlRequestRelease(req);
    }
    KeReleaseSpinLock(&Ctrl->InflightLock, oldIrql);
}

VOID
VirtioSndCtrlProcessUsed(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    void* cookie;
    UINT32 usedLen;

    if (Ctrl == NULL || Ctrl->ControlQ == NULL || Ctrl->ControlQ->Ops == NULL || Ctrl->ControlQ->Ops->PopUsed == NULL) {
        return;
    }

    for (;;) {
        cookie = NULL;
        usedLen = 0;

        if (!VirtioSndQueuePopUsed(Ctrl->ControlQ, &cookie, &usedLen)) {
            break;
        }

        if (cookie != NULL) {
            VirtioSndCtrlCompleteRequest((VIRTIOSND_CTRL_REQUEST*)cookie, (ULONG)usedLen);
        }
    }
}

VOID
VirtioSndCtrlOnUsed(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_opt_ void* Cookie, _In_ UINT32 UsedLen)
{
    UNREFERENCED_PARAMETER(Ctrl);

    if (Cookie == NULL) {
        return;
    }

    VirtioSndCtrlCompleteRequest((VIRTIOSND_CTRL_REQUEST*)Cookie, (ULONG)UsedLen);
}

static NTSTATUS
VirtioSndCtrlSendSyncLocked(
    _Inout_ VIRTIOSND_CONTROL* Ctrl,
    _In_reads_bytes_(ReqLen) const void* Req,
    _In_ ULONG ReqLen,
    _Out_writes_bytes_(RespCap) void* Resp,
    _In_ ULONG RespCap,
    _In_ ULONG TimeoutMs,
    _Out_opt_ ULONG* OutVirtioStatus,
    _Out_opt_ ULONG* OutRespLen)
{
    NTSTATUS status;
    NTSTATUS waitStatus;
    SIZE_T allocSize;
    ULONG reqOffset;
    ULONG respOffset;
    VIRTIOSND_CTRL_REQUEST* ctx;
    VIRTIOSND_DMA_BUFFER dmaBuf;
    VIRTIOSND_SG sg[16];
    USHORT sgCount;
    LARGE_INTEGER timeout;
    ULONGLONG deadline100ns;
    ULONGLONG now100ns;
    ULONG usedLen;
    ULONG virtioStatus;
    ULONG copyLen;
    KIRQL oldIrql;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (OutVirtioStatus != NULL) {
        *OutVirtioStatus = 0;
    }
    if (OutRespLen != NULL) {
        *OutRespLen = 0;
    }

    if (Ctrl == NULL || Req == NULL || Resp == NULL || ReqLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    if (RespCap < sizeof(ULONG)) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    if (Ctrl->ControlQ == NULL || Ctrl->ControlQ->Ops == NULL || Ctrl->ControlQ->Ops->Submit == NULL ||
        Ctrl->ControlQ->Ops->Kick == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (Ctrl->DmaCtx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (InterlockedCompareExchange(&Ctrl->Stopping, 0, 0) != 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    reqOffset = ALIGN_UP_BY(sizeof(*ctx), sizeof(ULONG));
    respOffset = ALIGN_UP_BY(reqOffset + ReqLen, sizeof(ULONG));

    allocSize = (SIZE_T)respOffset + (SIZE_T)RespCap;
    if (allocSize < respOffset) {
        return STATUS_INTEGER_OVERFLOW;
    }

    status = VirtIoSndAllocCommonBuffer(Ctrl->DmaCtx, allocSize, FALSE, &dmaBuf);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlZeroMemory(dmaBuf.Va, allocSize);
    ctx = (VIRTIOSND_CTRL_REQUEST*)dmaBuf.Va;
    ctx->Owner = Ctrl;
    ctx->DmaBuf = dmaBuf;
    ctx->ReqOffset = reqOffset;
    ctx->RespOffset = respOffset;
    ctx->Completed = 0;

    /*
     * Hold both references up-front to avoid a race where the device completes
     * immediately after submission and the completion path runs before the
     * sending thread can take an extra reference.
     */
    ctx->RefCount = 2;
    KeInitializeEvent(&ctx->Event, NotificationEvent, FALSE);
    InitializeListHead(&ctx->InflightLink);
    ctx->CompletionStatus = STATUS_PENDING;

    KeAcquireSpinLock(&Ctrl->ReqLock, &oldIrql);
    if (IsListEmpty(&Ctrl->ReqList)) {
        KeClearEvent(&Ctrl->ReqIdleEvent);
    }
    InsertTailList(&Ctrl->ReqList, &ctx->Link);
    KeReleaseSpinLock(&Ctrl->ReqLock, oldIrql);

    ctx->Code = (ReqLen >= sizeof(ULONG)) ? *(UNALIGNED const ULONG*)Req : 0;
    ctx->ReqBuf = ((PUCHAR)ctx) + reqOffset;
    ctx->ReqLen = ReqLen;
    ctx->RespBuf = ((PUCHAR)ctx) + respOffset;
    ctx->RespCap = RespCap;
    ctx->UsedLen = 0;
    ctx->VirtioStatus = 0xFFFFFFFFu;

    RtlCopyMemory(ctx->ReqBuf, Req, ReqLen);
    RtlZeroMemory(ctx->RespBuf, RespCap);

    /* Ensure request/response buffer writes are visible before publishing descriptors. */
    KeMemoryBarrier();

    sg[0].addr = ctx->DmaBuf.DmaAddr + (UINT64)reqOffset;
    sg[0].len = (UINT32)ReqLen;
    sg[0].write = FALSE;

    sg[1].addr = ctx->DmaBuf.DmaAddr + (UINT64)respOffset;
    sg[1].len = (UINT32)RespCap;
    sg[1].write = TRUE;

    sgCount = 2;

    VIRTIOSND_TRACE("ctrlq send code=0x%08lx req_len=%lu resp_cap=%lu\n", ctx->Code, ReqLen, RespCap);

    /* Track in-flight requests so STOP/REMOVE can cancel waiters. */
    {
        KIRQL oldIrql;
        KeAcquireSpinLock(&Ctrl->InflightLock, &oldIrql);
        InsertTailList(&Ctrl->InflightList, &ctx->InflightLink);
        KeReleaseSpinLock(&Ctrl->InflightLock, oldIrql);
    }

    status = VirtioSndQueueSubmit(Ctrl->ControlQ, sg, sgCount, ctx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("ctrlq Submit failed: 0x%08X\n", (UINT)status);

        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&Ctrl->InflightLock, &oldIrql);
            if (!IsListEmpty(&ctx->InflightLink)) {
                RemoveEntryList(&ctx->InflightLink);
                InitializeListHead(&ctx->InflightLink);
            }
            KeReleaseSpinLock(&Ctrl->InflightLock, oldIrql);
        }

        /* Drop both references (no completion will arrive). */
        VirtioSndCtrlRequestRelease(ctx);
        VirtioSndCtrlRequestRelease(ctx);
        return status;
    }

    InterlockedIncrement(&Ctrl->Stats.RequestsSent);

    VirtioSndQueueKick(Ctrl->ControlQ);

    /*
     * Poll used entries while waiting so this helper still functions if the
     * driver is running in a polling-only configuration (or if an interrupt is
     * delayed/lost). Control requests are infrequent, so a short polling cadence
     * keeps behavior deterministic without meaningful overhead.
     */
    now100ns = KeQueryInterruptTime();
    deadline100ns = now100ns + ((ULONGLONG)TimeoutMs * 10000ull);

    for (;;) {
        /* If already signaled, exit the loop without waiting. */
        if (KeReadStateEvent(&ctx->Event) != 0) {
            waitStatus = STATUS_SUCCESS;
            break;
        }

        VirtioSndCtrlProcessUsed(Ctrl);

        if (KeReadStateEvent(&ctx->Event) != 0) {
            waitStatus = STATUS_SUCCESS;
            break;
        }

        now100ns = KeQueryInterruptTime();
        if (now100ns >= deadline100ns) {
            VIRTIOSND_TRACE_ERROR("ctrlq timeout code=0x%08lx\n", ctx->Code);

            InterlockedIncrement(&Ctrl->Stats.RequestsTimedOut);

            /* Drop the send-thread reference. */
            VirtioSndCtrlRequestRelease(ctx);
            return STATUS_IO_TIMEOUT;
        }

        {
            ULONGLONG remaining = deadline100ns - now100ns;
            /* Poll at up to 10ms granularity. */
            if (remaining > 10ull * 1000ull * 10ull) {
                remaining = 10ull * 1000ull * 10ull;
            }
            timeout.QuadPart = -(LONGLONG)remaining;
        }

        waitStatus = KeWaitForSingleObject(&ctx->Event, Executive, KernelMode, FALSE, &timeout);
        if (waitStatus == STATUS_TIMEOUT) {
            continue;
        }
        break;
    }

    if (!NT_SUCCESS(waitStatus)) {
        VIRTIOSND_TRACE_ERROR("ctrlq wait failed: 0x%08X\n", (UINT)waitStatus);

        /* Drop the send-thread reference; completion may still arrive. */
        VirtioSndCtrlRequestRelease(ctx);
        return waitStatus;
    }

    if (ctx->CompletionStatus != STATUS_SUCCESS) {
        status = ctx->CompletionStatus;

        /* Drop the send-thread reference. */
        VirtioSndCtrlRequestRelease(ctx);
        return status;
    }

    usedLen = ctx->UsedLen;
    virtioStatus = ctx->VirtioStatus;

    copyLen = usedLen;
    if (copyLen > RespCap) {
        copyLen = RespCap;
    }
    if (copyLen != 0) {
        RtlCopyMemory(Resp, ctx->RespBuf, copyLen);
    }

    if (OutRespLen != NULL) {
        *OutRespLen = usedLen;
    }
    if (OutVirtioStatus != NULL) {
        *OutVirtioStatus = virtioStatus;
    }

    if (usedLen < sizeof(ULONG)) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        status = STATUS_DEVICE_PROTOCOL_ERROR;
#else
        status = STATUS_UNSUCCESSFUL;
#endif

        VirtioSndCtrlRequestRelease(ctx);
        return status;
    }

    status = VirtioSndStatusToNtStatus(virtioStatus);

    /* Drop the send-thread reference. */
    VirtioSndCtrlRequestRelease(ctx);
    return status;
}

NTSTATUS
VirtioSndCtrlSendSync(
    _Inout_ VIRTIOSND_CONTROL* Ctrl,
    _In_reads_bytes_(ReqLen) const void* Req,
    _In_ ULONG ReqLen,
    _Out_writes_bytes_(RespCap) void* Resp,
    _In_ ULONG RespCap,
    _In_ ULONG TimeoutMs,
    _Out_opt_ ULONG* OutVirtioStatus,
    _Out_opt_ ULONG* OutRespLen)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSendSyncLocked(Ctrl, Req, ReqLen, Resp, RespCap, TimeoutMs, OutVirtioStatus, OutRespLen);
    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

static NTSTATUS
VirtioSndCtrlPcmInfoQuery(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* PlaybackInfo, _Out_ VIRTIO_SND_PCM_INFO* CaptureInfo);

NTSTATUS
VirtioSndCtrlPcmInfo(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info)
{
    VIRTIO_SND_PCM_INFO captureInfo;

    if (Ctrl == NULL || Info == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlPcmInfoQuery(Ctrl, Info, &captureInfo);
}

static NTSTATUS
VirtioSndCtrlPcmInfoQuery(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* PlaybackInfo, _Out_ VIRTIO_SND_PCM_INFO* CaptureInfo)
{
    NTSTATUS status;
    VIRTIO_SND_PCM_INFO_REQ req;
    UCHAR resp[sizeof(ULONG) + (sizeof(VIRTIO_SND_PCM_INFO) * 2)];
    ULONG respLen;

    if (Ctrl == NULL || PlaybackInfo == NULL || CaptureInfo == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndCtrlBuildPcmInfoReq(&req);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSendSyncLocked(
        Ctrl,
        &req,
        sizeof(req),
        resp,
        sizeof(resp),
        VIRTIOSND_CTRL_TIMEOUT_DEFAULT_MS,
        NULL,
        &respLen);
    ExReleaseFastMutex(&Ctrl->Mutex);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    return VirtioSndCtrlParsePcmInfoResp(resp, respLen, PlaybackInfo, CaptureInfo);
}

NTSTATUS
VirtioSndCtrlPcmInfo1(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info)
{
    VIRTIO_SND_PCM_INFO playbackInfo;

    if (Ctrl == NULL || Info == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlPcmInfoQuery(Ctrl, &playbackInfo, Info);
}

static NTSTATUS
VirtioSndCtrlSetParamsLocked(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG StreamId, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);

NTSTATUS
VirtioSndCtrlSetParams(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSetParamsLocked(Ctrl, VIRTIO_SND_PLAYBACK_STREAM_ID, BufferBytes, PeriodBytes);
    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

static NTSTATUS
VirtioSndCtrlSetParamsLocked(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG StreamId, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    NTSTATUS status;
    VIRTIO_SND_PCM_SET_PARAMS_REQ req;
    ULONG respStatus;
    ULONG respLen;
    ULONG virtioStatus;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, StreamId, BufferBytes, PeriodBytes);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (Ctrl->StreamState[StreamId] != VirtioSndStreamStateIdle && Ctrl->StreamState[StreamId] != VirtioSndStreamStateParamsSet) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSendSyncLocked(
        Ctrl,
        &req,
        sizeof(req),
        &respStatus,
        sizeof(respStatus),
        VIRTIOSND_CTRL_TIMEOUT_DEFAULT_MS,
        &virtioStatus,
        &respLen);

    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[StreamId] = VirtioSndStreamStateParamsSet;
        Ctrl->Params[StreamId].BufferBytes = BufferBytes;
        Ctrl->Params[StreamId].PeriodBytes = PeriodBytes;
        Ctrl->Params[StreamId].Channels = req.channels;
        Ctrl->Params[StreamId].Format = req.format;
        Ctrl->Params[StreamId].Rate = req.rate;
    }

    return status;
}

NTSTATUS
VirtioSndCtrlSetParams1(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSetParamsLocked(Ctrl, VIRTIO_SND_CAPTURE_STREAM_ID, BufferBytes, PeriodBytes);
    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

static NTSTATUS
VirtioSndCtrlSimpleStreamCmdLocked(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG StreamId, _In_ ULONG Code)
{
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    ULONG respStatus;
    ULONG respLen;
    ULONG virtioStatus;

    NTSTATUS buildStatus;
    buildStatus = VirtioSndCtrlBuildPcmSimpleReq(&req, StreamId, Code);
    if (!NT_SUCCESS(buildStatus)) {
        return buildStatus;
    }

    return VirtioSndCtrlSendSyncLocked(
        Ctrl,
        &req,
        sizeof(req),
        &respStatus,
        sizeof(respStatus),
        VIRTIOSND_CTRL_TIMEOUT_DEFAULT_MS,
        &virtioStatus,
        &respLen);
}

NTSTATUS
VirtioSndCtrlPrepare(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] != VirtioSndStreamStateParamsSet &&
        Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] != VirtioSndStreamStatePrepared) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_PREPARE);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] = VirtioSndStreamStatePrepared;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlPrepare1(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] != VirtioSndStreamStateParamsSet &&
        Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] != VirtioSndStreamStatePrepared) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_PREPARE);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] = VirtioSndStreamStatePrepared;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlStart(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] != VirtioSndStreamStatePrepared &&
        Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] != VirtioSndStreamStateRunning) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_START);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] = VirtioSndStreamStateRunning;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlStart1(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] != VirtioSndStreamStatePrepared &&
        Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] != VirtioSndStreamStateRunning) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_START);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] = VirtioSndStreamStateRunning;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlStop(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] != VirtioSndStreamStateRunning) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_STOP);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] = VirtioSndStreamStatePrepared;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlStop1(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] != VirtioSndStreamStateRunning) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_STOP);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] = VirtioSndStreamStatePrepared;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlRelease(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_RELEASE);

    Ctrl->StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] = VirtioSndStreamStateIdle;
    RtlZeroMemory(&Ctrl->Params[VIRTIO_SND_PLAYBACK_STREAM_ID], sizeof(Ctrl->Params[VIRTIO_SND_PLAYBACK_STREAM_ID]));

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlRelease1(_Inout_ VIRTIOSND_CONTROL* Ctrl)
{
    NTSTATUS status;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_RELEASE);

    Ctrl->StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] = VirtioSndStreamStateIdle;
    RtlZeroMemory(&Ctrl->Params[VIRTIO_SND_CAPTURE_STREAM_ID], sizeof(Ctrl->Params[VIRTIO_SND_CAPTURE_STREAM_ID]));

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

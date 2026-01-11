/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd_control.h"

#define VIRTIOSND_CTRL_REQ_TAG 'rCSV' /* 'VSCr' */
#define VIRTIOSND_CTRL_TIMEOUT_DEFAULT_MS 1000u

/*
 * Per-request context. Allocated from NonPagedPool so it is safe to touch from
 * control-queue DPC context.
 *
 * Lifetime:
 *  - One reference is owned by the sending thread.
 *  - One reference is owned by the virtqueue cookie and released on completion.
 *
 * This ensures the DMA buffers remain valid even if the synchronous wait times
 * out and completion arrives later.
 */
typedef struct _VIRTIOSND_CTRL_REQUEST {
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

static __forceinline VOID
VirtioSndCtrlRequestRelease(_In_ VIRTIOSND_CTRL_REQUEST* Req)
{
    if (InterlockedDecrement(&Req->RefCount) == 0) {
        ExFreePoolWithTag(Req, VIRTIOSND_CTRL_REQ_TAG);
    }
}

static NTSTATUS
VirtioSndCtrlAppendSg(
    _Inout_updates_(SgCap) VIRTIOSND_SG* Sg,
    _In_ USHORT SgCap,
    _Inout_ USHORT* SgCount,
    _In_reads_bytes_(Length) const VOID* Buffer,
    _In_ ULONG Length,
    _In_ BOOLEAN Write)
{
    PUCHAR p;
    ULONG remaining;

    p = (PUCHAR)Buffer;
    remaining = Length;

    while (remaining != 0) {
        ULONG pageOffset;
        ULONG chunk;
        PHYSICAL_ADDRESS pa;

        if (*SgCount >= SgCap) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        pageOffset = (ULONG)((ULONG_PTR)p & (PAGE_SIZE - 1));
        chunk = PAGE_SIZE - pageOffset;
        if (chunk > remaining) {
            chunk = remaining;
        }

        pa = MmGetPhysicalAddress(p);

        Sg[*SgCount].addr = (UINT64)pa.QuadPart;
        Sg[*SgCount].len = (UINT32)chunk;
        Sg[*SgCount].write = Write;
        (*SgCount)++;

        p += chunk;
        remaining -= chunk;
    }

    return STATUS_SUCCESS;
}

static VOID
VirtioSndCtrlCompleteRequest(_Inout_ VIRTIOSND_CTRL_REQUEST* Req, _In_ ULONG UsedLen)
{
    ULONG virtioStatus;

    Req->UsedLen = UsedLen;

    virtioStatus = 0xFFFFFFFFu;
    if (UsedLen >= sizeof(ULONG) && Req->RespBuf != NULL) {
        virtioStatus = *(UNALIGNED const ULONG*)Req->RespBuf;
    }
    Req->VirtioStatus = virtioStatus;

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

VOID
VirtioSndCtrlInit(_Out_ VIRTIOSND_CONTROL* Ctrl, _In_ VIRTIOSND_QUEUE* ControlQ)
{
    RtlZeroMemory(Ctrl, sizeof(*Ctrl));
    Ctrl->ControlQ = ControlQ;

    ExInitializeFastMutex(&Ctrl->Mutex);

    Ctrl->StreamState = VirtioSndStreamStateIdle;
    RtlZeroMemory(&Ctrl->Params, sizeof(Ctrl->Params));
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
    VIRTIOSND_SG sg[16];
    USHORT sgCount;
    LARGE_INTEGER timeout;
    ULONG usedLen;
    ULONG virtioStatus;
    ULONG copyLen;

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

    reqOffset = ALIGN_UP_BY(sizeof(*ctx), sizeof(ULONG));
    respOffset = ALIGN_UP_BY(reqOffset + ReqLen, sizeof(ULONG));

    allocSize = (SIZE_T)respOffset + (SIZE_T)RespCap;
    if (allocSize < respOffset) {
        return STATUS_INTEGER_OVERFLOW;
    }

    ctx = (VIRTIOSND_CTRL_REQUEST*)ExAllocatePoolWithTag(NonPagedPool, allocSize, VIRTIOSND_CTRL_REQ_TAG);
    if (ctx == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(ctx, sizeof(*ctx));

    /*
     * Hold both references up-front to avoid a race where the device completes
     * immediately after submission and the completion path runs before the
     * sending thread can take an extra reference.
     */
    ctx->RefCount = 2;
    KeInitializeEvent(&ctx->Event, NotificationEvent, FALSE);

    ctx->Code = (ReqLen >= sizeof(ULONG)) ? *(UNALIGNED const ULONG*)Req : 0;
    ctx->ReqBuf = ((PUCHAR)ctx) + reqOffset;
    ctx->ReqLen = ReqLen;
    ctx->RespBuf = ((PUCHAR)ctx) + respOffset;
    ctx->RespCap = RespCap;
    ctx->UsedLen = 0;
    ctx->VirtioStatus = 0xFFFFFFFFu;

    RtlCopyMemory(ctx->ReqBuf, Req, ReqLen);
    RtlZeroMemory(ctx->RespBuf, RespCap);

    sgCount = 0;
    status = VirtioSndCtrlAppendSg(sg, (USHORT)(sizeof(sg) / sizeof(sg[0])), &sgCount, ctx->ReqBuf, ctx->ReqLen, FALSE);
    if (NT_SUCCESS(status)) {
        status = VirtioSndCtrlAppendSg(
            sg,
            (USHORT)(sizeof(sg) / sizeof(sg[0])),
            &sgCount,
            ctx->RespBuf,
            ctx->RespCap,
            TRUE);
    }
    if (!NT_SUCCESS(status)) {
        ExFreePoolWithTag(ctx, VIRTIOSND_CTRL_REQ_TAG);
        return status;
    }

    VIRTIOSND_TRACE("ctrlq send code=0x%08lx req_len=%lu resp_cap=%lu\n", ctx->Code, ReqLen, RespCap);

    status = VirtioSndQueueSubmit(Ctrl->ControlQ, sg, sgCount, ctx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("ctrlq Submit failed: 0x%08X\n", status);

        /* Drop both references (no completion will arrive). */
        VirtioSndCtrlRequestRelease(ctx);
        VirtioSndCtrlRequestRelease(ctx);
        return status;
    }

    VirtioSndQueueKick(Ctrl->ControlQ);

    /*
     * Best-effort poll in case the driver is using a polling path and the
     * completion interrupt is delayed or suppressed.
     */
    VirtioSndCtrlProcessUsed(Ctrl);

    timeout.QuadPart = -((LONGLONG)TimeoutMs * 10000); /* relative, 100ns units */
    waitStatus = KeWaitForSingleObject(&ctx->Event, Executive, KernelMode, FALSE, &timeout);
    if (waitStatus == STATUS_TIMEOUT) {
        VIRTIOSND_TRACE_ERROR("ctrlq timeout code=0x%08lx\n", ctx->Code);

        /* Drop the send-thread reference. */
        VirtioSndCtrlRequestRelease(ctx);
        return STATUS_IO_TIMEOUT;
    }

    if (!NT_SUCCESS(waitStatus)) {
        VIRTIOSND_TRACE_ERROR("ctrlq wait failed: 0x%08X\n", waitStatus);

        /* Drop the send-thread reference; completion may still arrive. */
        VirtioSndCtrlRequestRelease(ctx);
        return waitStatus;
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

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSendSyncLocked(Ctrl, Req, ReqLen, Resp, RespCap, TimeoutMs, OutVirtioStatus, OutRespLen);
    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

NTSTATUS
VirtioSndCtrlPcmInfo(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info)
{
    NTSTATUS status;
    VIRTIO_SND_PCM_INFO_REQ req;
    UCHAR resp[sizeof(ULONG) + sizeof(VIRTIO_SND_PCM_INFO)];
    ULONG respLen;
    ULONG virtioStatus;
    VIRTIO_SND_PCM_INFO info;

    if (Ctrl == NULL || Info == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_INFO;
    req.start_id = 0;
    req.count = 1;

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSendSyncLocked(
        Ctrl,
        &req,
        sizeof(req),
        resp,
        sizeof(resp),
        VIRTIOSND_CTRL_TIMEOUT_DEFAULT_MS,
        &virtioStatus,
        &respLen);
    ExReleaseFastMutex(&Ctrl->Mutex);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (respLen < sizeof(ULONG)) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    if (virtioStatus != VIRTIO_SND_S_OK) {
        return VirtioSndStatusToNtStatus(virtioStatus);
    }

    if (respLen < sizeof(ULONG) + sizeof(VIRTIO_SND_PCM_INFO)) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    RtlCopyMemory(&info, resp + sizeof(ULONG), sizeof(info));

    if (info.stream_id != VIRTIO_SND_PLAYBACK_STREAM_ID) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }
    if (info.direction != VIRTIO_SND_D_OUTPUT) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    if ((info.formats & VIRTIO_SND_PCM_FMT_MASK_S16) == 0 || (info.rates & VIRTIO_SND_PCM_RATE_MASK_48000) == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    if (info.channels_min > 2 || info.channels_max < 2) {
        return STATUS_NOT_SUPPORTED;
    }

    *Info = info;
    return STATUS_SUCCESS;
}

NTSTATUS
VirtioSndCtrlSetParams(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    NTSTATUS status;
    VIRTIO_SND_PCM_SET_PARAMS_REQ req;
    ULONG respStatus;
    ULONG respLen;
    ULONG virtioStatus;

    if (Ctrl == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_SET_PARAMS;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    req.buffer_bytes = BufferBytes;
    req.period_bytes = PeriodBytes;
    req.features = 0;
    req.channels = 2;
    req.format = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
    req.rate = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
    req.padding = 0;

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState != VirtioSndStreamStateIdle && Ctrl->StreamState != VirtioSndStreamStateParamsSet) {
        ExReleaseFastMutex(&Ctrl->Mutex);
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
        Ctrl->StreamState = VirtioSndStreamStateParamsSet;
        Ctrl->Params.BufferBytes = BufferBytes;
        Ctrl->Params.PeriodBytes = PeriodBytes;
        Ctrl->Params.Channels = 2;
        Ctrl->Params.Format = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
        Ctrl->Params.Rate = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
    }

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

static NTSTATUS
VirtioSndCtrlSimpleStreamCmdLocked(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG Code)
{
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    ULONG respStatus;
    ULONG respLen;
    ULONG virtioStatus;

    RtlZeroMemory(&req, sizeof(req));
    req.code = Code;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

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

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState != VirtioSndStreamStateParamsSet && Ctrl->StreamState != VirtioSndStreamStatePrepared) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_R_PCM_PREPARE);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState = VirtioSndStreamStatePrepared;
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

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState != VirtioSndStreamStatePrepared && Ctrl->StreamState != VirtioSndStreamStateRunning) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_R_PCM_START);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState = VirtioSndStreamStateRunning;
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

    ExAcquireFastMutex(&Ctrl->Mutex);

    if (Ctrl->StreamState != VirtioSndStreamStateRunning) {
        ExReleaseFastMutex(&Ctrl->Mutex);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_R_PCM_STOP);
    if (NT_SUCCESS(status)) {
        Ctrl->StreamState = VirtioSndStreamStatePrepared;
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

    ExAcquireFastMutex(&Ctrl->Mutex);
    status = VirtioSndCtrlSimpleStreamCmdLocked(Ctrl, VIRTIO_SND_R_PCM_RELEASE);

    Ctrl->StreamState = VirtioSndStreamStateIdle;
    RtlZeroMemory(&Ctrl->Params, sizeof(Ctrl->Params));

    ExReleaseFastMutex(&Ctrl->Mutex);
    return status;
}

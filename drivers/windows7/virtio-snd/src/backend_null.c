/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "backend.h"
#include "trace.h"
#include "virtiosnd.h"

typedef struct _VIRTIOSND_BACKEND_NULL {
    VIRTIOSND_BACKEND Backend;
    /* Render (stream 0 / TX) */
    ULONG RenderBufferBytes;
    ULONG RenderPeriodBytes;
    ULONGLONG TotalBytesWritten;
    BOOLEAN RenderPrepared;
    BOOLEAN RenderRunning;

    /* Capture (stream 1 / RX) */
    ULONG CaptureBufferBytes;
    ULONG CapturePeriodBytes;
    BOOLEAN CapturePrepared;
    BOOLEAN CaptureRunning;
    volatile LONG CapturePendingCompletions;
    void* CaptureLastCookie;
} VIRTIOSND_BACKEND_NULL, *PVIRTIOSND_BACKEND_NULL;

static NTSTATUS
VirtIoSndBackendNull_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->RenderBufferBytes = BufferBytes;
    ctx->RenderPeriodBytes = PeriodBytes;
    VIRTIOSND_TRACE("backend(null): SetParams buffer=%lu period=%lu\n", BufferBytes, PeriodBytes);
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Prepare(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->RenderPrepared = TRUE;
    VIRTIOSND_TRACE("backend(null): Prepare\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Start(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->RenderRunning = TRUE;
    VIRTIOSND_TRACE("backend(null): Start\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Stop(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->RenderRunning = FALSE;
    VIRTIOSND_TRACE("backend(null): Stop\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Release(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->RenderPrepared = FALSE;
    ctx->RenderRunning = FALSE;
    ctx->TotalBytesWritten = 0;
    VIRTIOSND_TRACE("backend(null): Release\n");
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndBackendNull_WritePeriod(
    _In_ PVOID Context,
    _In_ UINT64 Pcm1DmaAddr,
    _In_ SIZE_T Pcm1Bytes,
    _In_ UINT64 Pcm2DmaAddr,
    _In_ SIZE_T Pcm2Bytes
    )
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    UNREFERENCED_PARAMETER(Pcm1DmaAddr);
    UNREFERENCED_PARAMETER(Pcm2DmaAddr);

    ctx->TotalBytesWritten += (ULONGLONG)Pcm1Bytes + (ULONGLONG)Pcm2Bytes;

    if (ctx->RenderRunning) {
        VIRTIOSND_TRACE(
            "backend(null): WritePeriod %Iu+%Iu (total=%I64u)\n",
            Pcm1Bytes,
            Pcm2Bytes,
            ctx->TotalBytesWritten);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndBackendNull_SetParamsCapture(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->CaptureBufferBytes = BufferBytes;
    ctx->CapturePeriodBytes = PeriodBytes;
    ctx->CapturePrepared = FALSE;
    ctx->CaptureRunning = FALSE;
    InterlockedExchange(&ctx->CapturePendingCompletions, 0);
    ctx->CaptureLastCookie = NULL;
    VIRTIOSND_TRACE("backend(null): SetParamsCapture buffer=%lu period=%lu\n", BufferBytes, PeriodBytes);
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_PrepareCapture(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->CapturePrepared = TRUE;
    VIRTIOSND_TRACE("backend(null): PrepareCapture\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_StartCapture(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->CaptureRunning = TRUE;
    VIRTIOSND_TRACE("backend(null): StartCapture\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_StopCapture(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->CaptureRunning = FALSE;
    VIRTIOSND_TRACE("backend(null): StopCapture\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_ReleaseCapture(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->CapturePrepared = FALSE;
    ctx->CaptureRunning = FALSE;
    ctx->CaptureBufferBytes = 0;
    ctx->CapturePeriodBytes = 0;
    InterlockedExchange(&ctx->CapturePendingCompletions, 0);
    ctx->CaptureLastCookie = NULL;
    VIRTIOSND_TRACE("backend(null): ReleaseCapture\n");
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndBackendNull_SubmitCapturePeriodSg(
    _In_ PVOID Context,
    _In_reads_(SegmentCount) const VIRTIOSND_RX_SEGMENT* Segments,
    _In_ USHORT SegmentCount,
    _In_opt_ void* Cookie)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    UNREFERENCED_PARAMETER(Segments);
    UNREFERENCED_PARAMETER(SegmentCount);

    if (!ctx->CaptureRunning) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx->CaptureLastCookie = Cookie;
    InterlockedIncrement(&ctx->CapturePendingCompletions);
    return STATUS_SUCCESS;
}

static ULONG
VirtIoSndBackendNull_DrainCaptureCompletions(
    _In_ PVOID Context,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION* Callback,
    _In_opt_ void* CallbackContext)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    LONG pending;

    pending = InterlockedExchange(&ctx->CapturePendingCompletions, 0);
    if (pending <= 0) {
        return 0;
    }

    if (Callback != NULL) {
        LONG i;
        for (i = 0; i < pending; i++) {
            Callback(
                ctx->CaptureLastCookie,
                STATUS_SUCCESS,
                VIRTIO_SND_S_OK,
                0,
                0,
                (UINT32)sizeof(VIRTIO_SND_PCM_STATUS),
                CallbackContext);
        }
    }

    return (ULONG)pending;
}

static VOID VirtIoSndBackendNull_Destroy(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
}

static const VIRTIOSND_BACKEND_OPS g_VirtIoSndBackendNullOps = {
    VirtIoSndBackendNull_SetParams,
    VirtIoSndBackendNull_Prepare,
    VirtIoSndBackendNull_Start,
    VirtIoSndBackendNull_Stop,
    VirtIoSndBackendNull_Release,
    VirtIoSndBackendNull_WritePeriod,
    VirtIoSndBackendNull_SetParamsCapture,
    VirtIoSndBackendNull_PrepareCapture,
    VirtIoSndBackendNull_StartCapture,
    VirtIoSndBackendNull_StopCapture,
    VirtIoSndBackendNull_ReleaseCapture,
    VirtIoSndBackendNull_SubmitCapturePeriodSg,
    VirtIoSndBackendNull_DrainCaptureCompletions,
    VirtIoSndBackendNull_Destroy,
};

NTSTATUS
VirtIoSndBackendNull_Create(_Outptr_result_maybenull_ PVIRTIOSND_BACKEND *OutBackend)
{
    PVIRTIOSND_BACKEND_NULL backend;

    if (OutBackend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutBackend = NULL;

    backend = (PVIRTIOSND_BACKEND_NULL)ExAllocatePoolWithTag(
        NonPagedPool,
        sizeof(*backend),
        VIRTIOSND_POOL_TAG);
    if (backend == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(backend, sizeof(*backend));
    backend->Backend.Ops = &g_VirtIoSndBackendNullOps;
    backend->Backend.Context = backend;

    *OutBackend = &backend->Backend;
    return STATUS_SUCCESS;
}

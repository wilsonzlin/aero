#include <ntddk.h>

#include "backend.h"
#include "trace.h"
#include "virtiosnd.h"

typedef struct _VIRTIOSND_BACKEND_NULL {
    VIRTIOSND_BACKEND Backend;
    ULONG BufferBytes;
    ULONG PeriodBytes;
    ULONGLONG TotalBytesWritten;
    BOOLEAN Prepared;
    BOOLEAN Running;
} VIRTIOSND_BACKEND_NULL, *PVIRTIOSND_BACKEND_NULL;

static NTSTATUS
VirtIoSndBackendNull_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->BufferBytes = BufferBytes;
    ctx->PeriodBytes = PeriodBytes;
    VIRTIOSND_TRACE("backend(null): SetParams buffer=%lu period=%lu\n", BufferBytes, PeriodBytes);
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Prepare(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->Prepared = TRUE;
    VIRTIOSND_TRACE("backend(null): Prepare\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Start(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->Running = TRUE;
    VIRTIOSND_TRACE("backend(null): Start\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Stop(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->Running = FALSE;
    VIRTIOSND_TRACE("backend(null): Stop\n");
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendNull_Release(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    ctx->Prepared = FALSE;
    ctx->Running = FALSE;
    ctx->TotalBytesWritten = 0;
    VIRTIOSND_TRACE("backend(null): Release\n");
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndBackendNull_Write(_In_ PVOID Context, _In_reads_bytes_(Bytes) const VOID *Pcm, _In_ SIZE_T Bytes)
{
    PVIRTIOSND_BACKEND_NULL ctx = (PVIRTIOSND_BACKEND_NULL)Context;
    UNREFERENCED_PARAMETER(Pcm);

    ctx->TotalBytesWritten += Bytes;

    if (ctx->Running) {
        VIRTIOSND_TRACE("backend(null): Write %Iu (total=%I64u)\n", Bytes, ctx->TotalBytesWritten);
    }

    return STATUS_SUCCESS;
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
    VirtIoSndBackendNull_Write,
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

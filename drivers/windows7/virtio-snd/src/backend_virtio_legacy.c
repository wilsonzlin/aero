/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#if !defined(_KERNEL_MODE)
#error virtio-snd is a kernel-mode driver
#endif

#include <ntddk.h>

#include "aeroviosnd.h"
#include "aeroviosnd_backend.h"
#include "trace.h"

typedef struct _AEROVIOSND_BACKEND_LEGACY {
    VIRTIOSND_BACKEND Backend;
    PAEROVIOSND_DEVICE_EXTENSION Dx;
} AEROVIOSND_BACKEND_LEGACY, *PAEROVIOSND_BACKEND_LEGACY;

static NTSTATUS VirtIoSndBackendLegacy_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndHwSetPcmParams(ctx->Dx, BufferBytes, PeriodBytes);
}

static NTSTATUS VirtIoSndBackendLegacy_Prepare(_In_ PVOID Context)
{
    /*
     * The WaveRT state machine issues Prepare immediately before Start when
     * transitioning to KSSTATE_RUN. The legacy hardware helper does both prepare
     * and start; call it here to ensure the PCM stream is ready.
     */
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndHwStartPcm(ctx->Dx);
}

static NTSTATUS VirtIoSndBackendLegacy_Start(_In_ PVOID Context)
{
    // Idempotent (VirtIoSndHwStartPcm returns success if already running).
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndHwStartPcm(ctx->Dx);
}

static NTSTATUS VirtIoSndBackendLegacy_Stop(_In_ PVOID Context)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndHwStopPcm(ctx->Dx);
}

static NTSTATUS VirtIoSndBackendLegacy_Release(_In_ PVOID Context)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndHwReleasePcm(ctx->Dx);
}

static NTSTATUS VirtIoSndBackendLegacy_Write(_In_ PVOID Context, _In_reads_bytes_(Bytes) const VOID* Pcm, _In_ SIZE_T Bytes)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    ULONG bytes32;

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (Pcm == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Bytes == 0) {
        return STATUS_SUCCESS;
    }
    if (Bytes > 0xFFFFFFFFull) {
        return STATUS_INVALID_PARAMETER;
    }

    bytes32 = (ULONG)Bytes;
    return VirtIoSndHwSubmitTx(ctx->Dx, Pcm, bytes32);
}

static VOID VirtIoSndBackendLegacy_Destroy(_In_ PVOID Context)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL) {
        return;
    }

    ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
}

static const VIRTIOSND_BACKEND_OPS g_VirtIoSndBackendLegacyOps = {
    VirtIoSndBackendLegacy_SetParams,
    VirtIoSndBackendLegacy_Prepare,
    VirtIoSndBackendLegacy_Start,
    VirtIoSndBackendLegacy_Stop,
    VirtIoSndBackendLegacy_Release,
    VirtIoSndBackendLegacy_Write,
    VirtIoSndBackendLegacy_Destroy,
};

_Use_decl_annotations_ NTSTATUS VirtIoSndBackendLegacy_Create(PAEROVIOSND_DEVICE_EXTENSION Dx, PVIRTIOSND_BACKEND* OutBackend)
{
    PAEROVIOSND_BACKEND_LEGACY backend;

    if (OutBackend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *OutBackend = NULL;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    backend = (PAEROVIOSND_BACKEND_LEGACY)ExAllocatePoolWithTag(NonPagedPool, sizeof(*backend), VIRTIOSND_POOL_TAG);
    if (backend == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(backend, sizeof(*backend));

    backend->Backend.Ops = &g_VirtIoSndBackendLegacyOps;
    backend->Backend.Context = backend;
    backend->Dx = Dx;

    *OutBackend = &backend->Backend;
    VIRTIOSND_TRACE("backend(legacy-virtio): created\n");
    return STATUS_SUCCESS;
}

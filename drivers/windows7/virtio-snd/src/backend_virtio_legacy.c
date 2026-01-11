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
    PUCHAR Staging;
    ULONG StagingBytes;
    ULONG PeriodBytes;
} AEROVIOSND_BACKEND_LEGACY, *PAEROVIOSND_BACKEND_LEGACY;

static ULONG
VirtIoSndBackendLegacy_DrainTxCompletions(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx)
{
    USHORT head;
    ULONG len;
    PVOID ctx;
    KIRQL oldIrql;
    ULONG count;

    if (Dx == NULL || !Dx->Started) {
        return 0;
    }

    head = 0;
    len = 0;
    ctx = NULL;
    count = 0;

    KeAcquireSpinLock(&Dx->Lock, &oldIrql);

    while (VirtioQueuePopUsed(&Dx->TxVq, &head, &len, &ctx)) {
        PAEROVIOSND_TX_ENTRY entry = (PAEROVIOSND_TX_ENTRY)ctx;
        UNREFERENCED_PARAMETER(head);
        UNREFERENCED_PARAMETER(len);
        if (entry != NULL) {
            RemoveEntryList(&entry->Link);
            InsertTailList(&Dx->TxFreeList, &entry->Link);
            count++;
        }
    }

    KeReleaseSpinLock(&Dx->Lock, oldIrql);
    return count;
}

static NTSTATUS VirtIoSndBackendLegacy_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    NTSTATUS status;
    PUCHAR staging;

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtIoSndHwSetPcmParams(ctx->Dx, BufferBytes, PeriodBytes);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (PeriodBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    staging = ctx->Staging;
    if (staging == NULL || ctx->StagingBytes < PeriodBytes) {
        staging = (PUCHAR)ExAllocatePoolWithTag(NonPagedPool, PeriodBytes, VIRTIOSND_POOL_TAG);
        if (staging == NULL) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory(staging, PeriodBytes);

        if (ctx->Staging != NULL) {
            ExFreePoolWithTag(ctx->Staging, VIRTIOSND_POOL_TAG);
        }
        ctx->Staging = staging;
        ctx->StagingBytes = PeriodBytes;
    }

    ctx->PeriodBytes = PeriodBytes;
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendLegacy_Prepare(_In_ PVOID Context)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndHwPreparePcm(ctx->Dx);
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

static NTSTATUS
VirtIoSndBackendLegacy_WritePeriod(
    _In_ PVOID Context,
    _In_opt_ const VOID* Pcm1,
    _In_ SIZE_T Pcm1Bytes,
    _In_opt_ const VOID* Pcm2,
    _In_ SIZE_T Pcm2Bytes
    )
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    SIZE_T totalBytes;
    ULONG periodBytes;
    NTSTATUS status;
    const VOID* submit;

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    periodBytes = ctx->PeriodBytes;
    if (periodBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    totalBytes = Pcm1Bytes + Pcm2Bytes;
    if (totalBytes < Pcm1Bytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (totalBytes != (SIZE_T)periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /*
     * Drain TX completions proactively; this keeps forward progress if an
     * interrupt is delayed/lost and reduces starvation risk with small TX pools.
     */
    (VOID)VirtIoSndBackendLegacy_DrainTxCompletions(ctx->Dx);

    submit = Pcm1;

    if (Pcm2Bytes != 0 || submit == NULL) {
        if (ctx->Staging == NULL || ctx->StagingBytes < periodBytes) {
            return STATUS_INVALID_DEVICE_STATE;
        }

        if (Pcm1Bytes != 0) {
            if (Pcm1 != NULL) {
                RtlCopyMemory(ctx->Staging, Pcm1, Pcm1Bytes);
            } else {
                RtlZeroMemory(ctx->Staging, Pcm1Bytes);
            }
        }
        if (Pcm2Bytes != 0) {
            if (Pcm2 != NULL) {
                RtlCopyMemory(ctx->Staging + Pcm1Bytes, Pcm2, Pcm2Bytes);
            } else {
                RtlZeroMemory(ctx->Staging + Pcm1Bytes, Pcm2Bytes);
            }
        }

        submit = ctx->Staging;
    }

    status = VirtIoSndHwSubmitTx(ctx->Dx, submit, periodBytes);
    if (status == STATUS_INSUFFICIENT_RESOURCES) {
        (VOID)VirtIoSndBackendLegacy_DrainTxCompletions(ctx->Dx);
        status = VirtIoSndHwSubmitTx(ctx->Dx, submit, periodBytes);
        if (status == STATUS_INSUFFICIENT_RESOURCES) {
            /*
             * No buffers available right now. Treat as a dropped period so the
             * WaveRT engine can keep moving; the host side outputs silence on
             * underrun.
             */
            return STATUS_SUCCESS;
        }
    }

    return status;
}

static VOID VirtIoSndBackendLegacy_Destroy(_In_ PVOID Context)
{
    PAEROVIOSND_BACKEND_LEGACY ctx = (PAEROVIOSND_BACKEND_LEGACY)Context;
    if (ctx == NULL) {
        return;
    }

    if (ctx->Staging != NULL) {
        ExFreePoolWithTag(ctx->Staging, VIRTIOSND_POOL_TAG);
        ctx->Staging = NULL;
        ctx->StagingBytes = 0;
    }

    ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
}

static const VIRTIOSND_BACKEND_OPS g_VirtIoSndBackendLegacyOps = {
    VirtIoSndBackendLegacy_SetParams,
    VirtIoSndBackendLegacy_Prepare,
    VirtIoSndBackendLegacy_Start,
    VirtIoSndBackendLegacy_Stop,
    VirtIoSndBackendLegacy_Release,
    VirtIoSndBackendLegacy_WritePeriod,
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
    backend->Staging = NULL;
    backend->StagingBytes = 0;
    backend->PeriodBytes = Dx->PeriodBytes;

    *OutBackend = &backend->Backend;
    VIRTIOSND_TRACE("backend(legacy-virtio): created\n");
    return STATUS_SUCCESS;
}

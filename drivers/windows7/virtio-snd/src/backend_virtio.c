/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "backend.h"
#include "trace.h"
#include "virtiosnd.h"

typedef struct _VIRTIOSND_BACKEND_VIRTIO {
    VIRTIOSND_BACKEND Backend;
    PVIRTIOSND_DEVICE_EXTENSION Dx;
    ULONG BufferBytes;
    ULONG PeriodBytes;
} VIRTIOSND_BACKEND_VIRTIO, *PVIRTIOSND_BACKEND_VIRTIO;

static NTSTATUS
VirtIoSndBackendVirtio_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;
    NTSTATUS status;
    VIRTIO_SND_PCM_INFO info;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->Dx->Started || ctx->Dx->Removed) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (PeriodBytes == 0 || BufferBytes == 0 || (PeriodBytes % VirtioSndTxFrameSizeBytes()) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndCtrlPcmInfo(&ctx->Dx->Control, &info);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("backend(virtio): PCM_INFO failed: 0x%08X\n", (UINT)status);
        return status;
    }

    status = VirtioSndCtrlSetParams(&ctx->Dx->Control, BufferBytes, PeriodBytes);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("backend(virtio): SET_PARAMS failed: 0x%08X\n", (UINT)status);
        return status;
    }

    if (ctx->Dx->Tx.Buffers == NULL || ctx->Dx->Tx.MaxPeriodBytes != PeriodBytes) {
        VirtioSndTxUninit(&ctx->Dx->Tx);

        status = VirtioSndTxInit(
            &ctx->Dx->Tx,
            &ctx->Dx->DmaCtx,
            &ctx->Dx->Queues[VIRTIOSND_QUEUE_TX],
            PeriodBytes,
            8);
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("backend(virtio): TxInit failed: 0x%08X\n", (UINT)status);
            return status;
        }
    }

    /*
     * Allow the INTx DPC to drain and dispatch txq used entries to the TX engine.
     * Without this, completions will never be processed and the buffer pool will
     * eventually exhaust.
     */
    InterlockedExchange(&ctx->Dx->TxEngineInitialized, 1);

    ctx->BufferBytes = BufferBytes;
    ctx->PeriodBytes = PeriodBytes;

    VIRTIOSND_TRACE("backend(virtio): SetParams buffer=%lu period=%lu\n", BufferBytes, PeriodBytes);
    return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndBackendVirtio_Prepare(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->Dx->Started || ctx->Dx->Removed) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlPrepare(&ctx->Dx->Control);
}

static NTSTATUS VirtIoSndBackendVirtio_Start(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->Dx->Started || ctx->Dx->Removed) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlStart(&ctx->Dx->Control);
}

static NTSTATUS VirtIoSndBackendVirtio_Stop(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->Dx->Started || ctx->Dx->Removed) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlStop(&ctx->Dx->Control);
}

static NTSTATUS VirtIoSndBackendVirtio_Release(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;
    NTSTATUS status;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->Dx->Started || ctx->Dx->Removed) {
        // Device is gone/stopped; just discard local TX buffers.
        VirtioSndTxUninit(&ctx->Dx->Tx);
        ctx->BufferBytes = 0;
        ctx->PeriodBytes = 0;
        return STATUS_SUCCESS;
    }

    status = VirtioSndCtrlRelease(&ctx->Dx->Control);

    ctx->BufferBytes = 0;
    ctx->PeriodBytes = 0;

    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_Write(_In_ PVOID Context, _In_reads_bytes_(Bytes) const VOID *Pcm, _In_ SIZE_T Bytes)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;
    ULONG periodBytes;
    NTSTATUS status;

    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->Dx->Started || ctx->Dx->Removed) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    periodBytes = ctx->PeriodBytes;
    if (periodBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Bytes > periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    // Poll used entries to recycle TX buffers even if interrupts are delayed.
    VirtioSndTxProcessCompletions(&ctx->Dx->Tx);

    status = VirtioSndTxSubmitPeriod(&ctx->Dx->Tx, Pcm, (ULONG)Bytes, NULL, 0, TRUE);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    return STATUS_SUCCESS;
}

static VOID VirtIoSndBackendVirtio_Destroy(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx = (PVIRTIOSND_BACKEND_VIRTIO)Context;
    ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
}

static const VIRTIOSND_BACKEND_OPS g_VirtIoSndBackendVirtioOps = {
    VirtIoSndBackendVirtio_SetParams,
    VirtIoSndBackendVirtio_Prepare,
    VirtIoSndBackendVirtio_Start,
    VirtIoSndBackendVirtio_Stop,
    VirtIoSndBackendVirtio_Release,
    VirtIoSndBackendVirtio_Write,
    VirtIoSndBackendVirtio_Destroy,
};

NTSTATUS
VirtIoSndBackendVirtio_Create(_In_ PVIRTIOSND_DEVICE_EXTENSION Dx, _Outptr_result_maybenull_ PVIRTIOSND_BACKEND *OutBackend)
{
    PVIRTIOSND_BACKEND_VIRTIO backend;

    if (OutBackend == NULL || Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutBackend = NULL;

    backend = (PVIRTIOSND_BACKEND_VIRTIO)ExAllocatePoolWithTag(NonPagedPool, sizeof(*backend), VIRTIOSND_POOL_TAG);
    if (backend == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(backend, sizeof(*backend));
    backend->Backend.Ops = &g_VirtIoSndBackendVirtioOps;
    backend->Backend.Context = backend;
    backend->Dx = Dx;

    *OutBackend = &backend->Backend;
    return STATUS_SUCCESS;
}

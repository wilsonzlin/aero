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

static __forceinline PVIRTIOSND_BACKEND_VIRTIO
VirtIoSndBackendVirtioFromContext(_In_ PVOID Context)
{
    return (PVIRTIOSND_BACKEND_VIRTIO)Context;
}

static NTSTATUS
VirtIoSndBackendVirtio_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;
    ULONG txBuffers;
    USHORT qsz;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    dx = ctx->Dx;

    if (dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    /*
     * virtio-snd uses byte counts, but the device requires PCM payloads to be
     * frame-aligned. Clamp to S16_LE stereo framing.
     */
    BufferBytes &= ~(VIRTIOSND_BLOCK_ALIGN - 1u);
    PeriodBytes &= ~(VIRTIOSND_BLOCK_ALIGN - 1u);
    if (BufferBytes == 0 || PeriodBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndCtrlSetParams(&dx->Control, BufferBytes, PeriodBytes);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("backend(virtio): SET_PARAMS failed: 0x%08X\n", (UINT)status);
        return status;
    }

    /*
     * The tx engine is stream-specific (depends on period size and pool depth),
     * so bring it up on the first SetParams and re-create it if the period size
     * changes.
     */
    if (InterlockedCompareExchange(&dx->TxEngineInitialized, 0, 0) != 0 && dx->Tx.MaxPeriodBytes != PeriodBytes) {
        VirtIoSndUninitTxEngine(dx);
    }

    if (InterlockedCompareExchange(&dx->TxEngineInitialized, 0, 0) == 0) {
        qsz = dx->QueueSplit[VIRTIOSND_QUEUE_TX].QueueSize;
        txBuffers = 64;
        if (qsz != 0 && txBuffers > (ULONG)qsz / 2u) {
            txBuffers = (ULONG)qsz / 2u;
        }
        if (txBuffers == 0) {
            txBuffers = 1;
        }

        status = VirtIoSndInitTxEngine(dx, PeriodBytes, txBuffers, TRUE);
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("backend(virtio): Tx engine init failed: 0x%08X\n", (UINT)status);
            return status;
        }
    }

    ctx->BufferBytes = BufferBytes;
    ctx->PeriodBytes = PeriodBytes;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndBackendVirtio_Prepare(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ctx->Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!ctx->Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlPrepare(&ctx->Dx->Control);
}

static NTSTATUS
VirtIoSndBackendVirtio_Start(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ctx->Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!ctx->Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlStart(&ctx->Dx->Control);
}

static NTSTATUS
VirtIoSndBackendVirtio_Stop(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ctx->Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!ctx->Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlStop(&ctx->Dx->Control);
}

static NTSTATUS
VirtIoSndBackendVirtio_Release(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    NTSTATUS status;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ctx->Dx->Removed || !ctx->Dx->Started) {
        /*
         * Device is already stopped/removed (STOP_DEVICE / REMOVE_DEVICE path).
         * Tear down the local tx engine best-effort so buffers are not leaked.
         */
        VirtIoSndUninitTxEngine(ctx->Dx);
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
VirtIoSndBackendVirtio_WritePeriod(
    _In_ PVOID Context,
    _In_opt_ const VOID *Pcm1,
    _In_ SIZE_T Pcm1Bytes,
    _In_opt_ const VOID *Pcm2,
    _In_ SIZE_T Pcm2Bytes
    )
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG periodBytes;
    SIZE_T totalBytes;
    NTSTATUS status;

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    dx = ctx->Dx;

    if (dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!dx->Started) {
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
     * Drain completions proactively so small TX buffer pools don't starve.
     *
     * Note: In Aero today TX completions are effectively immediate, but that is not
     * a playback clock; it's just resource reclamation.
     */
    (VOID)VirtIoSndHwDrainTxCompletions(dx);

    status = VirtIoSndHwSubmitTx(dx, Pcm1, (ULONG)Pcm1Bytes, Pcm2, (ULONG)Pcm2Bytes, TRUE);
    if (status == STATUS_INSUFFICIENT_RESOURCES) {
        (VOID)VirtIoSndHwDrainTxCompletions(dx);
        status = VirtIoSndHwSubmitTx(dx, Pcm1, (ULONG)Pcm1Bytes, Pcm2, (ULONG)Pcm2Bytes, TRUE);
        if (status == STATUS_INSUFFICIENT_RESOURCES) {
            /*
             * No buffers available right now. Treat as a dropped period so the WaveRT engine
             * can keep moving; the host side is expected to output silence on underrun.
             */
            return STATUS_SUCCESS;
        }
    }

    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    return status;
}

static VOID
VirtIoSndBackendVirtio_Destroy(_In_ PVOID Context)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL) {
        return;
    }

    ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
}

static const VIRTIOSND_BACKEND_OPS g_VirtIoSndBackendVirtioOps = {
    VirtIoSndBackendVirtio_SetParams,
    VirtIoSndBackendVirtio_Prepare,
    VirtIoSndBackendVirtio_Start,
    VirtIoSndBackendVirtio_Stop,
    VirtIoSndBackendVirtio_Release,
    VirtIoSndBackendVirtio_WritePeriod,
    VirtIoSndBackendVirtio_Destroy,
};

_Use_decl_annotations_
NTSTATUS
VirtIoSndBackendVirtio_Create(PVIRTIOSND_DEVICE_EXTENSION Dx, PVIRTIOSND_BACKEND *OutBackend)
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

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "backend.h"
#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_control_proto.h"
#include "virtiosnd_limits.h"

typedef struct _VIRTIOSND_BACKEND_VIRTIO {
    VIRTIOSND_BACKEND Backend;
    PVIRTIOSND_DEVICE_EXTENSION Dx;
    /* Render (stream 0 / TX) */
    ULONG RenderBufferBytes;
    ULONG RenderPeriodBytes;
    ULONG RenderFrameBytes;

    /* Capture (stream 1 / RX) */
    ULONG CaptureBufferBytes;
    ULONG CapturePeriodBytes;
    ULONG CaptureFrameBytes;
} VIRTIOSND_BACKEND_VIRTIO, *PVIRTIOSND_BACKEND_VIRTIO;

static __forceinline PVIRTIOSND_BACKEND_VIRTIO
VirtIoSndBackendVirtioFromContext(_In_ PVOID Context)
{
    return (PVIRTIOSND_BACKEND_VIRTIO)Context;
}

static __forceinline ULONG
VirtIoSndBackendVirtioFrameBytesForStream(_In_ const PVIRTIOSND_DEVICE_EXTENSION Dx, _In_ ULONG StreamId)
{
    USHORT bytesPerSample;
    ULONG frameBytes;
    VIRTIOSND_PCM_FORMAT selected;

    if (Dx == NULL) {
        return 0;
    }

    if (StreamId != VIRTIO_SND_PLAYBACK_STREAM_ID && StreamId != VIRTIO_SND_CAPTURE_STREAM_ID) {
        return 0;
    }

    selected = Dx->Control.SelectedFormat[StreamId];

    bytesPerSample = 0;
    if (selected.Channels != 0 && VirtioSndPcmFormatToBytesPerSample(selected.Format, &bytesPerSample) && bytesPerSample != 0) {
        frameBytes = (ULONG)selected.Channels * (ULONG)bytesPerSample;
        if (frameBytes != 0) {
            return frameBytes;
        }
    }

    /* Fallback to Aero contract v1 fixed formats. */
    return (StreamId == VIRTIO_SND_CAPTURE_STREAM_ID) ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN;
}

static NTSTATUS
VirtIoSndBackendVirtio_SetParams(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;
    ULONG txBuffers;
    USHORT qsz;
    VIRTIOSND_STREAM_STATE streamState;

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
     * frame-aligned. Clamp to the currently selected stream format (defaults to
     * the contract-v1 baseline S16/48kHz stereo).
     */
    {
        ULONG frameBytes = VirtIoSndBackendVirtioFrameBytesForStream(dx, VIRTIO_SND_PLAYBACK_STREAM_ID);
        if (frameBytes == 0) {
            return STATUS_INVALID_DEVICE_STATE;
        }
        BufferBytes = (BufferBytes / frameBytes) * frameBytes;
        PeriodBytes = (PeriodBytes / frameBytes) * frameBytes;
        ctx->RenderFrameBytes = frameBytes;
    }
    if (BufferBytes == 0 || PeriodBytes == 0 || PeriodBytes > BufferBytes) {
        return STATUS_INVALID_PARAMETER;
    }
    if (PeriodBytes > VIRTIOSND_MAX_PCM_PAYLOAD_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (BufferBytes > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if ((BufferBytes % PeriodBytes) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /*
     * SET_PARAMS is only valid when the PCM stream is Idle/ParamsSet. WaveRT can
     * reallocate buffers while paused, so ensure the virtio-snd PCM state machine
     * is back in Idle first.
     */
    streamState = dx->Control.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID];
    if (streamState == VirtioSndStreamStateRunning) {
        (VOID)VirtioSndCtrlStop(&dx->Control);
        streamState = dx->Control.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID];
    }
    if (streamState != VirtioSndStreamStateIdle && streamState != VirtioSndStreamStateParamsSet) {
        (VOID)VirtioSndCtrlRelease(&dx->Control);
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
    if (InterlockedCompareExchange(&dx->TxEngineInitialized, 0, 0) != 0 &&
        (dx->Tx.MaxPeriodBytes != PeriodBytes || dx->Tx.FrameBytes != ctx->RenderFrameBytes)) {
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

        status = VirtIoSndInitTxEngineEx(dx, ctx->RenderFrameBytes, PeriodBytes, txBuffers, TRUE);
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("backend(virtio): Tx engine init failed: 0x%08X\n", (UINT)status);
            return status;
        }
    }

    ctx->RenderBufferBytes = BufferBytes;
    ctx->RenderPeriodBytes = PeriodBytes;
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
    NTSTATUS status;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Stop is best-effort and should be idempotent. PortCls may invoke stream
     * state transitions during STOP_DEVICE / (surprise) REMOVE teardown after
     * the adapter has already been stopped.
     */
    if (ctx->Dx->Removed || !ctx->Dx->Started) {
        return STATUS_SUCCESS;
    }

    status = VirtioSndCtrlStop(&ctx->Dx->Control);
    if (status == STATUS_INVALID_DEVICE_STATE) {
        /* Best-effort: treat "already stopped" as success. */
        return STATUS_SUCCESS;
    }
    return status;
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

        ctx->RenderBufferBytes = 0;
        ctx->RenderPeriodBytes = 0;
        ctx->RenderFrameBytes = 0;
        return STATUS_SUCCESS;
    }

    status = VirtioSndCtrlRelease(&ctx->Dx->Control);
    VirtIoSndUninitTxEngine(ctx->Dx);
    ctx->RenderBufferBytes = 0;
    ctx->RenderPeriodBytes = 0;
    ctx->RenderFrameBytes = 0;
    if (status == STATUS_INVALID_DEVICE_STATE) {
        return STATUS_SUCCESS;
    }
    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_WritePeriod(
    _In_ PVOID Context,
    _In_ UINT64 Pcm1DmaAddr,
    _In_ SIZE_T Pcm1Bytes,
    _In_ UINT64 Pcm2DmaAddr,
    _In_ SIZE_T Pcm2Bytes
    )
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG periodBytes;
    SIZE_T totalBytes;
    NTSTATUS status;
    VIRTIOSND_TX_SEGMENT segments[2];
    ULONG segmentCount;

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

    periodBytes = ctx->RenderPeriodBytes;
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

    if (ctx->RenderFrameBytes == 0 || (totalBytes % (SIZE_T)ctx->RenderFrameBytes) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    segmentCount = 0;
    if (Pcm1Bytes != 0) {
        if (Pcm1Bytes > MAXULONG) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
        segments[segmentCount].Address.QuadPart = (LONGLONG)Pcm1DmaAddr;
        segments[segmentCount].Length = (ULONG)Pcm1Bytes;
        segmentCount++;
    }
    if (Pcm2Bytes != 0) {
        if (Pcm2Bytes > MAXULONG) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
        segments[segmentCount].Address.QuadPart = (LONGLONG)Pcm2DmaAddr;
        segments[segmentCount].Length = (ULONG)Pcm2Bytes;
        segmentCount++;
    }

    if (segmentCount == 0) {
        return STATUS_SUCCESS;
    }

    /*
     * Drain completions proactively so small TX buffer pools don't starve.
     *
     * Note: In Aero today TX completions are effectively immediate, but that is not
     * a playback clock; it's just resource reclamation.
     */
    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    if (dx->Tx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    /* Ensure PCM stores are ordered before publishing the TX descriptors. */
    KeMemoryBarrier();

    status = VirtIoSndHwSubmitTxSg(dx, segments, segmentCount);
    if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
        (VOID)VirtIoSndHwDrainTxCompletions(dx);
        if (dx->Tx.FatalError) {
            return STATUS_DEVICE_HARDWARE_ERROR;
        }

        status = VirtIoSndHwSubmitTxSg(dx, segments, segmentCount);
        if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
            /*
             * No buffers available right now.
             *
             * Do not claim success here: the WaveRT miniport uses the return status to decide
             * whether to advance its submission pointer. Returning STATUS_SUCCESS would make
             * the driver skip PCM periods silently, which in turn can lead to host-side wav
             * captures that are entirely silent while guest-side audio APIs appear to succeed.
             */
            return STATUS_DEVICE_BUSY;
        }
    }

    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    if (dx->Tx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }
    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_WritePeriodSg(
    _In_ PVOID Context,
    _In_reads_(SegmentCount) const VIRTIOSND_TX_SEGMENT* Segments,
    _In_ ULONG SegmentCount
    )
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG periodBytes;
    ULONGLONG totalBytes;
    ULONG i;
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

    if (SegmentCount != 0 && Segments == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (SegmentCount > VIRTIOSND_TX_MAX_SEGMENTS) {
        return STATUS_INVALID_PARAMETER;
    }

    periodBytes = ctx->RenderPeriodBytes;
    if (periodBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    totalBytes = 0;
    for (i = 0; i < SegmentCount; ++i) {
        if (Segments[i].Length == 0) {
            return STATUS_INVALID_PARAMETER;
        }
        totalBytes += (ULONGLONG)Segments[i].Length;
        if (totalBytes > (ULONGLONG)MAXULONG) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
    }

    if (totalBytes != (ULONGLONG)periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (ctx->RenderFrameBytes == 0 || (totalBytes % (ULONGLONG)ctx->RenderFrameBytes) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    if (SegmentCount == 0) {
        return STATUS_SUCCESS;
    }

    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    if (dx->Tx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    KeMemoryBarrier();

    status = VirtIoSndHwSubmitTxSg(dx, Segments, SegmentCount);
    if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
        (VOID)VirtIoSndHwDrainTxCompletions(dx);
        if (dx->Tx.FatalError) {
            return STATUS_DEVICE_HARDWARE_ERROR;
        }

        status = VirtIoSndHwSubmitTxSg(dx, Segments, SegmentCount);
        if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
            return STATUS_DEVICE_BUSY;
        }
    }

    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    if (dx->Tx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_WritePeriodCopy(
    _In_ PVOID Context,
    _In_opt_ const VOID *Pcm1,
    _In_ ULONG Pcm1Bytes,
    _In_opt_ const VOID *Pcm2,
    _In_ ULONG Pcm2Bytes,
    _In_ BOOLEAN AllowSilenceFill
    )
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG periodBytes;
    ULONG totalBytes;
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

    periodBytes = ctx->RenderPeriodBytes;
    if (periodBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    totalBytes = Pcm1Bytes + Pcm2Bytes;
    if (totalBytes < Pcm1Bytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (totalBytes != periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (ctx->RenderFrameBytes == 0 || (totalBytes % ctx->RenderFrameBytes) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    if (dx->Tx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    KeMemoryBarrier();

    status = VirtIoSndHwSubmitTx(dx, Pcm1, Pcm1Bytes, Pcm2, Pcm2Bytes, AllowSilenceFill);
    if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
        (VOID)VirtIoSndHwDrainTxCompletions(dx);
        if (dx->Tx.FatalError) {
            return STATUS_DEVICE_HARDWARE_ERROR;
        }

        status = VirtIoSndHwSubmitTx(dx, Pcm1, Pcm1Bytes, Pcm2, Pcm2Bytes, AllowSilenceFill);
        if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
            return STATUS_DEVICE_BUSY;
        }
    }

    (VOID)VirtIoSndHwDrainTxCompletions(dx);
    if (dx->Tx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_SetParamsCapture(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;
    VIRTIOSND_STREAM_STATE streamState;
    ULONG frameBytes;

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

    frameBytes = VirtIoSndBackendVirtioFrameBytesForStream(dx, VIRTIO_SND_CAPTURE_STREAM_ID);
    if (frameBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    /* Capture payloads must be frame-aligned. */
    BufferBytes = (BufferBytes / frameBytes) * frameBytes;
    PeriodBytes = (PeriodBytes / frameBytes) * frameBytes;
    ctx->CaptureFrameBytes = frameBytes;
    if (BufferBytes == 0 || PeriodBytes == 0 || PeriodBytes > BufferBytes) {
        return STATUS_INVALID_PARAMETER;
    }
    if (PeriodBytes > VIRTIOSND_MAX_PCM_PAYLOAD_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (BufferBytes > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if ((BufferBytes % PeriodBytes) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /*
     * SET_PARAMS1 is only valid when the capture stream is Idle/ParamsSet. WaveRT
     * can reallocate buffers while paused, so ensure stream 1 is back in Idle first.
     */
    streamState = dx->Control.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID];
    if (streamState == VirtioSndStreamStateRunning) {
        (VOID)VirtioSndCtrlStop1(&dx->Control);
        streamState = dx->Control.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID];
    }
    if (streamState != VirtioSndStreamStateIdle && streamState != VirtioSndStreamStateParamsSet) {
        (VOID)VirtioSndCtrlRelease1(&dx->Control);
    }

    status = VirtioSndCtrlSetParams1(&dx->Control, BufferBytes, PeriodBytes);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("backend(virtio): SET_PARAMS1 failed: 0x%08X\n", (UINT)status);
        return status;
    }

    /*
     * Initialize the RX engine for capture. Unlike the TX engine, RX request
     * contexts are not period-size dependent.
     */
    if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.FrameBytes != frameBytes) {
        VirtIoSndUninitRxEngine(dx);
    }

    if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) == 0) {
        status = VirtIoSndInitRxEngineEx(dx, frameBytes, VIRTIOSND_QUEUE_SIZE_RXQ);
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("backend(virtio): Rx engine init failed: 0x%08X\n", (UINT)status);
            return status;
        }

        /* Capture completions are timer-polled; suppress rxq interrupts. */
        VirtioSndQueueDisableInterrupts(&dx->Queues[VIRTIOSND_QUEUE_RX]);
    }

    ctx->CaptureBufferBytes = BufferBytes;
    ctx->CapturePeriodBytes = PeriodBytes;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndBackendVirtio_PrepareCapture(_In_ PVOID Context)
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

    return VirtioSndCtrlPrepare1(&ctx->Dx->Control);
}

static NTSTATUS
VirtIoSndBackendVirtio_StartCapture(_In_ PVOID Context)
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

    return VirtioSndCtrlStart1(&ctx->Dx->Control);
}

static NTSTATUS
VirtIoSndBackendVirtio_StopCapture(_In_ PVOID Context)
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
        return STATUS_SUCCESS;
    }

    status = VirtioSndCtrlStop1(&ctx->Dx->Control);
    if (status == STATUS_INVALID_DEVICE_STATE) {
        return STATUS_SUCCESS;
    }
    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_ReleaseCapture(_In_ PVOID Context)
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
         VirtIoSndUninitRxEngine(ctx->Dx);
         ctx->CaptureBufferBytes = 0;
         ctx->CapturePeriodBytes = 0;
         ctx->CaptureFrameBytes = 0;
         return STATUS_SUCCESS;
     }

    status = VirtioSndCtrlRelease1(&ctx->Dx->Control);
    VirtIoSndUninitRxEngine(ctx->Dx);
    ctx->CaptureBufferBytes = 0;
    ctx->CapturePeriodBytes = 0;
    ctx->CaptureFrameBytes = 0;
    if (status == STATUS_INVALID_DEVICE_STATE) {
        return STATUS_SUCCESS;
    }
    return status;
}

static NTSTATUS
VirtIoSndBackendVirtio_SubmitCapturePeriodSg(
    _In_ PVOID Context,
    _In_reads_(SegmentCount) const VIRTIOSND_RX_SEGMENT *Segments,
    _In_ USHORT SegmentCount,
    _In_opt_ void *Cookie)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG periodBytes;
    ULONGLONG totalBytes;
    USHORT i;
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

    periodBytes = ctx->CapturePeriodBytes;
    if (periodBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Segments == NULL || SegmentCount == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    totalBytes = 0;
    for (i = 0; i < SegmentCount; i++) {
        totalBytes += (ULONGLONG)Segments[i].len;
        if (totalBytes > 0xFFFFFFFFull) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
    }

    if (totalBytes != (ULONGLONG)periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (ctx->CaptureFrameBytes == 0 || (totalBytes % (ULONGLONG)ctx->CaptureFrameBytes) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /* Drain completions proactively so the RX request pool doesn't starve. */
    (VOID)VirtIoSndHwDrainRxCompletions(dx, NULL, NULL);
    if (dx->Rx.FatalError) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    status = VirtIoSndHwSubmitRxSg(dx, Segments, SegmentCount, Cookie);
    if (status == STATUS_INSUFFICIENT_RESOURCES || status == STATUS_DEVICE_BUSY) {
        (VOID)VirtIoSndHwDrainRxCompletions(dx, NULL, NULL);
        if (dx->Rx.FatalError) {
            return STATUS_DEVICE_HARDWARE_ERROR;
        }
        status = VirtIoSndHwSubmitRxSg(dx, Segments, SegmentCount, Cookie);
    }

    return status;
}

static ULONG
VirtIoSndBackendVirtio_DrainCaptureCompletions(
    _In_ PVOID Context,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION *Callback,
    _In_opt_ void *CallbackContext)
{
    PVIRTIOSND_BACKEND_VIRTIO ctx;

    ctx = VirtIoSndBackendVirtioFromContext(Context);
    if (ctx == NULL || ctx->Dx == NULL) {
        return 0;
    }

    return VirtIoSndHwDrainRxCompletions(ctx->Dx, Callback, CallbackContext);
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
    VirtIoSndBackendVirtio_WritePeriodSg,
    VirtIoSndBackendVirtio_WritePeriodCopy,
    VirtIoSndBackendVirtio_SetParamsCapture,
    VirtIoSndBackendVirtio_PrepareCapture,
    VirtIoSndBackendVirtio_StartCapture,
    VirtIoSndBackendVirtio_StopCapture,
    VirtIoSndBackendVirtio_ReleaseCapture,
    VirtIoSndBackendVirtio_SubmitCapturePeriodSg,
    VirtIoSndBackendVirtio_DrainCaptureCompletions,
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

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

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

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

/* Bounded reset poll (virtio status reset handshake). */
#define VIRTIOSND_RESET_TIMEOUT_US 1000000u
#define VIRTIOSND_RESET_POLL_DELAY_US 1000u

static __forceinline UCHAR VirtIoSndReadDeviceStatus(_In_ const VIRTIOSND_TRANSPORT *Transport)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->device_status);
}

static __forceinline VOID VirtIoSndWriteDeviceStatus(_In_ const VIRTIOSND_TRANSPORT *Transport, _In_ UCHAR Status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->device_status, Status);
}

static VOID VirtIoSndResetDeviceBestEffort(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    ULONG waitedUs;

    if (Dx->Transport.CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    VirtIoSndWriteDeviceStatus(&Dx->Transport, 0);
    KeMemoryBarrier();

    for (waitedUs = 0; waitedUs < VIRTIOSND_RESET_TIMEOUT_US; waitedUs += VIRTIOSND_RESET_POLL_DELAY_US) {
        if (VirtIoSndReadDeviceStatus(&Dx->Transport) == 0) {
            KeMemoryBarrier();
            return;
        }

        KeStallExecutionProcessor(VIRTIOSND_RESET_POLL_DELAY_US);
    }
}

static VOID VirtIoSndFailDeviceBestEffort(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    UCHAR status;

    if (Dx->Transport.CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    status = VirtIoSndReadDeviceStatus(&Dx->Transport);
    status |= VIRTIO_STATUS_FAILED;
    VirtIoSndWriteDeviceStatus(&Dx->Transport, status);
    KeMemoryBarrier();
}

static VOID VirtIoSndDestroyQueues(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    ULONG i;
    for (i = 0; i < VIRTIOSND_QUEUE_COUNT; ++i) {
        VirtioSndQueueSplitDestroy(&Dx->DmaCtx, &Dx->QueueSplit[i]);
        Dx->Queues[i].Ops = NULL;
        Dx->Queues[i].Ctx = NULL;
    }
}

static NTSTATUS VirtIoSndSetupQueues(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    ULONG q;
    const BOOLEAN eventIdx = (Dx->NegotiatedFeatures & (1ui64 << VIRTIO_F_RING_EVENT_IDX)) != 0;
    const BOOLEAN indirect = (Dx->NegotiatedFeatures & (1ui64 << VIRTIO_F_RING_INDIRECT_DESC)) != 0;

    /*
     * Contract v1 requires four virtqueues (control/event/tx/rx).
     */
    if (Dx->Transport.CommonCfg != NULL) {
        USHORT numQueues;

        numQueues = READ_REGISTER_USHORT((volatile USHORT*)&Dx->Transport.CommonCfg->num_queues);
        if (numQueues < (USHORT)VIRTIOSND_QUEUE_COUNT) {
            VIRTIOSND_TRACE_ERROR(
                "device exposes %u queues (< %u required by contract v1)\n",
                (UINT)numQueues,
                (UINT)VIRTIOSND_QUEUE_COUNT);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
        USHORT size;
        USHORT expectedSize;
        USHORT notifyOff;
        UINT64 descPa, availPa, usedPa;
        USHORT notifyOffReadback;

        size = 0;
        expectedSize = 0;
        notifyOff = 0;
        descPa = 0;
        availPa = 0;
        usedPa = 0;

        switch (q) {
        case VIRTIOSND_QUEUE_CONTROL:
            expectedSize = VIRTIOSND_QUEUE_SIZE_CONTROLQ;
            break;
        case VIRTIOSND_QUEUE_EVENT:
            expectedSize = VIRTIOSND_QUEUE_SIZE_EVENTQ;
            break;
        case VIRTIOSND_QUEUE_TX:
            expectedSize = VIRTIOSND_QUEUE_SIZE_TXQ;
            break;
        case VIRTIOSND_QUEUE_RX:
            expectedSize = VIRTIOSND_QUEUE_SIZE_RXQ;
            break;
        default:
            expectedSize = 0;
            break;
        }

        status = VirtIoSndTransportReadQueueSize(&Dx->Transport, (USHORT)q, &size);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (expectedSize != 0 && size != expectedSize) {
            VIRTIOSND_TRACE_ERROR(
                "queue %lu size mismatch: device=%u expected=%u\n",
                q,
                (UINT)size,
                (UINT)expectedSize);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        status = VirtIoSndTransportReadQueueNotifyOff(&Dx->Transport, (USHORT)q, &notifyOff);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (notifyOff != (USHORT)q) {
            VIRTIOSND_TRACE_ERROR(
                "queue %lu notify_off mismatch: device=%u expected=%lu\n",
                q,
                (UINT)notifyOff,
                q);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        status = VirtioSndQueueSplitCreate(
            &Dx->DmaCtx,
            &Dx->QueueSplit[q],
            (USHORT)q,
            size,
            eventIdx,
            indirect,
            Dx->Transport.NotifyBase,
            Dx->Transport.NotifyOffMultiplier,
            Dx->Transport.NotifyLength,
            notifyOff,
            &Dx->Queues[q],
            &descPa,
            &availPa,
            &usedPa);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        /*
         * Contract v1 requires VIRTIO_F_RING_INDIRECT_DESC. Prefer indirect
         * descriptors (threshold=0) for controlq/txq/rxq as long as an indirect
         * table pool is available.
         */
        if (Dx->QueueSplit[q].Vq != NULL && Dx->QueueSplit[q].Vq->indirect_pool_va != NULL) {
            if (q == VIRTIOSND_QUEUE_CONTROL || q == VIRTIOSND_QUEUE_TX || q == VIRTIOSND_QUEUE_RX) {
                Dx->QueueSplit[q].Vq->indirect_threshold = 0;
            }
        }

        notifyOffReadback = 0;
        status = VirtIoSndTransportSetupQueue(&Dx->Transport, (USHORT)q, descPa, availPa, usedPa, &notifyOffReadback);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (notifyOffReadback != notifyOff) {
            VIRTIOSND_TRACE_ERROR(
                "queue %lu notify_off readback mismatch: init=%u readback=%u\n",
                q,
                (UINT)notifyOff,
                (UINT)notifyOffReadback);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        VIRTIOSND_TRACE("queue %lu enabled (size=%u)\n", q, (UINT)size);

        if (Dx->QueueSplit[q].Vq != NULL) {
            VIRTIOSND_TRACE(
                "queue %lu ring: VA=%p DMA=%I64x bytes=%Iu cache=%s\n",
                q,
                Dx->QueueSplit[q].Ring.Va,
                (ULONGLONG)Dx->QueueSplit[q].Ring.DmaAddr,
                Dx->QueueSplit[q].Ring.Size,
                Dx->QueueSplit[q].Ring.CacheEnabled ? "MmCached" : "MmNonCached");

            VIRTIOSND_TRACE(
                "queue %lu desc VA=%p PA=%I64x | avail VA=%p PA=%I64x | used VA=%p PA=%I64x\n",
                q,
                Dx->QueueSplit[q].Vq->desc,
                (ULONGLONG)Dx->QueueSplit[q].Vq->desc_pa,
                Dx->QueueSplit[q].Vq->avail,
                (ULONGLONG)Dx->QueueSplit[q].Vq->avail_pa,
                Dx->QueueSplit[q].Vq->used,
                (ULONGLONG)Dx->QueueSplit[q].Vq->used_pa);

            VIRTIOSND_TRACE(
                "queue %lu indirect: VA=%p DMA=%I64x bytes=%Iu tables=%u max_desc=%u cache=%s\n",
                q,
                Dx->QueueSplit[q].IndirectPool.Va,
                (ULONGLONG)Dx->QueueSplit[q].IndirectPool.DmaAddr,
                Dx->QueueSplit[q].IndirectPool.Size,
                (UINT)Dx->QueueSplit[q].IndirectTableCount,
                (UINT)Dx->QueueSplit[q].IndirectMaxDesc,
                Dx->QueueSplit[q].IndirectPool.CacheEnabled ? "MmCached" : "MmNonCached");
        }
    }

    return STATUS_SUCCESS;
}
_Use_decl_annotations_
VOID VirtIoSndStopHardware(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS cancelStatus;
    BOOLEAN wasStarted;

    if (Dx == NULL) {
        return;
    }

    /*
     * Stop accepting new TX/control submissions as early as possible. WaveRT's
     * period timer runs independently of the virtio interrupt DPC; dropping this
     * flag up-front prevents racey writes while teardown is in progress.
     */
    wasStarted = Dx->Started;
    Dx->Started = FALSE;

    cancelStatus = Dx->Removed ? STATUS_DEVICE_REMOVED : STATUS_CANCELLED;

    VirtIoSndIntxDisconnect(Dx);

    VirtIoSndResetDeviceBestEffort(Dx);

    /*
     * Cancel and drain protocol operations before teardown so request DMA common
     * buffers are freed while the DMA adapter is still valid.
     *
     * Note: StopHardware is also used as a best-effort cleanup routine on the
     * first START_DEVICE before the control engine has been initialized. Guard
     * against calling Control::Uninit on a zeroed (uninitialized) struct.
     */
    if (Dx->Control.DmaCtx != NULL) {
        if (wasStarted) {
            VirtioSndCtrlCancelAll(&Dx->Control, cancelStatus);
        }
        VirtioSndCtrlUninit(&Dx->Control);
    }

    VirtioSndTxUninit(&Dx->Tx);
    (VOID)InterlockedExchange(&Dx->TxEngineInitialized, 0);

    VirtIoSndRxUninit(&Dx->Rx);

    VirtIoSndDestroyQueues(Dx);

    VirtIoSndDmaUninit(&Dx->DmaCtx);

    VirtIoSndTransportUninit(&Dx->Transport);

    Dx->NegotiatedFeatures = 0;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndStartHardware(
    PVIRTIOSND_DEVICE_EXTENSION Dx,
    PCM_RESOURCE_LIST RawResources,
    PCM_RESOURCE_LIST TranslatedResources)
{
    NTSTATUS status;
    UCHAR devStatus;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtIoSndStopHardware(Dx);

    status = VirtIoSndTransportInit(&Dx->Transport, Dx->LowerDeviceObject, RawResources, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("transport init failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    VIRTIOSND_TRACE(
        "transport: rev=0x%02X bar0=0x%I64x len=0x%I64x notify_mult=%lu\n",
        (UINT)Dx->Transport.PciRevisionId,
        Dx->Transport.Bar0Base,
        (ULONGLONG)Dx->Transport.Bar0Length,
        Dx->Transport.NotifyOffMultiplier);

    status = VirtIoSndTransportNegotiateFeatures(&Dx->Transport, &Dx->NegotiatedFeatures);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("feature negotiation failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    VIRTIOSND_TRACE("features negotiated: 0x%I64x\n", Dx->NegotiatedFeatures);
    status = VirtIoSndDmaInit(Dx->Pdo, &Dx->DmaCtx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndDmaInit failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    status = VirtIoSndIntxCaptureResources(Dx, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to locate INTx resource: 0x%08X\n", (UINT)status);
        goto fail;
    }

    status = VirtIoSndSetupQueues(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("queue setup failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    /* Initialize the protocol engines now that queues are available. */
    VirtioSndCtrlInit(&Dx->Control, &Dx->DmaCtx, &Dx->Queues[VIRTIOSND_QUEUE_CONTROL]);

    RtlZeroMemory(&Dx->Tx, sizeof(Dx->Tx));
    Dx->TxEngineInitialized = 0;

    status = VirtIoSndIntxConnect(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to connect INTx: 0x%08X\n", (UINT)status);
        goto fail;
    }

    KeMemoryBarrier();
    devStatus = VirtIoSndReadDeviceStatus(&Dx->Transport);
    devStatus |= VIRTIO_STATUS_DRIVER_OK;
    VirtIoSndWriteDeviceStatus(&Dx->Transport, devStatus);
    KeMemoryBarrier();

    VIRTIOSND_TRACE("device_status=0x%02X\n", (UINT)VirtIoSndReadDeviceStatus(&Dx->Transport));

    Dx->Started = TRUE;
    return STATUS_SUCCESS;

fail:
    VirtIoSndFailDeviceBestEffort(Dx);
    VirtIoSndStopHardware(Dx);
    return status;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndHwSendControl(
    PVIRTIOSND_DEVICE_EXTENSION Dx,
    const void* Req,
    ULONG ReqLen,
    void* Resp,
    ULONG RespCap,
    ULONG TimeoutMs,
    ULONG* OutVirtioStatus,
    ULONG* OutRespLen)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlSendSync(&Dx->Control, Req, ReqLen, Resp, RespCap, TimeoutMs, OutVirtioStatus, OutRespLen);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndHwSubmitTx(
    PVIRTIOSND_DEVICE_EXTENSION Dx,
    const VOID* Pcm1,
    ULONG Pcm1Bytes,
    const VOID* Pcm2,
    ULONG Pcm2Bytes,
    BOOLEAN AllowSilenceFill)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    /*
     * TX engine initialization (buffer sizing, pool depth) is stream-specific and
     * currently performed by higher layers (WaveRT stream). Fail clearly if a
     * caller attempts to submit before TxInit has run.
     */
    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 || Dx->Tx.Queue == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndTxSubmitPeriod(&Dx->Tx, Pcm1, Pcm1Bytes, Pcm2, Pcm2Bytes, AllowSilenceFill);
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndHwSubmitTxSg(PVIRTIOSND_DEVICE_EXTENSION Dx, const VIRTIOSND_TX_SEGMENT* Segments, ULONG SegmentCount)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 || Dx->Tx.Queue == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndTxSubmitSg(&Dx->Tx, Segments, SegmentCount);
}

_Use_decl_annotations_
ULONG
VirtIoSndHwDrainTxCompletions(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return 0;
    }

    if (Dx->Removed) {
        return 0;
    }

    if (!Dx->Started) {
        return 0;
    }

    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 || Dx->Tx.Queue == NULL) {
        return 0;
    }

    return VirtioSndTxDrainCompletions(&Dx->Tx);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndInitTxEngine(PVIRTIOSND_DEVICE_EXTENSION Dx, ULONG MaxPeriodBytes, ULONG BufferCount, BOOLEAN SuppressInterrupts)
{
    NTSTATUS status;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) != 0) {
#ifdef STATUS_ALREADY_INITIALIZED
        return STATUS_ALREADY_INITIALIZED;
#else
        return STATUS_INVALID_DEVICE_STATE;
#endif
    }

    status = VirtioSndTxInit(&Dx->Tx, &Dx->DmaCtx, &Dx->Queues[VIRTIOSND_QUEUE_TX], MaxPeriodBytes, BufferCount, SuppressInterrupts);
    if (NT_SUCCESS(status)) {
        InterlockedExchange(&Dx->TxEngineInitialized, 1);
    } else {
        RtlZeroMemory(&Dx->Tx, sizeof(Dx->Tx));
        InterlockedExchange(&Dx->TxEngineInitialized, 0);
    }

    return status;
}

_Use_decl_annotations_
VOID VirtIoSndUninitTxEngine(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    LARGE_INTEGER delay;

    if (Dx == NULL) {
        return;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }
    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0) {
        return;
    }

    InterlockedExchange(&Dx->TxEngineInitialized, 0);

    delay.QuadPart = -10 * 1000; /* 1ms */
    while (InterlockedCompareExchange(&Dx->DpcInFlight, 0, 0) != 0) {
        KeDelayExecutionThread(KernelMode, FALSE, &delay);
    }

    VirtioSndTxUninit(&Dx->Tx);
}

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

    for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
        USHORT size;
        USHORT notifyOff;
        UINT64 descPa, availPa, usedPa;
        USHORT notifyOffReadback;

        size = 0;
        notifyOff = 0;
        descPa = 0;
        availPa = 0;
        usedPa = 0;

        status = VirtIoSndTransportReadQueueSize(&Dx->Transport, (USHORT)q, &size);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        status = VirtIoSndTransportReadQueueNotifyOff(&Dx->Transport, (USHORT)q, &notifyOff);
        if (!NT_SUCCESS(status)) {
            return status;
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
            notifyOff,
            &Dx->Queues[q],
            &descPa,
            &availPa,
            &usedPa);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        notifyOffReadback = 0;
        status = VirtIoSndTransportSetupQueue(&Dx->Transport, (USHORT)q, descPa, availPa, usedPa, &notifyOffReadback);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (notifyOffReadback != notifyOff) {
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        VIRTIOSND_TRACE("queue %lu enabled (size=%u)\n", q, (ULONG)size);
    }

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndStopHardware(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    VirtIoSndIntxDisconnect(Dx);

    VirtIoSndResetDeviceBestEffort(Dx);

    VirtioSndTxUninit(&Dx->Tx);

    VirtIoSndDestroyQueues(Dx);

    VirtIoSndDmaUninit(&Dx->DmaCtx);

    VirtIoSndTransportUninit(&Dx->Transport);

    Dx->NegotiatedFeatures = 0;
    Dx->Started = FALSE;
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
        VIRTIOSND_TRACE_ERROR("transport init failed: 0x%08X\n", status);
        goto fail;
    }

    VIRTIOSND_TRACE(
        "transport: rev=0x%02X bar0=0x%I64x len=0x%I64x notify_mult=%lu\n",
        (ULONG)Dx->Transport.PciRevisionId,
        Dx->Transport.Bar0Base,
        (ULONGLONG)Dx->Transport.Bar0Length,
        Dx->Transport.NotifyOffMultiplier);

    status = VirtIoSndTransportNegotiateFeatures(&Dx->Transport, &Dx->NegotiatedFeatures);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("feature negotiation failed: 0x%08X\n", status);
        goto fail;
    }

    VIRTIOSND_TRACE("features negotiated: 0x%I64x\n", Dx->NegotiatedFeatures);
    status = VirtIoSndDmaInit(Dx->Pdo, &Dx->DmaCtx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndDmaInit failed: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndIntxCaptureResources(Dx, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to locate INTx resource: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndSetupQueues(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("queue setup failed: 0x%08X\n", status);
        goto fail;
    }

    VirtioSndCtrlInit(&Dx->Control, &Dx->Queues[VIRTIOSND_QUEUE_CONTROL]);

    status = VirtIoSndIntxConnect(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to connect INTx: 0x%08X\n", status);
        goto fail;
    }

    KeMemoryBarrier();
    devStatus = VirtIoSndReadDeviceStatus(&Dx->Transport);
    devStatus |= VIRTIO_STATUS_DRIVER_OK;
    VirtIoSndWriteDeviceStatus(&Dx->Transport, devStatus);
    KeMemoryBarrier();

    VIRTIOSND_TRACE("device_status=0x%02X\n", (ULONG)VirtIoSndReadDeviceStatus(&Dx->Transport));

    Dx->Started = TRUE;
    return STATUS_SUCCESS;

fail:
    VirtIoSndFailDeviceBestEffort(Dx);
    VirtIoSndStopHardware(Dx);
    return status;
}

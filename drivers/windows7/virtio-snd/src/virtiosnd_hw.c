#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"

/* virtio_pci_isr bits (modern PCI transport). */
#define VIRTIO_PCI_ISR_QUEUE_INTERRUPT 0x01
#define VIRTIO_PCI_ISR_CONFIG_INTERRUPT 0x02

/* Bounded reset poll (virtio status reset handshake). */
#define VIRTIOSND_RESET_TIMEOUT_US 1000000u
#define VIRTIOSND_RESET_POLL_DELAY_US 1000u

static NTSTATUS VirtIoSndParseInterruptResource(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
                                               _In_opt_ PCM_RESOURCE_LIST TranslatedResources);

static BOOLEAN VirtIoSndIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext);
static VOID VirtIoSndDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_ PVOID SystemArgument1, _In_ PVOID SystemArgument2);

static VOID VirtIoSndDisconnectInterrupt(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx->InterruptObject != NULL) {
        IoDisconnectInterrupt(Dx->InterruptObject);
        Dx->InterruptObject = NULL;
        VIRTIOSND_TRACE("INTx disconnected\n");
    }
}

static NTSTATUS VirtIoSndConnectInterrupt(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;

    KeInitializeDpc(&Dx->InterruptDpc, VirtIoSndDpc, Dx);

    Dx->PendingIsrStatus = 0;
    Dx->DpcInFlight = 0;
    KeSetEvent(&Dx->DpcIdleEvent, IO_NO_INCREMENT, FALSE);

    status = IoConnectInterrupt(
        &Dx->InterruptObject,
        VirtIoSndIsr,
        Dx,
        NULL,
        Dx->InterruptVector,
        Dx->InterruptIrql,
        Dx->InterruptIrql,
        Dx->InterruptMode,
        Dx->InterruptShareVector,
        Dx->InterruptAffinity,
        FALSE);

    if (!NT_SUCCESS(status)) {
        Dx->InterruptObject = NULL;
        return status;
    }

    VIRTIOSND_TRACE("INTx connected\n");
    return STATUS_SUCCESS;
}

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

static NTSTATUS VirtIoSndParseInterruptResource(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
                                               _In_opt_ PCM_RESOURCE_LIST TranslatedResources)
{
    ULONG fullIndex;
    ULONG fullCount;

    if (TranslatedResources == NULL || TranslatedResources->Count == 0) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    fullCount = TranslatedResources->Count;

    for (fullIndex = 0; fullIndex < fullCount; ++fullIndex) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc;
        ULONG count;
        ULONG i;

        count = TranslatedResources->List[fullIndex].PartialResourceList.Count;
        desc = TranslatedResources->List[fullIndex].PartialResourceList.PartialDescriptors;

        for (i = 0; i < count; ++i) {
            if (desc[i].Type != CmResourceTypeInterrupt) {
                continue;
            }

            if ((desc[i].Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
                continue;
            }

            Dx->InterruptVector = desc[i].u.Interrupt.Vector;
            Dx->InterruptIrql = (KIRQL)desc[i].u.Interrupt.Level;
            Dx->InterruptAffinity = (KAFFINITY)desc[i].u.Interrupt.Affinity;
            Dx->InterruptMode = (desc[i].Flags & CM_RESOURCE_INTERRUPT_LATCHED) ? Latched : LevelSensitive;
            Dx->InterruptShareVector = (desc[i].ShareDisposition == CmResourceShareDispositionShared) ? TRUE : FALSE;

            VIRTIOSND_TRACE(
                "INTx resource: vector=%lu irql=%lu affinity=%I64x flags=0x%x share=%u\n",
                Dx->InterruptVector,
                (ULONG)Dx->InterruptIrql,
                (ULONGLONG)Dx->InterruptAffinity,
                (ULONG)desc[i].Flags,
                Dx->InterruptShareVector);

            return STATUS_SUCCESS;
        }
    }

    return STATUS_RESOURCE_TYPE_NOT_FOUND;
}

static BOOLEAN VirtIoSndIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    UCHAR isrStatus;
    BOOLEAN inserted;

    UNREFERENCED_PARAMETER(Interrupt);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)ServiceContext;
    if (dx == NULL || dx->Transport.IsrStatus == NULL) {
        return FALSE;
    }

    isrStatus = READ_REGISTER_UCHAR(dx->Transport.IsrStatus);
    if (isrStatus == 0) {
        return FALSE;
    }

    (VOID)InterlockedOr(&dx->PendingIsrStatus, (LONG)isrStatus);

    if (InterlockedCompareExchange(&dx->Stopping, 0, 0) != 0) {
        return TRUE;
    }

    inserted = KeInsertQueueDpc(&dx->InterruptDpc, NULL, NULL);
    if (inserted) {
        if (InterlockedIncrement(&dx->DpcInFlight) == 1) {
            KeClearEvent(&dx->DpcIdleEvent);
        }
    }

    return TRUE;
}

static VOID VirtIoSndDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_ PVOID SystemArgument1, _In_ PVOID SystemArgument2)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    LONG pending;
    ULONG q;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)DeferredContext;
    if (dx == NULL) {
        return;
    }

    pending = InterlockedExchange(&dx->PendingIsrStatus, 0);

    if (InterlockedCompareExchange(&dx->Stopping, 0, 0) == 0) {
        if ((pending & VIRTIO_PCI_ISR_QUEUE_INTERRUPT) != 0) {
            for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
                VOID *cookie;
                UINT32 usedLen;

                if (dx->Queues[q].Ops == NULL) {
                    continue;
                }

                while (VirtioSndQueuePopUsed(&dx->Queues[q], &cookie, &usedLen)) {
                    UNREFERENCED_PARAMETER(cookie);
                    UNREFERENCED_PARAMETER(usedLen);
                }
            }
        } else if ((pending & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0) {
            /* Config-change not handled yet; ISR read already ACKed the interrupt. */
        }
    }

    if (InterlockedDecrement(&dx->DpcInFlight) == 0) {
        KeSetEvent(&dx->DpcIdleEvent, IO_NO_INCREMENT, FALSE);
    }
}

_Use_decl_annotations_
VOID VirtIoSndStopHardware(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    InterlockedExchange(&Dx->Stopping, 1);

    VirtIoSndDisconnectInterrupt(Dx);
    if (KeRemoveQueueDpc(&Dx->InterruptDpc)) {
        if (InterlockedDecrement(&Dx->DpcInFlight) == 0) {
            KeSetEvent(&Dx->DpcIdleEvent, IO_NO_INCREMENT, FALSE);
        }
    }

    (VOID)KeWaitForSingleObject(&Dx->DpcIdleEvent, Executive, KernelMode, FALSE, NULL);
    Dx->PendingIsrStatus = 0;
    Dx->DpcInFlight = 0;

    VirtIoSndResetDeviceBestEffort(Dx);

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
    InterlockedExchange(&Dx->Stopping, 0);

    status = VirtIoSndTransportInit(&Dx->Transport, Dx->LowerDeviceObject, RawResources, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("transport init failed: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndTransportNegotiateFeatures(&Dx->Transport, &Dx->NegotiatedFeatures);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("feature negotiation failed: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndDmaInit(Dx->Pdo, &Dx->DmaCtx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndDmaInit failed: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndParseInterruptResource(Dx, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to locate INTx resource: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndSetupQueues(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("queue setup failed: 0x%08X\n", status);
        goto fail;
    }

    status = VirtIoSndConnectInterrupt(Dx);
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

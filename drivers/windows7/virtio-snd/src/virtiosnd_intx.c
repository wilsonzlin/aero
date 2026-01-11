#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

static __forceinline BOOLEAN VirtIoSndIntxStopping(_In_ const PVIRTIOSND_DEVICE_EXTENSION Dx) {
    return (Dx->Stopping != 0) ? TRUE : FALSE;
}

_Use_decl_annotations_
VOID VirtIoSndIntxInitialize(PVIRTIOSND_DEVICE_EXTENSION Dx) {
    if (Dx == NULL) {
        return;
    }

    Dx->InterruptVector = 0;
    Dx->InterruptIrql = 0;
    Dx->InterruptMode = LevelSensitive;
    Dx->InterruptAffinity = 0;
    Dx->InterruptShareVector = TRUE;

    Dx->InterruptObject = NULL;
    Dx->PendingIsrStatus = 0;

    // Until START_DEVICE wires everything up, treat the device as "stopping" so
    // spurious interrupts can't queue work against uninitialized queues.
    Dx->Stopping = 1;
    Dx->DpcInFlight = 0;
    KeInitializeEvent(&Dx->DpcIdleEvent, NotificationEvent, TRUE);

    KeInitializeDpc(&Dx->InterruptDpc, VirtIoSndIntxDpc, Dx);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndIntxCaptureResources(PVIRTIOSND_DEVICE_EXTENSION Dx, PCM_RESOURCE_LIST TranslatedResources) {
    ULONG listIndex;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    Dx->InterruptVector = 0;
    Dx->InterruptIrql = 0;
    Dx->InterruptMode = LevelSensitive;
    Dx->InterruptAffinity = 0;
    Dx->InterruptShareVector = TRUE;

    if (TranslatedResources == NULL || TranslatedResources->Count == 0) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    for (listIndex = 0; listIndex < TranslatedResources->Count; ++listIndex) {
        PCM_FULL_RESOURCE_DESCRIPTOR full = &TranslatedResources->List[listIndex];
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = full->PartialResourceList.PartialDescriptors;
        ULONG count = full->PartialResourceList.Count;
        ULONG i;

        for (i = 0; i < count; ++i) {
            if (desc[i].Type != CmResourceTypeInterrupt) {
                continue;
            }

            // Contract v1 requires line-based INTx; ignore message-signaled interrupts.
            if ((desc[i].Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
                continue;
            }

            Dx->InterruptVector = desc[i].u.Interrupt.Vector;
            Dx->InterruptIrql = (KIRQL)desc[i].u.Interrupt.Level;
            Dx->InterruptAffinity = desc[i].u.Interrupt.Affinity;
            Dx->InterruptMode = ((desc[i].Flags & CM_RESOURCE_INTERRUPT_LATCHED) != 0) ? Latched : LevelSensitive;
            Dx->InterruptShareVector = (desc[i].ShareDisposition == CmResourceShareDispositionShared) ? TRUE : FALSE;

            VIRTIOSND_TRACE(
                "INTx resource: vector=%lu level=%lu affinity=%I64x mode=%s share=%u\n",
                Dx->InterruptVector,
                (ULONG)Dx->InterruptIrql,
                (ULONGLONG)Dx->InterruptAffinity,
                (Dx->InterruptMode == Latched) ? "latched" : "level",
                Dx->InterruptShareVector);

            return STATUS_SUCCESS;
        }
    }

    return STATUS_RESOURCE_TYPE_NOT_FOUND;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndIntxConnect(PVIRTIOSND_DEVICE_EXTENSION Dx) {
    NTSTATUS status;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->InterruptIrql == 0) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    if (Dx->Transport.IsrStatus == NULL) {
        // Without the ISR register mapping, an INTx interrupt would be impossible
        // to acknowledge/deassert and would result in an interrupt storm.
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Dx->InterruptObject != NULL) {
        return STATUS_ALREADY_REGISTERED;
    }

    Dx->PendingIsrStatus = 0;
    Dx->Stopping = 0;
    Dx->DpcInFlight = 0;
    KeSetEvent(&Dx->DpcIdleEvent, IO_NO_INCREMENT, FALSE);

    status = IoConnectInterrupt(
        &Dx->InterruptObject,
        VirtIoSndIntxIsr,
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
        Dx->Stopping = 1;
        VIRTIOSND_TRACE_ERROR("IoConnectInterrupt failed: 0x%08X\n", status);
        return status;
    }

    VIRTIOSND_TRACE("INTx connected\n");
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndIntxDisconnect(PVIRTIOSND_DEVICE_EXTENSION Dx) {
    if (Dx == NULL) {
        return;
    }

    // Prevent any new queue work from being queued/processed.
    Dx->Stopping = 1;

    if (Dx->InterruptObject != NULL) {
        IoDisconnectInterrupt(Dx->InterruptObject);
        Dx->InterruptObject = NULL;
    }

    // Cancel any DPC that is queued but not yet running.
    (VOID)KeRemoveQueueDpc(&Dx->InterruptDpc);

    // Wait for any in-flight DPC to finish before callers unmap MMIO/free queues.
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        (VOID)KeWaitForSingleObject(&Dx->DpcIdleEvent, Executive, KernelMode, FALSE, NULL);
    } else {
        VIRTIOSND_TRACE_ERROR("VirtIoSndIntxDisconnect called at IRQL %lu; skipping DPC idle wait\n", (ULONG)KeGetCurrentIrql());
    }

    (VOID)InterlockedExchange(&Dx->PendingIsrStatus, 0);
}

_Use_decl_annotations_
BOOLEAN VirtIoSndIntxIsr(PKINTERRUPT Interrupt, PVOID ServiceContext) {
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)ServiceContext;
    BOOLEAN stopping;
    UCHAR isr;

    UNREFERENCED_PARAMETER(Interrupt);

    if (dx == NULL || dx->Transport.IsrStatus == NULL) {
        return FALSE;
    }

    stopping = VirtIoSndIntxStopping(dx);

    // Read-to-ack: deassert the level-triggered INTx line.
    isr = READ_REGISTER_UCHAR(dx->Transport.IsrStatus);
    if (isr == 0) {
        return FALSE; // shared interrupt, not ours
    }

    if (stopping) {
        // Device is tearing down; don't queue DPC work against freed queues.
        return TRUE;
    }

    InterlockedOr(&dx->PendingIsrStatus, (LONG)isr);
    (VOID)KeInsertQueueDpc(&dx->InterruptDpc, NULL, NULL);
    return TRUE;
}

_Use_decl_annotations_
VOID VirtIoSndIntxDpc(PKDPC Dpc, PVOID DeferredContext, PVOID SystemArgument1, PVOID SystemArgument2) {
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)DeferredContext;
    LONG active;
    LONG isr;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (dx == NULL) {
        return;
    }

    active = InterlockedIncrement(&dx->DpcInFlight);
    if (active == 1) {
        KeClearEvent(&dx->DpcIdleEvent);
    }

    isr = InterlockedExchange(&dx->PendingIsrStatus, 0);

    if (VirtIoSndIntxStopping(dx)) {
        goto Exit;
    }

    if ((isr & VIRTIOSND_ISR_CONFIG) != 0) {
        VIRTIOSND_TRACE("INTx DPC: config interrupt (unhandled)\n");
    }

    if ((isr & VIRTIOSND_ISR_QUEUE) != 0) {
        /*
         * Drain used rings. Route completions to protocol engines when
         * initialized so cookies are not leaked.
         */
        VirtioSndCtrlProcessUsed(&dx->Control);
        VirtioSndTxProcessCompletions(&dx->Tx);

        /*
         * eventq is device->driver notifications; we do not submit receive
         * buffers yet, so there should be no used entries. Drain defensively in
         * case a future path does submit buffers.
         */
        if (dx->Queues[VIRTIOSND_QUEUE_EVENT].Ops != NULL) {
            VOID* cookie;
            UINT32 usedLen;

            while (VirtioSndQueuePopUsed(&dx->Queues[VIRTIOSND_QUEUE_EVENT], &cookie, &usedLen)) {
                UNREFERENCED_PARAMETER(cookie);
                UNREFERENCED_PARAMETER(usedLen);
            }
        }
    }

Exit:
    if (InterlockedDecrement(&dx->DpcInFlight) == 0) {
        KeSetEvent(&dx->DpcIdleEvent, IO_NO_INCREMENT, FALSE);
    }
}

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004
#endif

static __forceinline BOOLEAN VirtIoSndIntxStopping(_In_ const PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    return (Dx->Stopping != 0) ? TRUE : FALSE;
}

static VOID VirtIoSndIntxQueueUsed(
    _In_ USHORT QueueIndex,
    _In_opt_ void* Cookie,
    _In_ UINT32 UsedLen,
    _In_opt_ void* Context)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Context;
    if (dx == NULL) {
        return;
    }

    switch (QueueIndex) {
    case VIRTIOSND_QUEUE_CONTROL:
        VirtioSndCtrlOnUsed(&dx->Control, Cookie, UsedLen);
        break;
    case VIRTIOSND_QUEUE_TX:
        VirtioSndTxOnUsed(&dx->Tx, Cookie, UsedLen);
        break;
    case VIRTIOSND_QUEUE_RX:
        VIRTIOSND_TRACE_ERROR("rxq unexpected completion: cookie=%p len=%lu\n", Cookie, (ULONG)UsedLen);
        if (Cookie != NULL) {
            ExFreePool(Cookie);
        }
        break;
    default:
        UNREFERENCED_PARAMETER(Cookie);
        UNREFERENCED_PARAMETER(UsedLen);
        break;
    }
}

_Use_decl_annotations_
VOID VirtIoSndIntxInitialize(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
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

    KeInitializeDpc(&Dx->InterruptDpc, VirtIoSndIntxDpc, Dx);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndIntxCaptureResources(PVIRTIOSND_DEVICE_EXTENSION Dx, PCM_RESOURCE_LIST TranslatedResources)
{
    ULONG listIndex;
    BOOLEAN sawMessageInterrupt;

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

    sawMessageInterrupt = FALSE;

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
                sawMessageInterrupt = TRUE;
                continue;
            }

            Dx->InterruptVector = desc[i].u.Interrupt.Vector;
            Dx->InterruptIrql = (KIRQL)desc[i].u.Interrupt.Level;
            Dx->InterruptAffinity = desc[i].u.Interrupt.Affinity;
            Dx->InterruptMode = ((desc[i].Flags & CM_RESOURCE_INTERRUPT_LATCHED) != 0) ? Latched : LevelSensitive;
            Dx->InterruptShareVector = TRUE;
#if defined(CmResourceShareShared)
            Dx->InterruptShareVector = (desc[i].ShareDisposition == CmResourceShareShared) ? TRUE : FALSE;
#elif defined(CmShareShared)
            Dx->InterruptShareVector = (desc[i].ShareDisposition == CmShareShared) ? TRUE : FALSE;
#else
            /* Assume shared if headers do not expose the share disposition constants. */
            Dx->InterruptShareVector = TRUE;
#endif

            VIRTIOSND_TRACE(
                "INTx resource: vector=%lu level=%lu affinity=%I64x mode=%s share=%u\n",
                Dx->InterruptVector,
                (ULONG)Dx->InterruptIrql,
                (ULONGLONG)Dx->InterruptAffinity,
                (Dx->InterruptMode == Latched) ? "latched" : "level",
                (UINT)Dx->InterruptShareVector);

            return STATUS_SUCCESS;
        }
    }

    return sawMessageInterrupt ? STATUS_NOT_SUPPORTED : STATUS_RESOURCE_TYPE_NOT_FOUND;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndIntxConnect(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
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

    status = IoConnectInterrupt(&Dx->InterruptObject,
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
        VIRTIOSND_TRACE_ERROR("IoConnectInterrupt failed: 0x%08X\n", (UINT)status);
        return status;
    }

    VIRTIOSND_TRACE("INTx connected\n");
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndIntxDisconnect(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    BOOLEAN removed;
    LARGE_INTEGER delay;

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
    removed = KeRemoveQueueDpc(&Dx->InterruptDpc);
    if (removed) {
        /*
         * The queued DPC was removed and will never run, so decrement the DPC
         * in-flight counter here to keep teardown synchronization correct.
         *
         * Note: a DPC instance may still be *running* concurrently (a KDPC can be
         * re-queued while executing). We still wait for DpcInFlight to drain below.
         */
        LONG remaining = InterlockedDecrement(&Dx->DpcInFlight);
        if (remaining < 0) {
            (VOID)InterlockedExchange(&Dx->DpcInFlight, 0);
        }
    }

    // Wait for any in-flight DPC to finish before callers unmap MMIO/free queues.
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        delay.QuadPart = -10 * 1000; /* 1ms */
        while (InterlockedCompareExchange(&Dx->DpcInFlight, 0, 0) != 0) {
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    } else {
        VIRTIOSND_TRACE_ERROR(
            "VirtIoSndIntxDisconnect called at IRQL %lu; skipping DPC idle wait\n",
            (ULONG)KeGetCurrentIrql());
    }

    (VOID)InterlockedExchange(&Dx->PendingIsrStatus, 0);
    (VOID)InterlockedExchange(&Dx->DpcInFlight, 0);
}

_Use_decl_annotations_
BOOLEAN VirtIoSndIntxIsr(PKINTERRUPT Interrupt, PVOID ServiceContext)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)ServiceContext;
    BOOLEAN stopping;
    UCHAR isr;
    BOOLEAN inserted;

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
    inserted = KeInsertQueueDpc(&dx->InterruptDpc, NULL, NULL);
    if (inserted) {
        /*
         * DpcInFlight tracks both queued and running DPC instances so teardown can
         * safely wait even if the DPC has been dequeued for execution but has not
         * started running yet (KeRemoveQueueDpc would return FALSE in that state).
         */
        (VOID)InterlockedIncrement(&dx->DpcInFlight);
    }
    return TRUE;
}

_Use_decl_annotations_
VOID VirtIoSndIntxDpc(PKDPC Dpc, PVOID DeferredContext, PVOID SystemArgument1, PVOID SystemArgument2)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)DeferredContext;
    LONG isr;
    LONG remaining;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (dx == NULL) {
        return;
    }

    isr = InterlockedExchange(&dx->PendingIsrStatus, 0);
    if (!VirtIoSndIntxStopping(dx)) {
        if ((isr & VIRTIOSND_ISR_CONFIG) != 0) {
            VIRTIOSND_TRACE("INTx DPC: config interrupt (unhandled)\n");
        }

        if ((isr & VIRTIOSND_ISR_QUEUE) != 0) {
            /* INTx doesn't identify which queue fired; drain all configured queues. */
            VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_CONTROL], VirtIoSndIntxQueueUsed, dx);
            VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_EVENT], VirtIoSndIntxQueueUsed, dx);
            VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_TX], VirtIoSndIntxQueueUsed, dx);
            VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_RX], VirtIoSndIntxQueueUsed, dx);
        }
    }

    remaining = InterlockedDecrement(&dx->DpcInFlight);
    if (remaining <= 0) {
        /*
         * Ensure the in-flight count is always non-negative even if DpcInFlight
         * gets out of sync due to unexpected callers.
         */
        if (remaining < 0) {
            (VOID)InterlockedExchange(&dx->DpcInFlight, 0);
        }
    }
}

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_intx_wdm.h"

static VOID
VirtioIntxDpcRoutine(_In_ KDPC *Dpc, _In_ PVOID DeferredContext, _In_ PVOID SystemArgument1, _In_ PVOID SystemArgument2)
{
    VIRTIO_INTX_WDM *intx = (VIRTIO_INTX_WDM *)DeferredContext;
    LONG pending;
    UCHAR isr;
    LONG remaining;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (intx == NULL) {
        return;
    }

    InterlockedIncrement(&intx->DpcCount);

    pending = InterlockedExchange(&intx->PendingIsrStatus, 0);
    isr = (UCHAR)(pending & 0xFF);

    if (intx->Stopping == 0) {
        if ((isr & VIRTIO_PCI_ISR_CONFIG) != 0 && intx->EvtConfigDpc != NULL) {
            intx->EvtConfigDpc(intx->EvtConfigDpcContext);
        }

        if ((isr & VIRTIO_PCI_ISR_QUEUE) != 0 && intx->EvtQueueDpc != NULL) {
            intx->EvtQueueDpc(intx->EvtQueueDpcContext);
        }
    }

    remaining = InterlockedDecrement(&intx->DpcInFlight);
    if (remaining <= 0) {
        /* Ensure the in-flight count is always non-negative. */
        if (remaining < 0) {
            (VOID)InterlockedExchange(&intx->DpcInFlight, 0);
        }
    }
}

static BOOLEAN
VirtioIntxIsrRoutine(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext)
{
    VIRTIO_INTX_WDM *intx = (VIRTIO_INTX_WDM *)ServiceContext;
    UCHAR isr;
    BOOLEAN inserted;

    UNREFERENCED_PARAMETER(Interrupt);

    if (intx == NULL || intx->IsrStatus == NULL) {
        return FALSE;
    }

    /*
     * Read-to-ack: deasserts the INTx line. If this is a shared line and the
     * returned status is 0, the interrupt was not for us.
     */
    isr = READ_REGISTER_UCHAR((PUCHAR)intx->IsrStatus);
    if (isr == 0) {
        return FALSE;
    }

    intx->LastIsrStatus = isr;
    InterlockedIncrement(&intx->IsrCount);

    if (intx->Stopping != 0) {
        /*
         * Device is tearing down; don't queue DPC work against resources that may
         * already be freed/unmapped.
         */
        return TRUE;
    }

    InterlockedOr(&intx->PendingIsrStatus, (LONG)isr);

    /* Coalesce multiple ISR invocations into a single DPC run. */
    inserted = KeInsertQueueDpc(&intx->Dpc, NULL, NULL);
    if (inserted) {
        /*
         * DpcInFlight tracks both queued and running DPC instances so teardown can
         * safely wait even if the DPC is re-queued while executing.
         */
        (VOID)InterlockedIncrement(&intx->DpcInFlight);
    }
    return TRUE;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioIntxConnect(_In_ PDEVICE_OBJECT DeviceObject,
                  _In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *InterruptDescTranslated,
                  _In_ volatile UCHAR *IsrStatusMmio,
                  _In_opt_ EVT_VIRTIO_INTX_WDM_QUEUE_DPC *EvtQueueDpc,
                  _In_opt_ PVOID EvtQueueDpcContext,
                  _In_opt_ EVT_VIRTIO_INTX_WDM_CONFIG_DPC *EvtConfigDpc,
                  _In_opt_ PVOID EvtConfigDpcContext,
                   _Out_ VIRTIO_INTX_WDM *Intx)
{
    NTSTATUS status;
    BOOLEAN shareVector;
    KINTERRUPT_MODE mode;
    KIRQL irql;
    ULONG vector;
    KAFFINITY affinity;

    if (DeviceObject == NULL || InterruptDescTranslated == NULL || IsrStatusMmio == NULL || Intx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Intx, sizeof(*Intx));

    Intx->IsrStatus = IsrStatusMmio;
    Intx->PendingIsrStatus = 0;
    Intx->Stopping = 0;
    Intx->DpcInFlight = 0;
    Intx->IsrCount = 0;
    Intx->DpcCount = 0;
    Intx->LastIsrStatus = 0;

    Intx->EvtQueueDpc = EvtQueueDpc;
    Intx->EvtQueueDpcContext = EvtQueueDpcContext;
    Intx->EvtConfigDpc = EvtConfigDpc;
    Intx->EvtConfigDpcContext = EvtConfigDpcContext;

    KeInitializeDpc(&Intx->Dpc, VirtioIntxDpcRoutine, Intx);

    UNREFERENCED_PARAMETER(DeviceObject);

    vector = InterruptDescTranslated->u.Interrupt.Vector;
    irql = (KIRQL)InterruptDescTranslated->u.Interrupt.Level;
    affinity = InterruptDescTranslated->u.Interrupt.Affinity;

    shareVector = TRUE;
#if defined(CmShareShared)
    shareVector = (InterruptDescTranslated->ShareDisposition == CmShareShared) ? TRUE : FALSE;
#elif defined(CmResourceShareShared)
    shareVector = (InterruptDescTranslated->ShareDisposition == CmResourceShareShared) ? TRUE : FALSE;
#else
    /* Assume shared if headers do not expose the share disposition constants. */
    shareVector = TRUE;
#endif
    mode = (InterruptDescTranslated->Flags & CM_RESOURCE_INTERRUPT_LATCHED) ? Latched : LevelSensitive;

    status = IoConnectInterrupt(&Intx->InterruptObject,
                               VirtioIntxIsrRoutine,
                               Intx,
                               NULL,
                               vector,
                               irql,
                               irql,
                               mode,
                               shareVector,
                               affinity,
                               FALSE);
    if (!NT_SUCCESS(status)) {
        RtlZeroMemory(Intx, sizeof(*Intx));
        return status;
    }

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioIntxDisconnect(_Inout_ VIRTIO_INTX_WDM *Intx)
{
    LARGE_INTEGER delay;

    if (Intx == NULL) {
        return;
    }

    /*
     * Allow callers to unconditionally call VirtioIntxDisconnect() during PnP
     * teardown even when INTx was never connected (e.g. start failure).
     * Avoid touching the KDPC/KEVENT fields unless Connect initialized them.
     */
    if (Intx->InterruptObject == NULL && Intx->IsrStatus == NULL) {
        return;
    }

    /* Prevent any new queue work from being queued/processed. */
    (VOID)InterlockedExchange(&Intx->Stopping, 1);

    if (Intx->InterruptObject != NULL) {
        IoDisconnectInterrupt(Intx->InterruptObject);
        Intx->InterruptObject = NULL;
    }

    /* Cancel any DPC that is queued but not yet running. */
    if (KeRemoveQueueDpc(&Intx->Dpc)) {
        LONG remaining = InterlockedDecrement(&Intx->DpcInFlight);
        if (remaining < 0) {
            (VOID)InterlockedExchange(&Intx->DpcInFlight, 0);
        }
    }

    /* Wait for any in-flight DPC to finish before callers unmap MMIO/free queues. */
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        delay.QuadPart = -10 * 1000; /* 1ms */
        while (InterlockedCompareExchange(&Intx->DpcInFlight, 0, 0) != 0) {
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    } else {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
    }

    Intx->IsrStatus = NULL;
    Intx->PendingIsrStatus = 0;
    Intx->Stopping = 1;
    (VOID)InterlockedExchange(&Intx->DpcInFlight, 0);

    Intx->EvtQueueDpc = NULL;
    Intx->EvtQueueDpcContext = NULL;
    Intx->EvtConfigDpc = NULL;
    Intx->EvtConfigDpcContext = NULL;
}

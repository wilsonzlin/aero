/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_intx_wdm.h"

static BOOLEAN VirtioIntxIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext);
static VOID VirtioIntxDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2);

static __forceinline KINTERRUPT_MODE VirtioIntxInterruptModeFromDescriptor(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR* Desc)
{
    return ((Desc->Flags & CM_RESOURCE_INTERRUPT_LATCHED) != 0) ? Latched : LevelSensitive;
}

static __forceinline BOOLEAN VirtioIntxShareVectorFromDescriptor(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR* Desc)
{
    return (Desc->ShareDisposition == CmResourceShareShared) ? TRUE : FALSE;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioIntxConnect(_In_ PDEVICE_OBJECT DeviceObject,
                           _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDescTranslated,
                           _In_opt_ volatile UCHAR* IsrStatusRegister,
                           _In_opt_ EVT_VIRTIO_INTX_CONFIG_CHANGE* EvtConfigChange,
                           _In_opt_ EVT_VIRTIO_INTX_QUEUE_WORK* EvtQueueWork,
                           _In_opt_ EVT_VIRTIO_INTX_DPC* EvtDpc,
                           _In_opt_ PVOID Cookie,
                           _Inout_ PVIRTIO_INTX Intx)
{
    NTSTATUS status;
    KINTERRUPT_MODE interruptMode;
    BOOLEAN shareVector;
    ULONG vector;
    KIRQL irql;
    KAFFINITY affinity;

    UNREFERENCED_PARAMETER(DeviceObject);

    if (Intx == NULL || InterruptDescTranslated == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (InterruptDescTranslated->Type != CmResourceTypeInterrupt) {
        return STATUS_INVALID_PARAMETER;
    }

    if ((InterruptDescTranslated->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
        return STATUS_NOT_SUPPORTED;
    }

    RtlZeroMemory(Intx, sizeof(*Intx));

    Intx->IsrStatusRegister = IsrStatusRegister;
    Intx->EvtConfigChange = EvtConfigChange;
    Intx->EvtQueueWork = EvtQueueWork;
    Intx->EvtDpc = EvtDpc;
    Intx->Cookie = Cookie;

    Intx->DpcInFlight = 0;

    KeInitializeDpc(&Intx->Dpc, VirtioIntxDpc, Intx);
    Intx->Initialized = TRUE;

    interruptMode = VirtioIntxInterruptModeFromDescriptor(InterruptDescTranslated);
    shareVector = VirtioIntxShareVectorFromDescriptor(InterruptDescTranslated);

    vector = InterruptDescTranslated->u.Interrupt.Vector;
    irql = (KIRQL)InterruptDescTranslated->u.Interrupt.Level;
    affinity = (KAFFINITY)InterruptDescTranslated->u.Interrupt.Affinity;

    status = IoConnectInterrupt(&Intx->InterruptObject,
                                VirtioIntxIsr,
                                Intx,
                                NULL,
                                vector,
                                irql,
                                irql,
                                interruptMode,
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
VOID VirtioIntxDisconnect(_Inout_ PVIRTIO_INTX Intx)
{
    BOOLEAN removed;
    LONG remaining;
    LARGE_INTEGER delay;

    if (Intx == NULL) {
        return;
    }

    /*
     * Allow callers to unconditionally call VirtioIntxDisconnect() during PnP
     * teardown even when INTx was never connected (e.g. start failure).
     */
    if (!Intx->Initialized) {
        RtlZeroMemory(Intx, sizeof(*Intx));
        return;
    }

    /* Ensure any late-running DPC does not call back into the driver. */
    Intx->EvtConfigChange = NULL;
    Intx->EvtQueueWork = NULL;
    Intx->EvtDpc = NULL;
    Intx->Cookie = NULL;

    if (Intx->InterruptObject != NULL) {
        IoDisconnectInterrupt(Intx->InterruptObject);
        Intx->InterruptObject = NULL;
    }

    /* Cancel any DPC that is queued but not yet running. */
    removed = KeRemoveQueueDpc(&Intx->Dpc);
    if (removed) {
        remaining = InterlockedDecrement(&Intx->DpcInFlight);
        if (remaining < 0) {
            (VOID)InterlockedExchange(&Intx->DpcInFlight, 0);
        }
    }

    /*
     * Wait for any in-flight DPC to finish before callers unmap MMIO/free queues.
     * (DpcInFlight tracks both queued and running DPC instances.)
     */
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        delay.QuadPart = -10 * 1000; /* 1ms */
        for (;;) {
            remaining = InterlockedCompareExchange(&Intx->DpcInFlight, 0, 0);
            if (remaining <= 0) {
                if (remaining < 0) {
                    (VOID)InterlockedExchange(&Intx->DpcInFlight, 0);
                }
                break;
            }

            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    } else {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        /*
         * Can't wait at elevated IRQL. Intx remains partially initialized so the
         * KDPC stays valid if still running.
         */
        return;
    }

    RtlZeroMemory(Intx, sizeof(*Intx));
}

/*
 * PKSERVICE_ROUTINE
 *
 * For virtio-pci modern INTx, reading the ISR status register is the
 * acknowledge/deassert operation. This read must happen as early as possible to
 * avoid keeping the line asserted and retriggering/level-storming.
 */
static BOOLEAN VirtioIntxIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext)
{
    PVIRTIO_INTX intx;
    volatile UCHAR* isr;
    UCHAR isrStatus;
    BOOLEAN inserted;

    UNREFERENCED_PARAMETER(Interrupt);

    intx = (PVIRTIO_INTX)ServiceContext;
    if (intx == NULL) {
        return FALSE;
    }

    isr = intx->IsrStatusRegister;
    if (isr == NULL) {
        return FALSE;
    }

    /* First MMIO operation: ACK/deassert INTx by reading the virtio ISR byte (read-to-clear). */
    isrStatus = READ_REGISTER_UCHAR(isr);
    if (isrStatus == 0) {
        (VOID)InterlockedIncrement(&intx->SpuriousCount);
        return FALSE; /* shared interrupt: not ours */
    }

    (VOID)InterlockedIncrement(&intx->IsrCount);

    (VOID)InterlockedOr(&intx->PendingIsrStatus, (LONG)isrStatus);

    inserted = KeInsertQueueDpc(&intx->Dpc, NULL, NULL);
    if (inserted) {
        (VOID)InterlockedIncrement(&intx->DpcInFlight);
    }

    return TRUE;
}

/*
 * PKDEFERRED_ROUTINE
 *
 * Runs at DISPATCH_LEVEL.
 */
static VOID VirtioIntxDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2)
{
    PVIRTIO_INTX intx;
    LONG pending;
    UCHAR isrStatus;
    LONG remaining;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    intx = (PVIRTIO_INTX)DeferredContext;
    if (intx == NULL) {
        return;
    }

    (VOID)InterlockedIncrement(&intx->DpcCount);

    pending = InterlockedExchange(&intx->PendingIsrStatus, 0);
    isrStatus = (UCHAR)pending;

    if (isrStatus != 0) {
        if (intx->EvtDpc != NULL) {
            intx->EvtDpc(intx, isrStatus, intx->Cookie);
        } else {
            if (((isrStatus & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0) && (intx->EvtConfigChange != NULL)) {
                intx->EvtConfigChange(intx, intx->Cookie);
            }

            if (((isrStatus & VIRTIO_PCI_ISR_QUEUE_INTERRUPT) != 0) && (intx->EvtQueueWork != NULL)) {
                intx->EvtQueueWork(intx, intx->Cookie);
            }
        }
    }

    remaining = InterlockedDecrement(&intx->DpcInFlight);
    if (remaining < 0) {
        (VOID)InterlockedExchange(&intx->DpcInFlight, 0);
    }
}


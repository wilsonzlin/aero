/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_interrupts_wdm.h"

/*
 * Pool tags are traditionally specified as multi-character constants (e.g. 'tInV')
 * in WDK codebases. Host-side unit tests build this file with GCC/Clang, which
 * warn on multi-character character constants.
 *
 * Define the tag via a portable shift-based encoding for non-MSVC builds to
 * avoid -Wmultichar noise.
 */
#if defined(_MSC_VER)
#define VIRTIO_PCI_WDM_INT_TAG 'tInV'
#else
#define VIRTIO_PCI_WDM_MAKE_POOL_TAG(a, b, c, d) \
    ((ULONG)(((ULONG)(a) << 24) | ((ULONG)(b) << 16) | ((ULONG)(c) << 8) | ((ULONG)(d))))
#define VIRTIO_PCI_WDM_INT_TAG VIRTIO_PCI_WDM_MAKE_POOL_TAG('t', 'I', 'n', 'V')
#endif

static BOOLEAN VirtioPciWdmMessageIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext, _In_ ULONG MessageId);
static VOID VirtioPciWdmMessageDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2);
static VOID VirtioPciWdmIntxDpc(_Inout_ PVIRTIO_INTX Intx, _In_ UCHAR IsrStatus, _In_opt_ PVOID Cookie);

static __forceinline VOID VirtioPciWdmDispatch(
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts,
    _In_ ULONG MessageId,
    _In_ BOOLEAN IsConfig,
    _In_ USHORT QueueIndex)
{
    if (Interrupts == NULL) {
        return;
    }

    if (Interrupts->EvtDpc != NULL) {
        Interrupts->EvtDpc(Interrupts, MessageId, IsConfig, QueueIndex, Interrupts->Cookie);
        return;
    }

    if (IsConfig) {
        if (Interrupts->EvtConfigChange != NULL) {
            Interrupts->EvtConfigChange(Interrupts, Interrupts->Cookie);
        }
        return;
    }

    if (Interrupts->EvtQueueWork != NULL) {
        Interrupts->EvtQueueWork(Interrupts, QueueIndex, Interrupts->Cookie);
    }
}

static __forceinline BOOLEAN VirtioPciWdmIsMessageInterrupt(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR* Desc)
{
    return ((Desc->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) ? TRUE : FALSE;
}

static __forceinline USHORT VirtioPciWdmMessageCountFromDescriptor(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR* Desc)
{
    /*
     * Windows 7 WDK exposes message interrupts via `u.MessageInterrupt` (not
     * `u.Interrupt`) and provides a MessageCount field.
     */
    return Desc->u.MessageInterrupt.MessageCount;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciWdmInterruptConnect(
    _In_ PDEVICE_OBJECT DeviceObject,
    _In_opt_ PDEVICE_OBJECT PhysicalDeviceObject,
    _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDescTranslated,
    _In_opt_ volatile UCHAR* IsrStatusRegister,
    _In_opt_ EVT_VIRTIO_PCI_WDM_CONFIG_CHANGE* EvtConfigChange,
    _In_opt_ EVT_VIRTIO_PCI_WDM_QUEUE_WORK* EvtQueueWork,
    _In_opt_ EVT_VIRTIO_PCI_WDM_DPC* EvtDpc,
    _In_opt_ PVOID Cookie,
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts)
{
    NTSTATUS status;

    if (Interrupts == NULL || InterruptDescTranslated == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (DeviceObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (InterruptDescTranslated->Type != CmResourceTypeInterrupt) {
        RtlZeroMemory(Interrupts, sizeof(*Interrupts));
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Interrupts, sizeof(*Interrupts));

    Interrupts->EvtConfigChange = EvtConfigChange;
    Interrupts->EvtQueueWork = EvtQueueWork;
    Interrupts->EvtDpc = EvtDpc;
    Interrupts->Cookie = Cookie;

    if (!VirtioPciWdmIsMessageInterrupt(InterruptDescTranslated)) {
        /*
         * INTx path: reuse the dedicated INTx helper.
         *
         * Note: the INTx helper does not use DeviceObject, but we validate it to
         * keep a consistent API surface.
         */
        if (IsrStatusRegister == NULL) {
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return STATUS_INVALID_DEVICE_STATE;
        }

        status = VirtioIntxConnect(DeviceObject,
                                   InterruptDescTranslated,
                                   IsrStatusRegister,
                                   NULL,
                                   NULL,
                                   VirtioPciWdmIntxDpc,
                                   Interrupts,
                                   &Interrupts->u.Intx.Intx);
        if (!NT_SUCCESS(status)) {
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return status;
        }

        Interrupts->Mode = VirtioPciWdmInterruptModeIntx;
        Interrupts->Initialized = TRUE;
        return STATUS_SUCCESS;
    }

    /*
     * MSI/MSI-X path.
     *
     * Message interrupt descriptors report the MessageCount in u.MessageInterrupt.
     */
    {
        USHORT messageCount;
        ULONG i;
        IO_CONNECT_INTERRUPT_PARAMETERS params;

        if (PhysicalDeviceObject == NULL) {
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return STATUS_INVALID_PARAMETER;
        }

        messageCount = VirtioPciWdmMessageCountFromDescriptor(InterruptDescTranslated);
        if (messageCount == 0) {
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return STATUS_INVALID_PARAMETER;
        }

        /* Allocate per-message KDPC + route arrays from nonpaged pool. */
        Interrupts->u.Message.MessageCount = (ULONG)messageCount;
        Interrupts->u.Message.MessageDpcs = (PKDPC)ExAllocatePoolWithTag(NonPagedPool,
                                                                        sizeof(KDPC) * (size_t)messageCount,
                                                                        VIRTIO_PCI_WDM_INT_TAG);
        if (Interrupts->u.Message.MessageDpcs == NULL) {
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        Interrupts->u.Message.Routes =
            (VIRTIO_PCI_WDM_MESSAGE_ROUTE*)ExAllocatePoolWithTag(NonPagedPool,
                                                                sizeof(VIRTIO_PCI_WDM_MESSAGE_ROUTE) * (size_t)messageCount,
                                                                VIRTIO_PCI_WDM_INT_TAG);
        if (Interrupts->u.Message.Routes == NULL) {
            ExFreePoolWithTag(Interrupts->u.Message.MessageDpcs, VIRTIO_PCI_WDM_INT_TAG);
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        RtlZeroMemory(Interrupts->u.Message.MessageDpcs, sizeof(KDPC) * (size_t)messageCount);
        RtlZeroMemory(Interrupts->u.Message.Routes, sizeof(VIRTIO_PCI_WDM_MESSAGE_ROUTE) * (size_t)messageCount);

        for (i = 0; i < (ULONG)messageCount; ++i) {
            /*
             * Default mapping:
             *   message 0 -> config
             *   message 1.. -> queue (message - 1)
             *
             * Drivers can override with VirtioPciWdmInterruptSetMessageRoute.
             */
            Interrupts->u.Message.Routes[i].IsConfig = (i == 0) ? TRUE : FALSE;
            if (i == 0) {
                /*
                 * When only one message interrupt is available, virtio devices
                 * must route config + all queues to that single message (vector
                 * 0 fallback). Represent this as config + "unknown/all queues".
                 *
                 * Otherwise, message 0 is treated as config-only by default to
                 * avoid draining queues concurrently with per-queue message DPCs.
                 */
                Interrupts->u.Message.Routes[i].QueueIndex =
                    (messageCount == 1) ? VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN : VIRTIO_PCI_WDM_QUEUE_INDEX_NONE;
            } else {
                Interrupts->u.Message.Routes[i].QueueIndex = (USHORT)(i - 1);
            }

            KeInitializeDpc(&Interrupts->u.Message.MessageDpcs[i], VirtioPciWdmMessageDpc, Interrupts);
        }

        Interrupts->u.Message.DpcInFlight = 0;

        /*
         * Mark the structure as initialized for the message ISR before calling
         * IoConnectInterruptEx.
         *
         * On real systems, an MSI/MSI-X interrupt can arrive on another CPU
         * immediately after (or even while) IoConnectInterruptEx establishes the
         * connection. If we defer setting Mode/Initialized until after the call
         * returns, the ISR could reject a legitimate interrupt as "not ours".
         *
         * The fields required by the ISR (Mode, MessageDpcs, MessageCount) are
         * already set up at this point.
         */
        Interrupts->Mode = VirtioPciWdmInterruptModeMessage;
        Interrupts->Initialized = TRUE;

        RtlZeroMemory(&params, sizeof(params));
        params.Version = CONNECT_MESSAGE_BASED;
        params.MessageBased.PhysicalDeviceObject = PhysicalDeviceObject;
        params.MessageBased.ServiceRoutine = VirtioPciWdmMessageIsr;
        params.MessageBased.ServiceContext = Interrupts;
        params.MessageBased.SpinLock = NULL;
        params.MessageBased.SynchronizeIrql = (ULONG)InterruptDescTranslated->u.MessageInterrupt.Level;
        params.MessageBased.FloatingSave = FALSE;
        params.MessageBased.MessageCount = (ULONG)messageCount;
        params.MessageBased.MessageInfo = NULL;
        params.MessageBased.ConnectionContext = NULL;

        status = IoConnectInterruptEx(&params);
        if (!NT_SUCCESS(status)) {
            ExFreePoolWithTag(Interrupts->u.Message.Routes, VIRTIO_PCI_WDM_INT_TAG);
            ExFreePoolWithTag(Interrupts->u.Message.MessageDpcs, VIRTIO_PCI_WDM_INT_TAG);
            RtlZeroMemory(Interrupts, sizeof(*Interrupts));
            return status;
        }

        Interrupts->u.Message.ConnectionContext = params.MessageBased.ConnectionContext;
        Interrupts->u.Message.MessageInfo = params.MessageBased.MessageInfo;
        return STATUS_SUCCESS;
    }
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtioPciWdmInterruptDisconnect(_Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts)
{
    LARGE_INTEGER delay;

    if (Interrupts == NULL) {
        return;
    }

    if (!Interrupts->Initialized) {
        RtlZeroMemory(Interrupts, sizeof(*Interrupts));
        return;
    }

    /* Ensure any late-running DPC does not call back into the driver. */
    Interrupts->EvtConfigChange = NULL;
    Interrupts->EvtQueueWork = NULL;
    Interrupts->EvtDpc = NULL;
    Interrupts->Cookie = NULL;

    if (Interrupts->Mode == VirtioPciWdmInterruptModeIntx) {
        VirtioIntxDisconnect(&Interrupts->u.Intx.Intx);
        RtlZeroMemory(Interrupts, sizeof(*Interrupts));
        return;
    }

    if (Interrupts->Mode != VirtioPciWdmInterruptModeMessage) {
        RtlZeroMemory(Interrupts, sizeof(*Interrupts));
        return;
    }

    /*
     * Disconnect the message-based interrupt.
     *
     * IoDisconnectInterruptEx is expected to quiesce ISR delivery before returning.
     */
    if (Interrupts->u.Message.ConnectionContext != NULL) {
        IO_DISCONNECT_INTERRUPT_PARAMETERS params;
        RtlZeroMemory(&params, sizeof(params));
        params.Version = DISCONNECT_MESSAGE_BASED;
        params.MessageBased.ConnectionContext = Interrupts->u.Message.ConnectionContext;
        IoDisconnectInterruptEx(&params);
        Interrupts->u.Message.ConnectionContext = NULL;
        Interrupts->u.Message.MessageInfo = NULL;
    }

    /* Cancel any DPCs that are queued but not yet running. */
    if (Interrupts->u.Message.MessageDpcs != NULL) {
        ULONG i;
        for (i = 0; i < Interrupts->u.Message.MessageCount; ++i) {
            BOOLEAN removed = KeRemoveQueueDpc(&Interrupts->u.Message.MessageDpcs[i]);
            if (removed) {
                LONG remaining = InterlockedDecrement(&Interrupts->u.Message.DpcInFlight);
                if (remaining < 0) {
                    (VOID)InterlockedExchange(&Interrupts->u.Message.DpcInFlight, 0);
                }
            }
        }
    }

    /* Wait for any in-flight DPC to finish before freeing the arrays. */
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        LONG remaining;
        delay.QuadPart = -10 * 1000; /* 1ms */
        for (;;) {
            remaining = InterlockedCompareExchange(&Interrupts->u.Message.DpcInFlight, 0, 0);
            if (remaining <= 0) {
                if (remaining < 0) {
                    (VOID)InterlockedExchange(&Interrupts->u.Message.DpcInFlight, 0);
                }
                break;
            }

            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    } else {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        return;
    }

    if (Interrupts->u.Message.Routes != NULL) {
        ExFreePoolWithTag(Interrupts->u.Message.Routes, VIRTIO_PCI_WDM_INT_TAG);
        Interrupts->u.Message.Routes = NULL;
    }

    if (Interrupts->u.Message.MessageDpcs != NULL) {
        ExFreePoolWithTag(Interrupts->u.Message.MessageDpcs, VIRTIO_PCI_WDM_INT_TAG);
        Interrupts->u.Message.MessageDpcs = NULL;
    }

    RtlZeroMemory(Interrupts, sizeof(*Interrupts));
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciWdmInterruptSetMessageRoute(
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts,
    _In_ ULONG MessageId,
    _In_ BOOLEAN IsConfig,
    _In_ USHORT QueueIndex)
{
    if (Interrupts == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!Interrupts->Initialized || Interrupts->Mode != VirtioPciWdmInterruptModeMessage) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Interrupts->u.Message.Routes == NULL || MessageId >= Interrupts->u.Message.MessageCount) {
        return STATUS_INVALID_PARAMETER;
    }

    Interrupts->u.Message.Routes[MessageId].IsConfig = IsConfig;
    Interrupts->u.Message.Routes[MessageId].QueueIndex = QueueIndex;
    return STATUS_SUCCESS;
}

/*
 * INTx DPC adapter:
 *   VirtioIntx invokes us with the latched ISR status byte.
 */
static VOID VirtioPciWdmIntxDpc(_Inout_ PVIRTIO_INTX Intx, _In_ UCHAR IsrStatus, _In_opt_ PVOID Cookie)
{
    PVIRTIO_PCI_WDM_INTERRUPTS interrupts = (PVIRTIO_PCI_WDM_INTERRUPTS)Cookie;
    UNREFERENCED_PARAMETER(Intx);

    if (interrupts == NULL) {
        return;
    }

    if ((IsrStatus & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0) {
        VirtioPciWdmDispatch(interrupts, VIRTIO_PCI_WDM_MESSAGE_ID_NONE, TRUE, VIRTIO_PCI_WDM_QUEUE_INDEX_NONE);
    }

    if ((IsrStatus & VIRTIO_PCI_ISR_QUEUE_INTERRUPT) != 0) {
        VirtioPciWdmDispatch(interrupts, VIRTIO_PCI_WDM_MESSAGE_ID_NONE, FALSE, VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN);
    }
}

/*
 * PKMESSAGE_SERVICE_ROUTINE (message-signaled ISR).
 *
 * IMPORTANT: Must NOT read the virtio ISR status byte.
 */
static BOOLEAN VirtioPciWdmMessageIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext, _In_ ULONG MessageId)
{
    PVIRTIO_PCI_WDM_INTERRUPTS interrupts;
    BOOLEAN inserted;

    UNREFERENCED_PARAMETER(Interrupt);

    interrupts = (PVIRTIO_PCI_WDM_INTERRUPTS)ServiceContext;
    if (interrupts == NULL || !interrupts->Initialized) {
        return FALSE;
    }

    if (interrupts->Mode != VirtioPciWdmInterruptModeMessage) {
        return FALSE;
    }

    if (interrupts->u.Message.MessageDpcs == NULL || MessageId >= interrupts->u.Message.MessageCount) {
        return FALSE;
    }

    (VOID)InterlockedIncrement(&interrupts->u.Message.IsrCount);

    /*
     * Track queued + running DPC instances.
     *
     * Increment the counter *before* queueing to avoid a race where the DPC runs
     * on another CPU (target-processor DPC) before we increment.
     */
    (VOID)InterlockedIncrement(&interrupts->u.Message.DpcInFlight);
    inserted = KeInsertQueueDpc(&interrupts->u.Message.MessageDpcs[MessageId], NULL, NULL);
    if (!inserted) {
        LONG remaining = InterlockedDecrement(&interrupts->u.Message.DpcInFlight);
        if (remaining < 0) {
            (VOID)InterlockedExchange(&interrupts->u.Message.DpcInFlight, 0);
        }
    }

    return TRUE;
}

/*
 * PKDEFERRED_ROUTINE
 *
 * Runs at DISPATCH_LEVEL.
 */
static VOID VirtioPciWdmMessageDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2)
{
    PVIRTIO_PCI_WDM_INTERRUPTS interrupts;
    ULONG messageId;
    VIRTIO_PCI_WDM_MESSAGE_ROUTE route;
    LONG remaining;

    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    interrupts = (PVIRTIO_PCI_WDM_INTERRUPTS)DeferredContext;
    if (interrupts == NULL) {
        return;
    }

    if (interrupts->Mode != VirtioPciWdmInterruptModeMessage || interrupts->u.Message.MessageDpcs == NULL) {
        return;
    }

    messageId = (ULONG)(Dpc - interrupts->u.Message.MessageDpcs);
    if (messageId >= interrupts->u.Message.MessageCount) {
        return;
    }

    (VOID)InterlockedIncrement(&interrupts->u.Message.DpcCount);

    if (interrupts->u.Message.Routes != NULL) {
        route = interrupts->u.Message.Routes[messageId];
    } else {
        route.IsConfig = (messageId == 0) ? TRUE : FALSE;
        if (messageId == 0) {
            route.QueueIndex = (interrupts->u.Message.MessageCount == 1) ? VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN : VIRTIO_PCI_WDM_QUEUE_INDEX_NONE;
        } else {
            route.QueueIndex = (USHORT)(messageId - 1);
        }
    }

    if (route.IsConfig) {
        VirtioPciWdmDispatch(interrupts, messageId, TRUE, VIRTIO_PCI_WDM_QUEUE_INDEX_NONE);
    }

    if (route.QueueIndex != VIRTIO_PCI_WDM_QUEUE_INDEX_NONE) {
        VirtioPciWdmDispatch(interrupts, messageId, FALSE, route.QueueIndex);
    }

    remaining = InterlockedDecrement(&interrupts->u.Message.DpcInFlight);
    if (remaining < 0) {
        (VOID)InterlockedExchange(&interrupts->u.Message.DpcInFlight, 0);
    }
}

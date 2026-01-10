#include "virtio_pci_interrupts.h"

#define VIRTIO_PCI_INTERRUPTS_POOL_TAG 'tInV'

typedef struct _VIRTIO_PCI_INTERRUPT_CONTEXT {
    PVIRTIO_PCI_INTERRUPTS Interrupts;
    USHORT MsixVectorIndex;
    BOOLEAN HandlesConfig;
    ULONGLONG QueueMask;
} VIRTIO_PCI_INTERRUPT_CONTEXT, *PVIRTIO_PCI_INTERRUPT_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_PCI_INTERRUPT_CONTEXT, VirtioPciInterruptGetContext);

#if DBG
#define VIRTIO_PCI_TRACE_INFO(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "virtio-input: " __VA_ARGS__)
#define VIRTIO_PCI_TRACE_ERROR(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL, "virtio-input: " __VA_ARGS__)
#else
#define VIRTIO_PCI_TRACE_INFO(...) ((void)0)
#define VIRTIO_PCI_TRACE_ERROR(...) ((void)0)
#endif

static BOOLEAN VirtioPciIntxIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID);
static BOOLEAN VirtioPciMsixIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID);
static VOID VirtioPciInterruptDpc(_In_ WDFINTERRUPT Interrupt, _In_ WDFOBJECT AssociatedObject);

static NTSTATUS VirtioPciFindInterruptResources(
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated,
    _Out_ PCM_PARTIAL_RESOURCE_DESCRIPTOR* InterruptRaw,
    _Out_ PCM_PARTIAL_RESOURCE_DESCRIPTOR* InterruptTranslated
    )
{
    ULONG i;
    ULONG count;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR candidateRaw;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR candidateTranslated;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR translatedDesc;

    rawDesc = NULL;
    translatedDesc = NULL;

    count = WdfCmResourceListGetCount(ResourcesTranslated);
    candidateRaw = NULL;
    candidateTranslated = NULL;

    //
    // Prefer message-signaled interrupts when present; fall back to the first
    // legacy line interrupt descriptor.
    //
    for (i = 0; i < count; i++) {
        translatedDesc = WdfCmResourceListGetDescriptor(ResourcesTranslated, i);
        if ((translatedDesc == NULL) || (translatedDesc->Type != CmResourceTypeInterrupt)) {
            continue;
        }

        rawDesc = WdfCmResourceListGetDescriptor(ResourcesRaw, i);
        if (rawDesc == NULL) {
            continue;
        }

        if ((translatedDesc->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
            *InterruptRaw = rawDesc;
            *InterruptTranslated = translatedDesc;
            return STATUS_SUCCESS;
        }

        if (candidateRaw == NULL) {
            candidateRaw = rawDesc;
            candidateTranslated = translatedDesc;
        }
    }

    rawDesc = candidateRaw;
    translatedDesc = candidateTranslated;

    if (rawDesc == NULL || translatedDesc == NULL) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    *InterruptRaw = rawDesc;
    *InterruptTranslated = translatedDesc;
    return STATUS_SUCCESS;
}

static ULONGLONG VirtioPciQueueMaskAll(_In_ ULONG QueueCount)
{
    ULONGLONG mask;
    ULONG q;

    mask = 0;
    for (q = 0; q < QueueCount; q++) {
        mask |= (1ULL << q);
    }

    return mask;
}

static VOID VirtioPciTraceVectorMapping(
    _In_ ULONG QueueCount,
    _In_ USHORT UsedVectorCount,
    _In_ const USHORT* QueueVectors
    )
{
    ULONG vector;
    ULONG q;

    for (q = 0; q < QueueCount; q++) {
        VIRTIO_PCI_TRACE_INFO("queue[%lu] -> vector %u\n", q, QueueVectors[q]);
    }

    for (vector = 0; vector < UsedVectorCount; vector++) {
        VIRTIO_PCI_TRACE_INFO("vector %lu: config=%s\n", vector, (vector == 0) ? "yes" : "no");
        for (q = 0; q < QueueCount; q++) {
            if (QueueVectors[q] == (USHORT)vector) {
                VIRTIO_PCI_TRACE_INFO("  queue %lu\n", q);
            }
        }
    }

    VIRTIO_PCI_TRACE_INFO("used vectors: %u\n", UsedVectorCount);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsPrepareHardware(
    _In_ WDFDEVICE Device,
    _Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated,
    _In_ ULONG QueueCount,
    _In_ volatile UCHAR* IsrStatusRegister,
    _In_opt_ EVT_VIRTIO_PCI_CONFIG_CHANGE* EvtConfigChange,
    _In_opt_ EVT_VIRTIO_PCI_DRAIN_QUEUE* EvtDrainQueue,
    _In_opt_ PVOID CallbackContext
    )
{
    NTSTATUS status;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR interruptRaw;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR interruptTranslated;
    WDF_OBJECT_ATTRIBUTES attributes;
    ULONG q;

    if (Interrupts == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Interrupts, sizeof(*Interrupts));

    Interrupts->Mode = VirtioPciInterruptModeUnknown;
    Interrupts->QueueCount = QueueCount;
    Interrupts->IsrStatusRegister = IsrStatusRegister;
    Interrupts->EvtConfigChange = EvtConfigChange;
    Interrupts->EvtDrainQueue = EvtDrainQueue;
    Interrupts->CallbackContext = CallbackContext;

    if (QueueCount > 64) {
        return STATUS_NOT_SUPPORTED;
    }

    if (QueueCount != 0) {
        WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
        attributes.ParentObject = Device;
        status = WdfMemoryCreate(
            &attributes,
            NonPagedPool,
            VIRTIO_PCI_INTERRUPTS_POOL_TAG,
            sizeof(WDFSPINLOCK) * QueueCount,
            &Interrupts->QueueLocksMemory,
            (PVOID*)&Interrupts->QueueLocks);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        for (q = 0; q < QueueCount; q++) {
            WDF_OBJECT_ATTRIBUTES lockAttributes;

            WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
            lockAttributes.ParentObject = Interrupts->QueueLocksMemory;

            status = WdfSpinLockCreate(&lockAttributes, &Interrupts->QueueLocks[q]);
            if (!NT_SUCCESS(status)) {
                return status;
            }
        }
    }

    status = VirtioPciFindInterruptResources(ResourcesRaw, ResourcesTranslated, &interruptRaw, &interruptTranslated);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if ((interruptTranslated->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) == 0) {
        WDF_INTERRUPT_CONFIG interruptConfig;
        WDF_OBJECT_ATTRIBUTES interruptAttributes;
        PVIRTIO_PCI_INTERRUPT_CONTEXT interruptContext;

        Interrupts->Mode = VirtioPciInterruptModeIntx;

        WDF_INTERRUPT_CONFIG_INIT(&interruptConfig, VirtioPciIntxIsr, VirtioPciInterruptDpc);
        interruptConfig.InterruptRaw = interruptRaw;
        interruptConfig.InterruptTranslated = interruptTranslated;
        interruptConfig.AutomaticSerialization = FALSE;

        WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&interruptAttributes, VIRTIO_PCI_INTERRUPT_CONTEXT);
        interruptAttributes.ParentObject = Device;

        status = WdfInterruptCreate(Device, &interruptConfig, &interruptAttributes, &Interrupts->u.Intx.Interrupt);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        interruptContext = VirtioPciInterruptGetContext(Interrupts->u.Intx.Interrupt);
        interruptContext->Interrupts = Interrupts;
        interruptContext->MsixVectorIndex = 0;
        interruptContext->HandlesConfig = TRUE;
        interruptContext->QueueMask = VirtioPciQueueMaskAll(QueueCount);

        VIRTIO_PCI_TRACE_INFO("interrupt mode: INTx\n");
        return STATUS_SUCCESS;
    }

    {
        ULONG messageCount;
        USHORT usedVectorCount;
        WDF_OBJECT_ATTRIBUTES memoryAttributes;
        WDF_OBJECT_ATTRIBUTES interruptAttributes;
        ULONG vector;

        Interrupts->Mode = VirtioPciInterruptModeMsix;

        messageCount = (ULONG)interruptTranslated->u.MessageInterrupt.MessageCount;
        if (messageCount == 0) {
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
        Interrupts->u.Msix.MessageCount = messageCount;

        usedVectorCount = 1;
        if (messageCount >= (1 + QueueCount)) {
            usedVectorCount = (USHORT)(1 + QueueCount);
        }

        Interrupts->u.Msix.UsedVectorCount = usedVectorCount;
        Interrupts->u.Msix.ConfigVector = 0;

        if (QueueCount != 0) {
            WDF_OBJECT_ATTRIBUTES_INIT(&memoryAttributes);
            memoryAttributes.ParentObject = Device;
            status = WdfMemoryCreate(
                &memoryAttributes,
                NonPagedPool,
                VIRTIO_PCI_INTERRUPTS_POOL_TAG,
                sizeof(USHORT) * QueueCount,
                &Interrupts->u.Msix.QueueVectorsMemory,
                (PVOID*)&Interrupts->u.Msix.QueueVectors);
            if (!NT_SUCCESS(status)) {
                return status;
            }

            for (q = 0; q < QueueCount; q++) {
                Interrupts->u.Msix.QueueVectors[q] = (usedVectorCount == 1) ? 0 : (USHORT)(1 + q);
            }
        }

        WDF_OBJECT_ATTRIBUTES_INIT(&memoryAttributes);
        memoryAttributes.ParentObject = Device;
        status = WdfMemoryCreate(
            &memoryAttributes,
            NonPagedPool,
            VIRTIO_PCI_INTERRUPTS_POOL_TAG,
            sizeof(WDFINTERRUPT) * usedVectorCount,
            &Interrupts->u.Msix.InterruptsMemory,
            (PVOID*)&Interrupts->u.Msix.Interrupts);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        for (vector = 0; vector < usedVectorCount; vector++) {
            WDF_INTERRUPT_CONFIG interruptConfig;
            PVIRTIO_PCI_INTERRUPT_CONTEXT interruptContext;
            ULONGLONG queueMask;

            WDF_INTERRUPT_CONFIG_INIT(&interruptConfig, VirtioPciMsixIsr, VirtioPciInterruptDpc);
            interruptConfig.InterruptRaw = interruptRaw;
            interruptConfig.InterruptTranslated = interruptTranslated;
            interruptConfig.MessageSignaled = TRUE;
            interruptConfig.MessageNumber = vector;
            interruptConfig.AutomaticSerialization = FALSE;

            WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&interruptAttributes, VIRTIO_PCI_INTERRUPT_CONTEXT);
            interruptAttributes.ParentObject = Interrupts->u.Msix.InterruptsMemory;

            status = WdfInterruptCreate(Device, &interruptConfig, &interruptAttributes, &Interrupts->u.Msix.Interrupts[vector]);
            if (!NT_SUCCESS(status)) {
                return status;
            }

            queueMask = 0;
            if (usedVectorCount == 1) {
                queueMask = VirtioPciQueueMaskAll(QueueCount);
            } else if (vector != 0) {
                queueMask = (1ULL << (vector - 1));
            }

            interruptContext = VirtioPciInterruptGetContext(Interrupts->u.Msix.Interrupts[vector]);
            interruptContext->Interrupts = Interrupts;
            interruptContext->MsixVectorIndex = (USHORT)vector;
            interruptContext->HandlesConfig = (vector == 0) ? TRUE : FALSE;
            interruptContext->QueueMask = queueMask;
        }

        VIRTIO_PCI_TRACE_INFO("interrupt mode: MSI/MSI-X\n");
        VIRTIO_PCI_TRACE_INFO("message count: %lu\n", messageCount);
        if (Interrupts->u.Msix.QueueVectors != NULL) {
            VirtioPciTraceVectorMapping(QueueCount, usedVectorCount, Interrupts->u.Msix.QueueVectors);
        }
        return STATUS_SUCCESS;
    }
}

VOID VirtioPciInterruptsReleaseHardware(_Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts)
{
    ULONG q;

    if (Interrupts == NULL) {
        return;
    }

    if (Interrupts->Mode == VirtioPciInterruptModeIntx) {
        if (Interrupts->u.Intx.Interrupt != NULL) {
            WdfObjectDelete(Interrupts->u.Intx.Interrupt);
            Interrupts->u.Intx.Interrupt = NULL;
        }
    } else if (Interrupts->Mode == VirtioPciInterruptModeMsix) {
        if (Interrupts->u.Msix.InterruptsMemory != NULL) {
            WdfObjectDelete(Interrupts->u.Msix.InterruptsMemory);
            Interrupts->u.Msix.InterruptsMemory = NULL;
        }

        if (Interrupts->u.Msix.QueueVectorsMemory != NULL) {
            WdfObjectDelete(Interrupts->u.Msix.QueueVectorsMemory);
            Interrupts->u.Msix.QueueVectorsMemory = NULL;
        }
    }

    if (Interrupts->QueueLocksMemory != NULL) {
        WdfObjectDelete(Interrupts->QueueLocksMemory);
        Interrupts->QueueLocksMemory = NULL;
    } else if (Interrupts->QueueLocks != NULL) {
        for (q = 0; q < Interrupts->QueueCount; q++) {
            if (Interrupts->QueueLocks[q] != NULL) {
                WdfObjectDelete(Interrupts->QueueLocks[q]);
            }
        }
    }

    RtlZeroMemory(Interrupts, sizeof(*Interrupts));
}

static BOOLEAN VirtioPciIntxIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID)
{
    PVIRTIO_PCI_INTERRUPT_CONTEXT interruptContext;
    PVIRTIO_PCI_INTERRUPTS interrupts;
    UCHAR isrStatus;

    UNREFERENCED_PARAMETER(MessageID);

    interruptContext = VirtioPciInterruptGetContext(Interrupt);
    interrupts = interruptContext->Interrupts;

    if (interrupts->IsrStatusRegister == NULL) {
        return FALSE;
    }

    isrStatus = READ_REGISTER_UCHAR(interrupts->IsrStatusRegister);
    if (isrStatus == 0) {
        InterlockedIncrement(&interrupts->u.Intx.SpuriousCount);
        return FALSE;
    }

    InterlockedOr(&interrupts->u.Intx.PendingIsrStatus, (LONG)isrStatus);
    WdfInterruptQueueDpcForIsr(Interrupt);
    return TRUE;
}

static BOOLEAN VirtioPciMsixIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID)
{
    UNREFERENCED_PARAMETER(MessageID);

    WdfInterruptQueueDpcForIsr(Interrupt);
    return TRUE;
}

static VOID VirtioPciInterruptDpc(_In_ WDFINTERRUPT Interrupt, _In_ WDFOBJECT AssociatedObject)
{
    PVIRTIO_PCI_INTERRUPT_CONTEXT interruptContext;
    PVIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE device;
    BOOLEAN processQueues;
    BOOLEAN processConfig;
    UCHAR isrStatus;
    ULONG q;

    interruptContext = VirtioPciInterruptGetContext(Interrupt);
    interrupts = interruptContext->Interrupts;
    device = (WDFDEVICE)AssociatedObject;

    processQueues = TRUE;
    processConfig = interruptContext->HandlesConfig;

    if (interrupts->Mode == VirtioPciInterruptModeIntx) {
        isrStatus = (UCHAR)InterlockedExchange(&interrupts->u.Intx.PendingIsrStatus, 0);
        processConfig = interruptContext->HandlesConfig && ((isrStatus & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0);
        processQueues = ((isrStatus & VIRTIO_PCI_ISR_QUEUE_INTERRUPT) != 0);
    }

    if (processConfig && (interrupts->EvtConfigChange != NULL)) {
        interrupts->EvtConfigChange(device, interrupts->CallbackContext);
    }

    if (processQueues && (interrupts->EvtDrainQueue != NULL)) {
        for (q = 0; q < interrupts->QueueCount; q++) {
            if ((interruptContext->QueueMask & (1ULL << q)) == 0) {
                continue;
            }

            WdfSpinLockAcquire(interrupts->QueueLocks[q]);
            interrupts->EvtDrainQueue(device, q, interrupts->CallbackContext);
            WdfSpinLockRelease(interrupts->QueueLocks[q]);
        }
    }
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciProgramMsixVectors(
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg,
    _In_ ULONG QueueCount,
    _In_ USHORT ConfigVector,
    _In_reads_(QueueCount) const USHORT* QueueVectors
    )
{
    USHORT readVector;
    ULONG q;

    if (CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    WRITE_REGISTER_USHORT(&CommonCfg->msix_config, ConfigVector);
    readVector = READ_REGISTER_USHORT(&CommonCfg->msix_config);

    if (readVector == 0xFFFF || readVector != ConfigVector) {
        VIRTIO_PCI_TRACE_ERROR("failed to set msix_config vector %u (read back %u)\n", ConfigVector, readVector);
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    for (q = 0; q < QueueCount; q++) {
        USHORT queueVector;

        queueVector = QueueVectors[q];

        WRITE_REGISTER_USHORT(&CommonCfg->queue_select, (USHORT)q);
        WRITE_REGISTER_USHORT(&CommonCfg->queue_msix_vector, queueVector);
        readVector = READ_REGISTER_USHORT(&CommonCfg->queue_msix_vector);

        if (readVector == 0xFFFF || readVector != queueVector) {
            VIRTIO_PCI_TRACE_ERROR(
                "failed to set queue %lu msix vector %u (read back %u)\n",
                q,
                queueVector,
                readVector);
            return STATUS_DEVICE_HARDWARE_ERROR;
        }
    }

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsProgramMsixVectors(
    _In_ const PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg
    )
{
    if (Interrupts == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Interrupts->Mode != VirtioPciInterruptModeMsix) {
        return STATUS_SUCCESS;
    }

    return VirtioPciProgramMsixVectors(
        CommonCfg,
        Interrupts->QueueCount,
        Interrupts->u.Msix.ConfigVector,
        Interrupts->u.Msix.QueueVectors);
}

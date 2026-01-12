#include "virtio_pci_interrupts.h"

/*
 * Pool tags are traditionally specified as multi-character constants (e.g. 'tInV')
 * in WDK codebases. Host-side unit tests build this file with GCC/Clang, which
 * warn on multi-character character constants.
 *
 * Define the tag via a portable shift-based encoding for non-MSVC builds to
 * avoid -Wmultichar noise in CI.
 */
#if defined(_MSC_VER)
#define VIRTIO_PCI_INTERRUPTS_POOL_TAG 'tInV'
#else
#define VIRTIO_PCI_MAKE_POOL_TAG(a, b, c, d) \
	((ULONG)(((ULONG)(a) << 24) | ((ULONG)(b) << 16) | ((ULONG)(c) << 8) | ((ULONG)(d))))
#define VIRTIO_PCI_INTERRUPTS_POOL_TAG VIRTIO_PCI_MAKE_POOL_TAG('t', 'I', 'n', 'V')
#endif

typedef struct _VIRTIO_PCI_INTERRUPT_CONTEXT {
    PVIRTIO_PCI_INTERRUPTS Interrupts;
    USHORT MsixVectorIndex;
    BOOLEAN HandlesConfig;
    ULONGLONG QueueMask;
} VIRTIO_PCI_INTERRUPT_CONTEXT, *PVIRTIO_PCI_INTERRUPT_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_PCI_INTERRUPT_CONTEXT, VirtioPciInterruptGetContext);

static BOOLEAN VirtioPciIntxIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID);
static BOOLEAN VirtioPciMsixIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID);
static VOID VirtioPciInterruptDpc(_In_ WDFINTERRUPT Interrupt, _In_ WDFOBJECT AssociatedObject);

static __forceinline VOID VirtioPciAcquireOptSpinLock(_In_opt_ WDFSPINLOCK Lock)
{
    if (Lock != NULL) {
        WdfSpinLockAcquire(Lock);
    }
}

static __forceinline VOID VirtioPciReleaseOptSpinLock(_In_opt_ WDFSPINLOCK Lock)
{
    if (Lock != NULL) {
        WdfSpinLockRelease(Lock);
    }
}

static NTSTATUS VirtioPciFindInterruptResources(
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated,
    _Out_ PCM_PARTIAL_RESOURCE_DESCRIPTOR* InterruptRaw,
    _Out_ PCM_PARTIAL_RESOURCE_DESCRIPTOR* InterruptTranslated)
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

_IRQL_requires_max_(PASSIVE_LEVEL)
static NTSTATUS VirtioPciDisableMsixVectors(
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg,
    _In_opt_ WDFSPINLOCK CommonCfgLock,
    _In_ ULONG QueueCount)
{
    USHORT readVector;
    ULONG q;

    if (CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtioPciAcquireOptSpinLock(CommonCfgLock);

    WRITE_REGISTER_USHORT(&CommonCfg->msix_config, VIRTIO_PCI_MSI_NO_VECTOR);
    readVector = READ_REGISTER_USHORT(&CommonCfg->msix_config);
    if (readVector != VIRTIO_PCI_MSI_NO_VECTOR) {
        VirtioPciReleaseOptSpinLock(CommonCfgLock);
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    for (q = 0; q < QueueCount; q++) {
        WRITE_REGISTER_USHORT(&CommonCfg->queue_select, (USHORT)q);
        (VOID)READ_REGISTER_USHORT(&CommonCfg->queue_select);
        WRITE_REGISTER_USHORT(&CommonCfg->queue_msix_vector, VIRTIO_PCI_MSI_NO_VECTOR);
        readVector = READ_REGISTER_USHORT(&CommonCfg->queue_msix_vector);
        if (readVector != VIRTIO_PCI_MSI_NO_VECTOR) {
            VirtioPciReleaseOptSpinLock(CommonCfgLock);
            return STATUS_DEVICE_HARDWARE_ERROR;
        }
    }

    VirtioPciReleaseOptSpinLock(CommonCfgLock);

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtioPciInterruptsPrepareHardware(
    WDFDEVICE Device,
    PVIRTIO_PCI_INTERRUPTS Interrupts,
    WDFCMRESLIST ResourcesRaw,
    WDFCMRESLIST ResourcesTranslated,
    ULONG QueueCount,
    volatile UCHAR* IsrStatusRegister,
    WDFSPINLOCK CommonCfgLock,
    EVT_VIRTIO_PCI_CONFIG_CHANGE* EvtConfigChange,
    EVT_VIRTIO_PCI_DRAIN_QUEUE* EvtDrainQueue,
    PVOID CallbackContext)
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
    Interrupts->CommonCfgLock = CommonCfgLock;
    Interrupts->ResetInProgress = 0;
    Interrupts->EvtConfigChange = EvtConfigChange;
    Interrupts->EvtDrainQueue = EvtDrainQueue;
    Interrupts->CallbackContext = CallbackContext;

    if (QueueCount > 64) {
        return STATUS_NOT_SUPPORTED;
    }

    {
        WDF_OBJECT_ATTRIBUTES lockAttributes;
        WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
        lockAttributes.ParentObject = Device;
        status = WdfSpinLockCreate(&lockAttributes, &Interrupts->ConfigLock);
        if (!NT_SUCCESS(status)) {
            return status;
        }
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
        Interrupts->u.Msix.ConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;

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
            RtlZeroMemory(Interrupts->u.Msix.QueueVectors, sizeof(USHORT) * QueueCount);
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

        //
        // MSI-X vector programming uses the message numbers (MSI-X table entry
        // indices) that KMDF actually connected. Query these via
        // WdfInterruptGetInfo so drivers never accidentally program APIC vectors.
        //
        {
            WDF_INTERRUPT_INFO info;
            ULONG messageNumber;

            WDF_INTERRUPT_INFO_INIT(&info);
            WdfInterruptGetInfo(Interrupts->u.Msix.Interrupts[0], &info);
            messageNumber = info.MessageNumber;
            Interrupts->u.Msix.ConfigVector = (messageNumber >= VIRTIO_PCI_MSI_NO_VECTOR) ?
                VIRTIO_PCI_MSI_NO_VECTOR :
                (USHORT)messageNumber;

            if (Interrupts->u.Msix.QueueVectors != NULL) {
                if (usedVectorCount == 1) {
                    for (q = 0; q < QueueCount; q++) {
                        Interrupts->u.Msix.QueueVectors[q] = Interrupts->u.Msix.ConfigVector;
                    }
                } else {
                    for (q = 0; q < QueueCount; q++) {
                        WDF_INTERRUPT_INFO_INIT(&info);
                        WdfInterruptGetInfo(Interrupts->u.Msix.Interrupts[1 + q], &info);
                        messageNumber = info.MessageNumber;
                        Interrupts->u.Msix.QueueVectors[q] = (messageNumber >= VIRTIO_PCI_MSI_NO_VECTOR) ?
                            VIRTIO_PCI_MSI_NO_VECTOR :
                            (USHORT)messageNumber;
                    }
                }
            }
        }

        return STATUS_SUCCESS;
    }
}

VOID VirtioPciInterruptsReleaseHardware(_Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts)
{
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
    }

    if (Interrupts->ConfigLock != NULL) {
        WdfObjectDelete(Interrupts->ConfigLock);
        Interrupts->ConfigLock = NULL;
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

    //
    // Read-to-ack: deasserts the level-triggered INTx line.
    //
    isrStatus = READ_REGISTER_UCHAR(interrupts->IsrStatusRegister);
    if (isrStatus == 0) {
        InterlockedIncrement(&interrupts->u.Intx.SpuriousCount);
        return FALSE;
    }

    if (interrupts->InterruptCounter != NULL) {
        (VOID)InterlockedIncrement(interrupts->InterruptCounter);
    }

    if (InterlockedCompareExchange(&interrupts->ResetInProgress, 0, 0) != 0) {
        return TRUE;
    }

    InterlockedOr(&interrupts->u.Intx.PendingIsrStatus, (LONG)isrStatus);
    WdfInterruptQueueDpcForIsr(Interrupt);
    return TRUE;
}

static BOOLEAN VirtioPciMsixIsr(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID)
{
    PVIRTIO_PCI_INTERRUPT_CONTEXT interruptContext = VirtioPciInterruptGetContext(Interrupt);
    PVIRTIO_PCI_INTERRUPTS interrupts = interruptContext->Interrupts;

    UNREFERENCED_PARAMETER(MessageID);

    if (interrupts->InterruptCounter != NULL) {
        (VOID)InterlockedIncrement(interrupts->InterruptCounter);
    }

    if (InterlockedCompareExchange(&interrupts->ResetInProgress, 0, 0) != 0) {
        return TRUE;
    }

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

    if (interrupts->DpcCounter != NULL) {
        (VOID)InterlockedIncrement(interrupts->DpcCounter);
    }

    if (InterlockedCompareExchange(&interrupts->ResetInProgress, 0, 0) != 0) {
        if (interrupts->Mode == VirtioPciInterruptModeIntx) {
            (VOID)InterlockedExchange(&interrupts->u.Intx.PendingIsrStatus, 0);
        }
        return;
    }

    processQueues = TRUE;
    processConfig = interruptContext->HandlesConfig;
    isrStatus = 0;

    if (interrupts->Mode == VirtioPciInterruptModeIntx) {
        isrStatus = (UCHAR)InterlockedExchange(&interrupts->u.Intx.PendingIsrStatus, 0);
        processConfig = interruptContext->HandlesConfig && ((isrStatus & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0);
        processQueues = ((isrStatus & VIRTIO_PCI_ISR_QUEUE_INTERRUPT) != 0);
    }

    if (processConfig && (interrupts->EvtConfigChange != NULL)) {
        WdfSpinLockAcquire(interrupts->ConfigLock);
        if (InterlockedCompareExchange(&interrupts->ResetInProgress, 0, 0) == 0) {
            interrupts->EvtConfigChange(device, interrupts->CallbackContext);
        }
        WdfSpinLockRelease(interrupts->ConfigLock);
    }

    if (processQueues && (interrupts->EvtDrainQueue != NULL)) {
        for (q = 0; q < interrupts->QueueCount; q++) {
            if ((interruptContext->QueueMask & (1ULL << q)) == 0) {
                continue;
            }

            if (interrupts->QueueLocks != NULL) {
                WdfSpinLockAcquire(interrupts->QueueLocks[q]);
            }
            if (InterlockedCompareExchange(&interrupts->ResetInProgress, 0, 0) == 0) {
                interrupts->EvtDrainQueue(device, q, interrupts->CallbackContext);
            }
            if (interrupts->QueueLocks != NULL) {
                WdfSpinLockRelease(interrupts->QueueLocks[q]);
            }
        }
    }
}

_Use_decl_annotations_
NTSTATUS VirtioPciProgramMsixVectors(
    volatile VIRTIO_PCI_COMMON_CFG* CommonCfg,
    WDFSPINLOCK CommonCfgLock,
    ULONG QueueCount,
    USHORT ConfigVector,
    const USHORT* QueueVectors)
{
    USHORT readVector;
    ULONG q;

    if (CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (QueueCount != 0 && QueueVectors == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtioPciAcquireOptSpinLock(CommonCfgLock);

    WRITE_REGISTER_USHORT(&CommonCfg->msix_config, ConfigVector);
    readVector = READ_REGISTER_USHORT(&CommonCfg->msix_config);

    if (readVector == VIRTIO_PCI_MSI_NO_VECTOR || readVector != ConfigVector) {
        VirtioPciReleaseOptSpinLock(CommonCfgLock);
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    for (q = 0; q < QueueCount; q++) {
        USHORT queueVector = QueueVectors[q];

        WRITE_REGISTER_USHORT(&CommonCfg->queue_select, (USHORT)q);
        (VOID)READ_REGISTER_USHORT(&CommonCfg->queue_select);
        WRITE_REGISTER_USHORT(&CommonCfg->queue_msix_vector, queueVector);
        readVector = READ_REGISTER_USHORT(&CommonCfg->queue_msix_vector);

        if (readVector == VIRTIO_PCI_MSI_NO_VECTOR || readVector != queueVector) {
            VirtioPciReleaseOptSpinLock(CommonCfgLock);
            return STATUS_DEVICE_HARDWARE_ERROR;
        }
    }

    VirtioPciReleaseOptSpinLock(CommonCfgLock);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtioPciInterruptsProgramMsixVectors(
    const PVIRTIO_PCI_INTERRUPTS Interrupts,
    volatile VIRTIO_PCI_COMMON_CFG* CommonCfg)
{
    if (Interrupts == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Interrupts->Mode != VirtioPciInterruptModeMsix) {
        return STATUS_SUCCESS;
    }

    return VirtioPciProgramMsixVectors(
        CommonCfg,
        Interrupts->CommonCfgLock,
        Interrupts->QueueCount,
        Interrupts->u.Msix.ConfigVector,
        Interrupts->u.Msix.QueueVectors);
}

_Use_decl_annotations_
NTSTATUS VirtioPciInterruptsQuiesce(PVIRTIO_PCI_INTERRUPTS Interrupts, volatile VIRTIO_PCI_COMMON_CFG* CommonCfg)
{
    NTSTATUS status;
    ULONG i;

    if (Interrupts == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    InterlockedExchange(&Interrupts->ResetInProgress, 1);

    status = STATUS_SUCCESS;

    if (Interrupts->Mode == VirtioPciInterruptModeIntx) {
        if (Interrupts->u.Intx.Interrupt != NULL) {
            status = WdfInterruptDisable(Interrupts->u.Intx.Interrupt);
        }
    } else if (Interrupts->Mode == VirtioPciInterruptModeMsix) {
        for (i = 0; i < Interrupts->u.Msix.UsedVectorCount; i++) {
            if (Interrupts->u.Msix.Interrupts != NULL && Interrupts->u.Msix.Interrupts[i] != NULL) {
                NTSTATUS disableStatus = WdfInterruptDisable(Interrupts->u.Msix.Interrupts[i]);
                if (!NT_SUCCESS(disableStatus) && NT_SUCCESS(status)) {
                    status = disableStatus;
                }
            }
        }

        if (CommonCfg != NULL) {
            NTSTATUS vectorStatus = VirtioPciDisableMsixVectors(CommonCfg, Interrupts->CommonCfgLock, Interrupts->QueueCount);
            if (!NT_SUCCESS(vectorStatus) && NT_SUCCESS(status)) {
                status = vectorStatus;
            }
        } else if (NT_SUCCESS(status)) {
            status = STATUS_INVALID_PARAMETER;
        }
    }

    //
    // Synchronize with any in-flight DPC work:
    // - Config callback section (ConfigLock)
    // - Per-queue callback sections (QueueLocks)
    //
    if (Interrupts->ConfigLock != NULL) {
        WdfSpinLockAcquire(Interrupts->ConfigLock);
        WdfSpinLockRelease(Interrupts->ConfigLock);
    }

    if (Interrupts->QueueLocks != NULL) {
        for (i = 0; i < Interrupts->QueueCount; i++) {
            WdfSpinLockAcquire(Interrupts->QueueLocks[i]);
            WdfSpinLockRelease(Interrupts->QueueLocks[i]);
        }
    }

    return status;
}

_Use_decl_annotations_
NTSTATUS VirtioPciInterruptsResume(PVIRTIO_PCI_INTERRUPTS Interrupts, volatile VIRTIO_PCI_COMMON_CFG* CommonCfg)
{
    NTSTATUS status;
    ULONG i;

    if (Interrupts == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Interrupts->Mode == VirtioPciInterruptModeMsix) {
        if (CommonCfg == NULL) {
            return STATUS_INVALID_PARAMETER;
        }

        status = VirtioPciInterruptsProgramMsixVectors(Interrupts, CommonCfg);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        for (i = 0; i < Interrupts->u.Msix.UsedVectorCount; i++) {
            if (Interrupts->u.Msix.Interrupts == NULL || Interrupts->u.Msix.Interrupts[i] == NULL) {
                continue;
            }

            status = WdfInterruptEnable(Interrupts->u.Msix.Interrupts[i]);
            if (!NT_SUCCESS(status)) {
                return status;
            }
        }
    } else if (Interrupts->Mode == VirtioPciInterruptModeIntx) {
        if (Interrupts->u.Intx.Interrupt != NULL) {
            status = WdfInterruptEnable(Interrupts->u.Intx.Interrupt);
            if (!NT_SUCCESS(status)) {
                return status;
            }
        }
    }

    InterlockedExchange(&Interrupts->ResetInProgress, 0);
    return STATUS_SUCCESS;
}

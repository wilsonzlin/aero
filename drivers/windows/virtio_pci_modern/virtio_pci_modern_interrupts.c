#include "virtio_pci_modern_interrupts.h"

_Use_decl_annotations_
NTSTATUS
VirtioPciModernInitializeLocks(WDFDEVICE Device, PVIRTIO_PCI_DEVICE_CONTEXT DevCtx)
{
    NTSTATUS status;
    WDF_OBJECT_ATTRIBUTES attributes;
    ULONG i;

    if (DevCtx->Queues == NULL && DevCtx->QueueCount != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;

    status = WdfSpinLockCreate(&attributes, &DevCtx->CommonCfgLock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    for (i = 0; i < DevCtx->QueueCount; i++) {
        WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
        attributes.ParentObject = Device;

        status = WdfSpinLockCreate(&attributes, &DevCtx->Queues[i].Lock);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS
VirtioPciModernCreateInterrupt(
    WDFDEVICE Device,
    PVIRTIO_PCI_DEVICE_CONTEXT DevCtx,
    VIRTIO_INTERRUPT_KIND Kind,
    PVIRTIO_QUEUE Queue,
    USHORT MsixVector,
    PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptRaw,
    PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptTranslated,
    WDFINTERRUPT* InterruptOut
)
{
    NTSTATUS status;
    WDF_INTERRUPT_CONFIG config;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDFINTERRUPT interrupt;
    PVIRTIO_INTERRUPT_CONTEXT interruptCtx;

    if (InterruptOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *InterruptOut = NULL;

    WDF_INTERRUPT_CONFIG_INIT(&config, VirtioPciModernEvtInterruptIsr, VirtioPciModernEvtInterruptDpc);

    /*
     * Intentional: allow true MSI-X multi-vector concurrency.
     *
     * With AutomaticSerialization enabled, KMDF typically serializes ISR/DPC
     * callbacks using the device synchronization scope, which negates the
     * benefit of having a separate MSI-X vector per virtqueue.
     *
     * Safety is provided by explicit per-queue and common_cfg spinlocks.
     */
    config.AutomaticSerialization = FALSE;

    config.InterruptRaw = InterruptRaw;
    config.InterruptTranslated = InterruptTranslated;

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, VIRTIO_INTERRUPT_CONTEXT);
    attributes.ParentObject = Device;

    status = WdfInterruptCreate(Device, &config, &attributes, &interrupt);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    interruptCtx = VirtioPciGetInterruptContext(interrupt);
    interruptCtx->DeviceContext = DevCtx;
    interruptCtx->Kind = Kind;
    interruptCtx->Queue = Queue;
    interruptCtx->MsixVector = MsixVector;

    *InterruptOut = interrupt;
    return STATUS_SUCCESS;
}

static VOID
VirtioPciModernSelectQueueLocked(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx, _In_ USHORT QueueIndex)
{
    /*
     * Access to queue_select must be serialized because it is global state
     * shared by all queue-specific common_cfg fields.
     *
     * Callers must hold DevCtx->CommonCfgLock.
     */
    WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->queue_select, QueueIndex);
    (VOID)READ_REGISTER_USHORT(&DevCtx->CommonCfg->queue_select);
}

static NTSTATUS
VirtioPciModernDisableDeviceVectors(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx)
{
    ULONG i;

    if (DevCtx->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    WdfSpinLockAcquire(DevCtx->CommonCfgLock);

    WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->msix_config, VIRTIO_MSI_NO_VECTOR);
    (VOID)READ_REGISTER_USHORT(&DevCtx->CommonCfg->msix_config);

    for (i = 0; i < DevCtx->QueueCount; i++) {
        VirtioPciModernSelectQueueLocked(DevCtx, DevCtx->Queues[i].QueueIndex);
        WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->queue_msix_vector, VIRTIO_MSI_NO_VECTOR);
        (VOID)READ_REGISTER_USHORT(&DevCtx->CommonCfg->queue_msix_vector);
    }

    WdfSpinLockRelease(DevCtx->CommonCfgLock);

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtioPciModernApplyStoredDeviceVectors(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx)
{
    ULONG i;

    if (DevCtx->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    WdfSpinLockAcquire(DevCtx->CommonCfgLock);

    WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->msix_config, DevCtx->ConfigMsixVector);
    if (READ_REGISTER_USHORT(&DevCtx->CommonCfg->msix_config) == VIRTIO_MSI_NO_VECTOR &&
        DevCtx->ConfigMsixVector != VIRTIO_MSI_NO_VECTOR) {
        WdfSpinLockRelease(DevCtx->CommonCfgLock);
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    for (i = 0; i < DevCtx->QueueCount; i++) {
        VirtioPciModernSelectQueueLocked(DevCtx, DevCtx->Queues[i].QueueIndex);
        WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->queue_msix_vector, DevCtx->Queues[i].MsixVector);
        if (READ_REGISTER_USHORT(&DevCtx->CommonCfg->queue_msix_vector) == VIRTIO_MSI_NO_VECTOR &&
            DevCtx->Queues[i].MsixVector != VIRTIO_MSI_NO_VECTOR) {
            WdfSpinLockRelease(DevCtx->CommonCfgLock);
            return STATUS_DEVICE_HARDWARE_ERROR;
        }
    }

    WdfSpinLockRelease(DevCtx->CommonCfgLock);
    return STATUS_SUCCESS;
}

static VOID
VirtioPciModernDrainUsedRingLocked(_Inout_ PVIRTIO_QUEUE Queue)
{
    volatile const VIRTQ_USED* used;

    used = Queue->UsedRing;
    if (used == NULL || Queue->QueueSize == 0) {
        return;
    }

    for (;;) {
        USHORT deviceIdx = used->idx;
        KeMemoryBarrier();

        if (Queue->LastUsedIdx == deviceIdx) {
            break;
        }

        USHORT slot = (USHORT)(Queue->LastUsedIdx % Queue->QueueSize);
        VIRTQ_USED_ELEM elem = used->ring[slot];

        Queue->LastUsedIdx++;

        if (Queue->EvtUsed != NULL) {
            Queue->EvtUsed(Queue, elem.id, elem.len, Queue->EvtUsedContext);
        }
    }
}

static VOID
VirtioPciModernHandleQueueDpc(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx, _Inout_ PVIRTIO_QUEUE Queue)
{
    WdfSpinLockAcquire(Queue->Lock);

    if (InterlockedCompareExchange(&DevCtx->ResetInProgress, 0, 0) != 0) {
        WdfSpinLockRelease(Queue->Lock);
        return;
    }

    VirtioPciModernDrainUsedRingLocked(Queue);

    WdfSpinLockRelease(Queue->Lock);
}

static VOID
VirtioPciModernHandleConfigDpc(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx)
{
    /*
     * Config-change DPCs are device-specific. We still take CommonCfgLock so
     * that reset/vector-programming can synchronize against config DPCs.
     */
    WdfSpinLockAcquire(DevCtx->CommonCfgLock);

    if (InterlockedCompareExchange(&DevCtx->ResetInProgress, 0, 0) == 0 && DevCtx->CommonCfg != NULL) {
        (VOID)READ_REGISTER_UCHAR(&DevCtx->CommonCfg->config_generation);
    }

    WdfSpinLockRelease(DevCtx->CommonCfgLock);
}

_Use_decl_annotations_
BOOLEAN
VirtioPciModernEvtInterruptIsr(WDFINTERRUPT Interrupt, ULONG MessageID)
{
    PVIRTIO_INTERRUPT_CONTEXT interruptCtx;
    PVIRTIO_PCI_DEVICE_CONTEXT devCtx;

    UNREFERENCED_PARAMETER(MessageID);

    interruptCtx = VirtioPciGetInterruptContext(Interrupt);
    devCtx = interruptCtx->DeviceContext;

    if (devCtx != NULL && InterlockedCompareExchange(&devCtx->ResetInProgress, 0, 0) != 0) {
        return TRUE;
    }

    (VOID)WdfInterruptQueueDpcForIsr(Interrupt);
    return TRUE;
}

_Use_decl_annotations_
VOID
VirtioPciModernEvtInterruptDpc(WDFINTERRUPT Interrupt, WDFOBJECT AssociatedObject)
{
    PVIRTIO_INTERRUPT_CONTEXT interruptCtx;
    PVIRTIO_PCI_DEVICE_CONTEXT devCtx;

    UNREFERENCED_PARAMETER(AssociatedObject);

    interruptCtx = VirtioPciGetInterruptContext(Interrupt);
    devCtx = interruptCtx->DeviceContext;

    if (devCtx == NULL) {
        return;
    }

    if (interruptCtx->Kind == VirtioInterruptKindQueue && interruptCtx->Queue != NULL) {
        VirtioPciModernHandleQueueDpc(devCtx, interruptCtx->Queue);
        return;
    }

    VirtioPciModernHandleConfigDpc(devCtx);
}

_Use_decl_annotations_
NTSTATUS
VirtioPciModernProgramMsixVectors(
    PVIRTIO_PCI_DEVICE_CONTEXT DevCtx,
    USHORT ConfigVector,
    const USHORT* QueueVectors
)
{
    ULONG i;

    if (DevCtx->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (QueueVectors == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    WdfSpinLockAcquire(DevCtx->CommonCfgLock);

    WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->msix_config, ConfigVector);
    if (READ_REGISTER_USHORT(&DevCtx->CommonCfg->msix_config) == VIRTIO_MSI_NO_VECTOR &&
        ConfigVector != VIRTIO_MSI_NO_VECTOR) {
        WdfSpinLockRelease(DevCtx->CommonCfgLock);
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    DevCtx->ConfigMsixVector = ConfigVector;

    for (i = 0; i < DevCtx->QueueCount; i++) {
        USHORT vector = QueueVectors[i];

        VirtioPciModernSelectQueueLocked(DevCtx, DevCtx->Queues[i].QueueIndex);
        WRITE_REGISTER_USHORT(&DevCtx->CommonCfg->queue_msix_vector, vector);

        if (READ_REGISTER_USHORT(&DevCtx->CommonCfg->queue_msix_vector) == VIRTIO_MSI_NO_VECTOR &&
            vector != VIRTIO_MSI_NO_VECTOR) {
            WdfSpinLockRelease(DevCtx->CommonCfgLock);
            return STATUS_DEVICE_HARDWARE_ERROR;
        }

        DevCtx->Queues[i].MsixVector = vector;
    }

    WdfSpinLockRelease(DevCtx->CommonCfgLock);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS
VirtioPciModernQuiesceInterrupts(PVIRTIO_PCI_DEVICE_CONTEXT DevCtx)
{
    NTSTATUS status = STATUS_SUCCESS;
    ULONG i;

    /*
     * Prevent DPC handlers from touching queue state while we disable vectors
     * and (potentially) reset/reconfigure the device.
     */
    InterlockedExchange(&DevCtx->ResetInProgress, 1);

    /*
     * Disable OS-level delivery first so no new DPCs are queued while we
     * reprogram virtio MSI-X vectors.
     */
    for (i = 0; i < DevCtx->InterruptCount; i++) {
        if (DevCtx->Interrupts[i] != NULL) {
            NTSTATUS disableStatus = WdfInterruptDisable(DevCtx->Interrupts[i]);
            if (!NT_SUCCESS(disableStatus) && NT_SUCCESS(status)) {
                status = disableStatus;
            }
        }
    }

    /*
     * Disable device-level vector routing. This prevents MSI-X messages from
     * being generated against partially initialized queue state.
     */
    {
        NTSTATUS vectorStatus = VirtioPciModernDisableDeviceVectors(DevCtx);
        if (!NT_SUCCESS(vectorStatus) && NT_SUCCESS(status)) {
            status = vectorStatus;
        }
    }

    /*
     * Synchronize with any in-flight queue DPC work by forcing entry/exit of
     * each queue's critical section.
     */
    for (i = 0; i < DevCtx->QueueCount; i++) {
        WdfSpinLockAcquire(DevCtx->Queues[i].Lock);
        WdfSpinLockRelease(DevCtx->Queues[i].Lock);
    }

    return status;
}

_Use_decl_annotations_
NTSTATUS
VirtioPciModernResumeInterrupts(PVIRTIO_PCI_DEVICE_CONTEXT DevCtx)
{
    NTSTATUS status;
    ULONG i;

    /*
     * Re-apply vector programming before enabling OS interrupt delivery.
     * The vectors are stored in DevCtx->ConfigMsixVector and Queue->MsixVector.
     */
    status = VirtioPciModernApplyStoredDeviceVectors(DevCtx);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    for (i = 0; i < DevCtx->InterruptCount; i++) {
        if (DevCtx->Interrupts[i] != NULL) {
            NTSTATUS enableStatus = WdfInterruptEnable(DevCtx->Interrupts[i]);
            if (!NT_SUCCESS(enableStatus)) {
                return enableStatus;
            }
        }
    }

    InterlockedExchange(&DevCtx->ResetInProgress, 0);
    return STATUS_SUCCESS;
}

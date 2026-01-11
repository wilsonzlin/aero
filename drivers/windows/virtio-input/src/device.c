#include "virtio_input.h"
#include "virtio_input_proto.h"
#include "virtqueue_split.h"

#include <wdmguid.h>

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

static VOID VioInputSetDeviceKind(_Inout_ PDEVICE_CONTEXT Ctx, _In_ VIOINPUT_DEVICE_KIND Kind);

static VOID VioInputInputLock(_In_opt_ PVOID Context)
{
    if (Context == NULL) {
        return;
    }

    WdfSpinLockAcquire((WDFSPINLOCK)Context);
}

static VOID VioInputInputUnlock(_In_opt_ PVOID Context)
{
    if (Context == NULL) {
        return;
    }

    WdfSpinLockRelease((WDFSPINLOCK)Context);
}

static NTSTATUS VioInputReadPciIdentity(_Inout_ PDEVICE_CONTEXT Ctx)
{
    ULONG bytesRead;
    UCHAR revision;
    ULONG subsys;
    USHORT subsysDeviceId;

    if (Ctx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Ctx->PciDevice.PciInterface.ReadConfig == NULL) {
        return STATUS_NOT_SUPPORTED;
    }

    revision = 0;
    bytesRead = Ctx->PciDevice.PciInterface.ReadConfig(Ctx->PciDevice.PciInterface.Context,
                                                       PCI_WHICHSPACE_CONFIG,
                                                       &revision,
                                                       0x08,
                                                       sizeof(revision));
    if (bytesRead != sizeof(revision)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    subsys = 0;
    bytesRead = Ctx->PciDevice.PciInterface.ReadConfig(Ctx->PciDevice.PciInterface.Context,
                                                       PCI_WHICHSPACE_CONFIG,
                                                       &subsys,
                                                       0x2C,
                                                       sizeof(subsys));
    if (bytesRead != sizeof(subsys)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    subsysDeviceId = (USHORT)(subsys >> 16);

    Ctx->PciRevisionId = revision;
    Ctx->PciSubsystemDeviceId = subsysDeviceId;

    if (revision != 0x01) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "unsupported PCI Revision ID 0x%02X\n", (ULONG)revision);
        return STATUS_NOT_SUPPORTED;
    }

    if (subsysDeviceId == VIOINPUT_PCI_SUBSYSTEM_ID_KEYBOARD) {
        VioInputSetDeviceKind(Ctx, VioInputDeviceKindKeyboard);
    } else if (subsysDeviceId == VIOINPUT_PCI_SUBSYSTEM_ID_MOUSE) {
        VioInputSetDeviceKind(Ctx, VioInputDeviceKindMouse);
    } else {
        VioInputSetDeviceKind(Ctx, VioInputDeviceKindUnknown);
    }

    return STATUS_SUCCESS;
}

static VOID VioInputSetDeviceKind(_Inout_ PDEVICE_CONTEXT Ctx, _In_ VIOINPUT_DEVICE_KIND Kind)
{
    if (Ctx == NULL) {
        return;
    }

    Ctx->DeviceKind = Kind;
    switch (Kind) {
    case VioInputDeviceKindKeyboard:
        virtio_input_device_set_enabled_reports(&Ctx->InputDevice, HID_TRANSLATE_REPORT_MASK_KEYBOARD);
        break;
    case VioInputDeviceKindMouse:
        virtio_input_device_set_enabled_reports(&Ctx->InputDevice, HID_TRANSLATE_REPORT_MASK_MOUSE);
        break;
    default:
        virtio_input_device_set_enabled_reports(&Ctx->InputDevice, HID_TRANSLATE_REPORT_MASK_ALL);
        break;
    }
}

static BOOLEAN VioInputAsciiEqualsInsensitiveZ(_In_z_ const CHAR* A, _In_z_ const CHAR* B)
{
    CHAR ca;
    CHAR cb;

    if (A == NULL || B == NULL) {
        return FALSE;
    }

    while (*A != '\0' && *B != '\0') {
        ca = *A++;
        cb = *B++;

        if (ca >= 'A' && ca <= 'Z') {
            ca = (CHAR)(ca - 'A' + 'a');
        }
        if (cb >= 'A' && cb <= 'Z') {
            cb = (CHAR)(cb - 'A' + 'a');
        }

        if (ca != cb) {
            return FALSE;
        }
    }

    return (*A == '\0') && (*B == '\0');
}

static NTSTATUS VioInputQueryInputConfig(
    _Inout_ PDEVICE_CONTEXT Ctx,
    _In_ UCHAR Select,
    _In_ UCHAR Subsel,
    _Out_writes_bytes_(OutBytes) UCHAR* Out,
    _In_ ULONG OutBytes,
    _Out_opt_ UCHAR* SizeOut)
{
    NTSTATUS status;
    UCHAR selectBytes[2];
    VIRTIO_INPUT_CONFIG cfg;
    UCHAR size;
    ULONG copyLen;

    if (SizeOut != NULL) {
        *SizeOut = 0;
    }

    if (Ctx == NULL || Out == NULL || OutBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    selectBytes[0] = Select;
    selectBytes[1] = Subsel;

    status = VirtioPciWriteDeviceConfig(&Ctx->PciDevice, 0, selectBytes, sizeof(selectBytes));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(&cfg, sizeof(cfg));
    status = VirtioPciReadDeviceConfig(&Ctx->PciDevice, 0, &cfg, sizeof(cfg));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    size = cfg.Size;
    if (size > sizeof(cfg.Payload)) {
        size = sizeof(cfg.Payload);
    }

    copyLen = (ULONG)size;
    if (copyLen > OutBytes) {
        copyLen = OutBytes;
    }

    RtlCopyMemory(Out, cfg.Payload, copyLen);
    if (copyLen < OutBytes) {
        RtlZeroMemory(Out + copyLen, OutBytes - copyLen);
    }

    if (SizeOut != NULL) {
        *SizeOut = size;
    }

    return STATUS_SUCCESS;
}

static VOID VioInputEventQUninitialize(_Inout_ PDEVICE_CONTEXT Ctx)
{
    if (Ctx == NULL) {
        return;
    }

    if (Ctx->EventRxCommonBuffer != NULL) {
        WdfObjectDelete(Ctx->EventRxCommonBuffer);
        Ctx->EventRxCommonBuffer = NULL;
    }

    if (Ctx->EventRingCommonBuffer != NULL) {
        WdfObjectDelete(Ctx->EventRingCommonBuffer);
        Ctx->EventRingCommonBuffer = NULL;
    }

    if (Ctx->EventVq != NULL) {
        ExFreePoolWithTag(Ctx->EventVq, VIOINPUT_POOL_TAG);
        Ctx->EventVq = NULL;
    }

    Ctx->EventRxVa = NULL;
    Ctx->EventRxPa = 0;
    Ctx->EventQueueSize = 0;
}

static NTSTATUS VioInputEventQInitialize(_Inout_ PDEVICE_CONTEXT Ctx, _In_ WDFDMAENABLER DmaEnabler, _In_ USHORT QueueSize)
{
    NTSTATUS status;
    WDF_OBJECT_ATTRIBUTES attributes;
    size_t vqBytes;
    size_t ringBytes;
    PVOID ringVa;
    PHYSICAL_ADDRESS ringPa;
    size_t rxBytes;
    PHYSICAL_ADDRESS rxPa;

    if (Ctx == NULL || DmaEnabler == NULL || QueueSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    VioInputEventQUninitialize(Ctx);

    vqBytes = VirtqSplitStateSize(QueueSize);
    Ctx->EventVq = (VIRTQ_SPLIT*)ExAllocatePoolWithTag(NonPagedPool, vqBytes, VIOINPUT_POOL_TAG);
    if (Ctx->EventVq == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    ringBytes = VirtqSplitRingMemSize(QueueSize, 4, FALSE);
    if (ringBytes == 0) {
        VioInputEventQUninitialize(Ctx);
        return STATUS_INVALID_PARAMETER;
    }

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Ctx->PciDevice.WdfDevice;

    status = WdfCommonBufferCreate(DmaEnabler, ringBytes, &attributes, &Ctx->EventRingCommonBuffer);
    if (!NT_SUCCESS(status)) {
        VioInputEventQUninitialize(Ctx);
        return status;
    }

    ringVa = WdfCommonBufferGetAlignedVirtualAddress(Ctx->EventRingCommonBuffer);
    ringPa = WdfCommonBufferGetAlignedLogicalAddress(Ctx->EventRingCommonBuffer);
    RtlZeroMemory(ringVa, ringBytes);

    status = VirtqSplitInit(Ctx->EventVq, QueueSize, FALSE, TRUE, ringVa, (UINT64)ringPa.QuadPart, 4, NULL, 0, 0, 0);
    if (!NT_SUCCESS(status)) {
        VioInputEventQUninitialize(Ctx);
        return status;
    }

    rxBytes = (size_t)QueueSize * sizeof(struct virtio_input_event_le);
    status = WdfCommonBufferCreate(DmaEnabler, rxBytes, &attributes, &Ctx->EventRxCommonBuffer);
    if (!NT_SUCCESS(status)) {
        VioInputEventQUninitialize(Ctx);
        return status;
    }

    Ctx->EventRxVa = WdfCommonBufferGetAlignedVirtualAddress(Ctx->EventRxCommonBuffer);
    rxPa = WdfCommonBufferGetAlignedLogicalAddress(Ctx->EventRxCommonBuffer);
    Ctx->EventRxPa = (UINT64)rxPa.QuadPart;
    RtlZeroMemory(Ctx->EventRxVa, rxBytes);

    Ctx->EventQueueSize = QueueSize;
    return STATUS_SUCCESS;
}

static NTSTATUS VioInputEventQPostRxBuffersLocked(_Inout_ PDEVICE_CONTEXT Ctx)
{
    NTSTATUS status;
    USHORT i;

    if (Ctx == NULL || Ctx->EventVq == NULL || Ctx->EventQueueSize == 0 || Ctx->EventRxVa == NULL || Ctx->EventRxPa == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    for (i = 0; i < Ctx->EventQueueSize; i++) {
        VIRTQ_SG sg;
        UINT16 head;
        PUCHAR bufVa;
        UINT64 bufPa;

        bufVa = (PUCHAR)Ctx->EventRxVa + ((SIZE_T)i * sizeof(struct virtio_input_event_le));
        bufPa = Ctx->EventRxPa + ((UINT64)i * (UINT64)sizeof(struct virtio_input_event_le));

        sg.addr = bufPa;
        sg.len = (UINT32)sizeof(struct virtio_input_event_le);
        sg.write = TRUE;

        head = VIRTQ_SPLIT_NO_DESC;
        status = VirtqSplitAddBuffer(Ctx->EventVq, &sg, 1, bufVa, &head);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        VirtqSplitPublish(Ctx->EventVq, head);
    }

    VirtioPciNotifyQueue(&Ctx->PciDevice, 0);
    VirtqSplitKickCommit(Ctx->EventVq);
    return STATUS_SUCCESS;
}

static VOID VioInputEventQProcessUsedBuffersLocked(_Inout_ PDEVICE_CONTEXT Ctx)
{
    NTSTATUS status;
    UINT32 reposted;
    const SIZE_T bufBytes = sizeof(struct virtio_input_event_le);
    PUCHAR base;
    PUCHAR end;

    if (Ctx == NULL || Ctx->EventVq == NULL || Ctx->EventRxVa == NULL || Ctx->EventRxPa == 0 || Ctx->EventQueueSize == 0) {
        return;
    }

    base = (PUCHAR)Ctx->EventRxVa;
    end = base + ((SIZE_T)Ctx->EventQueueSize * bufBytes);

    reposted = 0;
    for (;;) {
        void* cookie;
        UINT32 len;

        cookie = NULL;
        len = 0;

        status = VirtqSplitGetUsed(Ctx->EventVq, &cookie, &len);
        if (status == STATUS_NOT_FOUND) {
            break;
        }
        if (!NT_SUCCESS(status)) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "eventq VirtqSplitGetUsed failed: %!STATUS!\n", status);
            break;
        }

        if (cookie == NULL) {
            VioInputCounterInc(&Ctx->Counters.VirtioEventDrops);
            continue;
        }

        if (len < (UINT32)bufBytes) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "eventq used buffer too small: len=%lu (need %Iu)\n",
                (ULONG)len,
                bufBytes);
            VioInputCounterInc(&Ctx->Counters.VirtioEventDrops);
        } else if (Ctx->VirtioStarted != 0 && VirtioInputIsHidActive(Ctx)) {
            virtio_input_process_event_le(&Ctx->InputDevice, (const struct virtio_input_event_le*)cookie);
        }

        {
            VIRTQ_SG sg;
            UINT16 head;
            PUCHAR p = (PUCHAR)cookie;
            SIZE_T off;

            if (p < base || (p + bufBytes) > end) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "eventq cookie out of range\n");
                VioInputCounterInc(&Ctx->Counters.VirtioEventDrops);
                continue;
            }

            off = (SIZE_T)(p - base);

            sg.addr = Ctx->EventRxPa + (UINT64)off;
            sg.len = (UINT32)bufBytes;
            sg.write = TRUE;

            head = VIRTQ_SPLIT_NO_DESC;
            status = VirtqSplitAddBuffer(Ctx->EventVq, &sg, 1, cookie, &head);
            if (!NT_SUCCESS(status)) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "eventq VirtqSplitAddBuffer failed: %!STATUS!\n", status);
                VioInputCounterInc(&Ctx->Counters.VirtioEventDrops);
                continue;
            }

            VirtqSplitPublish(Ctx->EventVq, head);
            reposted++;
        }
    }

    if (reposted != 0) {
        VirtioPciNotifyQueue(&Ctx->PciDevice, 0);
        VirtqSplitKickCommit(Ctx->EventVq);
    }
}

static VOID VioInputEvtConfigChange(_In_ WDFDEVICE Device, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(Device);
    PDEVICE_CONTEXT devCtx = (PDEVICE_CONTEXT)Context;
    LONG cfgCount = -1;
    UCHAR gen = 0;

    if (devCtx != NULL) {
        cfgCount = InterlockedIncrement(&devCtx->ConfigInterruptCount);
        if (devCtx->PciDevice.CommonCfg != NULL) {
            gen = READ_REGISTER_UCHAR(&devCtx->PciDevice.CommonCfg->config_generation);
        }
    }

    VIOINPUT_LOG(
        VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
        "config change interrupt: gen=%u cfgIrqs=%ld interrupts=%ld dpcs=%ld\n",
        (ULONG)gen,
        cfgCount,
        devCtx ? devCtx->Counters.VirtioInterrupts : -1,
        devCtx ? devCtx->Counters.VirtioDpcs : -1);
}

static VOID VioInputEvtDrainQueue(_In_ WDFDEVICE Device, _In_ ULONG QueueIndex, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(Device);
    PDEVICE_CONTEXT devCtx = (PDEVICE_CONTEXT)Context;
    LONG queueCount = -1;

    if (devCtx != NULL && QueueIndex < VIRTIO_INPUT_QUEUE_COUNT) {
        queueCount = InterlockedIncrement(&devCtx->QueueInterruptCount[QueueIndex]);
    }

    /*
     * Queue 0 is the eventq (device -> driver).
     * Queue 1 is the statusq (driver -> device, e.g. keyboard LEDs).
     *
     * The virtqueue implementation is wired in elsewhere; the interrupt plumbing
     * calls into the relevant queue handlers here.
     */
    if (devCtx != NULL && devCtx->VirtioStarted != 0) {
        if (QueueIndex == 0) {
            VioInputEventQProcessUsedBuffersLocked(devCtx);
        } else if (QueueIndex == 1) {
            VirtioStatusQProcessUsedBuffers(devCtx->StatusQ);
        }
    }

    VIOINPUT_LOG(
        VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
        "queue interrupt: index=%lu queueIrqs=%ld interrupts=%ld dpcs=%ld\n",
        QueueIndex,
        queueCount,
        devCtx ? devCtx->Counters.VirtioInterrupts : -1,
        devCtx ? devCtx->Counters.VirtioDpcs : -1);
}

static VOID VioInputDrainInputReportRing(_In_ PDEVICE_CONTEXT Ctx)
{
    struct virtio_input_report report;

    if (Ctx == NULL) {
        return;
    }

    while (virtio_input_try_pop_report(&Ctx->InputDevice, &report)) {
    }
}

static void VirtioInputReportReady(_In_ void *context)
{
    WDFDEVICE device = (WDFDEVICE)context;
    PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(device);
    struct virtio_input_report report;
    ULONG drained = 0;

    VIOINPUT_LOG(
        VIOINPUT_LOG_VIRTQ,
        "report ready: virtioEvents=%ld ring=%ld pending=%ld drops=%ld overruns=%ld\n",
        deviceContext->Counters.VirtioEvents,
        deviceContext->Counters.ReportRingDepth,
        deviceContext->Counters.ReadReportQueueDepth,
        deviceContext->Counters.VirtioEventDrops,
        deviceContext->Counters.VirtioEventOverruns);

    while (virtio_input_try_pop_report(&deviceContext->InputDevice, &report)) {
        if (report.len == 0) {
            continue;
        }

        drained++;
        NTSTATUS status = VirtioInputReportArrived(device, report.data[0], report.data, report.len);
        if (!NT_SUCCESS(status)) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "ReportArrived failed: reportId=%u size=%u status=%!STATUS!\n",
                (ULONG)report.data[0],
                (ULONG)report.len,
                status);
        }
    }

    if (drained != 0) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_VIRTQ,
            "report ready drained=%lu ring=%ld pending=%ld\n",
            drained,
            deviceContext->Counters.ReportRingDepth,
            deviceContext->Counters.ReadReportQueueDepth);
    }
}

static VOID VirtioInputEvtDeviceSurpriseRemoval(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT ctx = VirtioInputGetDeviceContext(Device);

    ctx->VirtioStarted = 0;
    ctx->InD0 = FALSE;
    VirtioInputApplyTransportState(ctx);

    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_CANCELLED);
    VioInputDrainInputReportRing(ctx);

    if (ctx->PciDevice.CommonCfg != NULL) {
        VirtioPciResetDevice(&ctx->PciDevice);
    }
}

NTSTATUS VirtioInputEvtDriverDeviceAdd(_In_ WDFDRIVER Driver, _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    WDF_PNPPOWER_EVENT_CALLBACKS pnpPowerCallbacks;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDFDEVICE device;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Driver);

    PAGED_CODE();

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnpPowerCallbacks);
    pnpPowerCallbacks.EvtDevicePrepareHardware = VirtioInputEvtDevicePrepareHardware;
    pnpPowerCallbacks.EvtDeviceReleaseHardware = VirtioInputEvtDeviceReleaseHardware;
    pnpPowerCallbacks.EvtDeviceD0Entry = VirtioInputEvtDeviceD0Entry;
    pnpPowerCallbacks.EvtDeviceD0Exit = VirtioInputEvtDeviceD0Exit;
    pnpPowerCallbacks.EvtDeviceSurpriseRemoval = VirtioInputEvtDeviceSurpriseRemoval;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnpPowerCallbacks);

    /* Internal HID IOCTLs use the request's buffers directly; keep it simple for now. */
    WdfDeviceInitSetIoType(DeviceInit, WdfDeviceIoBuffered);

    status = VirtioInputFileConfigure(DeviceInit);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DEVICE_CONTEXT);
    attributes.ExecutionLevel = WdfExecutionLevelPassive;

    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    {
        PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(device);
        VioInputCountersInit(&deviceContext->Counters);

        deviceContext->HardwareReady = FALSE;
        deviceContext->InD0 = FALSE;
        deviceContext->HidActivated = FALSE;
        deviceContext->VirtioStarted = 0;
        deviceContext->NumDeviceInputBuffers = 0;
        deviceContext->DeviceKind = VioInputDeviceKindUnknown;
        deviceContext->PciSubsystemDeviceId = 0;
        deviceContext->PciRevisionId = 0;

        status = VirtioInputReadReportQueuesInitialize(device);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        RtlZeroMemory(&deviceContext->PciDevice, sizeof(deviceContext->PciDevice));
        RtlZeroMemory(&deviceContext->Interrupts, sizeof(deviceContext->Interrupts));
        deviceContext->ConfigInterruptCount = 0;
        RtlZeroMemory(deviceContext->QueueInterruptCount, sizeof(deviceContext->QueueInterruptCount));
        deviceContext->DmaEnabler = NULL;
        deviceContext->NegotiatedFeatures = 0;
        deviceContext->EventVq = NULL;
        deviceContext->EventRingCommonBuffer = NULL;
        deviceContext->EventRxCommonBuffer = NULL;
        deviceContext->EventRxVa = NULL;
        deviceContext->EventRxPa = 0;
        deviceContext->EventQueueSize = 0;

        {
            WDF_OBJECT_ATTRIBUTES lockAttributes;

            WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
            lockAttributes.ParentObject = device;
            status = WdfSpinLockCreate(&lockAttributes, &deviceContext->InputLock);
            if (!NT_SUCCESS(status)) {
                return status;
            }
        }

        virtio_input_device_init(
            &deviceContext->InputDevice,
            VirtioInputReportReady,
            (void *)device,
            VioInputInputLock,
            VioInputInputUnlock,
            deviceContext->InputLock);
    }

    {
        WDF_DMA_ENABLER_CONFIG dmaConfig;
        WDF_DMA_ENABLER_CONFIG_INIT(&dmaConfig, WdfDmaProfileScatterGather64, 0x10000);

        status = WdfDmaEnablerCreate(device, &dmaConfig, WDF_NO_OBJECT_ATTRIBUTES, &VirtioInputGetDeviceContext(device)->DmaEnabler);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    return VirtioInputQueueInitialize(device);
}

static VOID VirtioInputApplyTransportState(_In_ PDEVICE_CONTEXT DeviceContext)
{
    BOOLEAN active;

    active = VirtioInputIsHidActive(DeviceContext);

    if (DeviceContext->StatusQ == NULL) {
        return;
    }

    if (DeviceContext->Interrupts.QueueLocks != NULL && DeviceContext->Interrupts.QueueCount > 1) {
        WdfSpinLockAcquire(DeviceContext->Interrupts.QueueLocks[1]);
        VirtioStatusQSetActive(DeviceContext->StatusQ, active);
        WdfSpinLockRelease(DeviceContext->Interrupts.QueueLocks[1]);
    } else {
        VirtioStatusQSetActive(DeviceContext->StatusQ, active);
    }
}

NTSTATUS VirtioInputEvtDevicePrepareHardware(
    _In_ WDFDEVICE Device,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    PDEVICE_CONTEXT deviceContext;
    NTSTATUS status;
    UCHAR revisionId;
    VIRTIO_PCI_AERO_CONTRACT_V1_LAYOUT_FAILURE layoutFailure;

    PAGED_CODE();

    deviceContext = VirtioInputGetDeviceContext(Device);
    RtlZeroMemory(&deviceContext->PciDevice, sizeof(deviceContext->PciDevice));
    RtlZeroMemory(&deviceContext->Interrupts, sizeof(deviceContext->Interrupts));
    deviceContext->ConfigInterruptCount = 0;
    RtlZeroMemory(deviceContext->QueueInterruptCount, sizeof(deviceContext->QueueInterruptCount));
    deviceContext->HardwareReady = FALSE;
    deviceContext->InD0 = FALSE;
    deviceContext->VirtioStarted = 0;
    deviceContext->NegotiatedFeatures = 0;

    status = VirtioPciModernInit(Device, &deviceContext->PciDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    revisionId = 0;
    status = VirtioPciModernValidateAeroContractV1RevisionId(&deviceContext->PciDevice, &revisionId);
    if (!NT_SUCCESS(status)) {
        if (status == STATUS_NOT_SUPPORTED) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "unsupported Aero virtio contract revision ID 0x%02X (expected 0x%02X)\n",
                (ULONG)revisionId,
                (ULONG)VIRTIO_PCI_AERO_CONTRACT_V1_REVISION_ID);
        } else {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "VirtioPciModernValidateAeroContractV1RevisionId failed: %!STATUS!\n",
                status);
        }

        VirtioPciModernUninit(&deviceContext->PciDevice);
        return status;
    }

    status = VirtioPciModernMapBars(&deviceContext->PciDevice, ResourcesRaw, ResourcesTranslated);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUninit(&deviceContext->PciDevice);
        return status;
    }

    layoutFailure = VirtioPciAeroContractV1LayoutFailureNone;
    status = VirtioPciModernValidateAeroContractV1FixedLayout(&deviceContext->PciDevice, &layoutFailure);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "Aero contract v1 fixed-layout validation failed: %s\n",
            VirtioPciAeroContractV1LayoutFailureToString(layoutFailure));
        VirtioPciModernUninit(&deviceContext->PciDevice);
        return status;
    }

    status = VioInputReadPciIdentity(deviceContext);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUninit(&deviceContext->PciDevice);
        return status;
    }

    {
        USHORT numQueues = READ_REGISTER_USHORT(&deviceContext->PciDevice.CommonCfg->num_queues);
        if (numQueues < VIRTIO_INPUT_QUEUE_COUNT) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input reports only %u queues (need %u)\n",
                numQueues,
                (USHORT)VIRTIO_INPUT_QUEUE_COUNT);
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    {
        USHORT qsz0;
        USHORT qsz1;

        qsz0 = 0;
        status = VirtioPciGetQueueSize(&deviceContext->PciDevice, 0, &qsz0);
        if (!NT_SUCCESS(status)) {
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return status;
        }

        qsz1 = 0;
        status = VirtioPciGetQueueSize(&deviceContext->PciDevice, 1, &qsz1);
        if (!NT_SUCCESS(status)) {
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return status;
        }

        if (qsz0 != 64 || qsz1 != 64) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input queue sizes not supported: eventq=%u statusq=%u (need 64/64)\n",
                (ULONG)qsz0,
                (ULONG)qsz1);
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        status = VioInputEventQInitialize(deviceContext, deviceContext->DmaEnabler, qsz0);
        if (!NT_SUCCESS(status)) {
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return status;
        }

        status = VirtioStatusQInitialize(&deviceContext->StatusQ,
                                         Device,
                                         &deviceContext->PciDevice,
                                         deviceContext->DmaEnabler,
                                         1,
                                         qsz1);
        if (!NT_SUCCESS(status)) {
            VioInputEventQUninitialize(deviceContext);
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return status;
        }
    }

    status = VirtioPciInterruptsPrepareHardware(
        Device,
        &deviceContext->Interrupts,
        ResourcesRaw,
        ResourcesTranslated,
        VIRTIO_INPUT_QUEUE_COUNT,
        deviceContext->PciDevice.IsrStatus,
        deviceContext->PciDevice.CommonCfgLock,
        VioInputEvtConfigChange,
        VioInputEvtDrainQueue,
        deviceContext);
    if (!NT_SUCCESS(status)) {
        VirtioPciInterruptsReleaseHardware(&deviceContext->Interrupts);
        VirtioStatusQUninitialize(deviceContext->StatusQ);
        deviceContext->StatusQ = NULL;
        VioInputEventQUninitialize(deviceContext);
        VirtioPciModernUninit(&deviceContext->PciDevice);
        return status;
    }

    deviceContext->Interrupts.InterruptCounter = &deviceContext->Counters.VirtioInterrupts;
    deviceContext->Interrupts.DpcCounter = &deviceContext->Counters.VirtioDpcs;

    deviceContext->HardwareReady = TRUE;
    VirtioInputApplyTransportState(deviceContext);
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceReleaseHardware(_In_ WDFDEVICE Device, _In_ WDFCMRESLIST ResourcesTranslated)
{
    UNREFERENCED_PARAMETER(ResourcesTranslated);

    PAGED_CODE();

    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_DEVICE_NOT_READY);

    {
        PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(Device);
        deviceContext->HardwareReady = FALSE;
        deviceContext->InD0 = FALSE;
        deviceContext->HidActivated = FALSE;
        deviceContext->VirtioStarted = 0;
        VirtioInputApplyTransportState(deviceContext);

        virtio_input_device_reset_state(&deviceContext->InputDevice, false);

        if (deviceContext->PciDevice.CommonCfg != NULL) {
            VirtioPciResetDevice(&deviceContext->PciDevice);
        }

        if (deviceContext->StatusQ != NULL) {
            VirtioStatusQUninitialize(deviceContext->StatusQ);
            deviceContext->StatusQ = NULL;
        }
        VioInputEventQUninitialize(deviceContext);

        VirtioPciInterruptsReleaseHardware(&deviceContext->Interrupts);
        VirtioPciModernUninit(&deviceContext->PciDevice);
    }

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceD0Entry(_In_ WDFDEVICE Device, _In_ WDF_POWER_DEVICE_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);

    PDEVICE_CONTEXT deviceContext;
    NTSTATUS status;
    UINT64 negotiated;
    UINT64 descPa;
    UINT64 availPa;
    UINT64 usedPa;

    deviceContext = VirtioInputGetDeviceContext(Device);

    deviceContext->InD0 = FALSE;
    deviceContext->VirtioStarted = 0;

    if (!deviceContext->HardwareReady) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (deviceContext->EventVq == NULL || deviceContext->StatusQ == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    /*
     * Transport bring-up:
     *  - Negotiate features (includes reset, ACKNOWLEDGE|DRIVER, FEATURES_OK).
     *  - Program MSI-X vectors (if present) AFTER reset.
     *  - Configure queues.
     *  - Post initial RX buffers.
     *  - Set DRIVER_OK.
     */
    negotiated = 0;
    status = VirtioPciNegotiateFeatures(&deviceContext->PciDevice, VIRTIO_F_RING_INDIRECT_DESC, 0, &negotiated);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "VirtioPciNegotiateFeatures failed: %!STATUS!\n", status);
        VirtioPciResetDevice(&deviceContext->PciDevice);
        return status;
    }
    deviceContext->NegotiatedFeatures = negotiated;

    status = VirtioPciInterruptsProgramMsixVectors(&deviceContext->Interrupts, deviceContext->PciDevice.CommonCfg);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "VirtioPciInterruptsProgramMsixVectors failed: %!STATUS!\n",
            status);
        VirtioPciResetDevice(&deviceContext->PciDevice);
        return status;
    }

    /*
     * Device config discovery (contract v1 required selectors).
     *
     * Use ID_NAME as the authoritative keyboard-vs-mouse classification.
     */
    {
        CHAR name[129];
        UCHAR size;

        RtlZeroMemory(name, sizeof(name));
        size = 0;
        status = VioInputQueryInputConfig(deviceContext,
                                          VIRTIO_INPUT_CFG_ID_NAME,
                                          0,
                                          (UCHAR*)name,
                                          (ULONG)(sizeof(name) - 1),
                                          &size);
        if (!NT_SUCCESS(status) || size == 0) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input ID_NAME query failed: %!STATUS!\n", status);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        if (VioInputAsciiEqualsInsensitiveZ(name, "Aero Virtio Keyboard")) {
            VioInputSetDeviceKind(deviceContext, VioInputDeviceKindKeyboard);
        } else if (VioInputAsciiEqualsInsensitiveZ(name, "Aero Virtio Mouse")) {
            VioInputSetDeviceKind(deviceContext, VioInputDeviceKindMouse);
        }

        if (deviceContext->DeviceKind == VioInputDeviceKindUnknown) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input device kind unknown (ID_NAME=%s)\n", name);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        VIOINPUT_LOG(
            VIOINPUT_LOG_VIRTQ,
            "virtio-input config: ID_NAME='%s' pci_subsys=0x%04X kind=%s\n",
            name,
            (ULONG)deviceContext->PciSubsystemDeviceId,
            (deviceContext->DeviceKind == VioInputDeviceKindKeyboard) ? "keyboard" : "mouse");
    }

    {
        VIRTIO_INPUT_DEVIDS ids;
        UCHAR size;
        USHORT expectedProduct;

        RtlZeroMemory(&ids, sizeof(ids));
        size = 0;
        status = VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_ID_DEVIDS, 0, (UCHAR*)&ids, sizeof(ids), &size);
        if (!NT_SUCCESS(status) || size < sizeof(ids)) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input ID_DEVIDS query failed: %!STATUS!\n", status);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        expectedProduct = (deviceContext->DeviceKind == VioInputDeviceKindKeyboard) ? VIRTIO_INPUT_DEVIDS_PRODUCT_KEYBOARD
                                                                                    : VIRTIO_INPUT_DEVIDS_PRODUCT_MOUSE;

        if (ids.Bustype != VIRTIO_INPUT_DEVIDS_BUSTYPE_VIRTUAL || ids.Vendor != VIRTIO_INPUT_DEVIDS_VENDOR_VIRTIO ||
            ids.Product != expectedProduct || ids.Version != VIRTIO_INPUT_DEVIDS_VERSION) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input ID_DEVIDS mismatch: bustype=0x%04X vendor=0x%04X product=0x%04X version=0x%04X (expected bustype=0x%04X vendor=0x%04X product=0x%04X version=0x%04X)\n",
                (ULONG)ids.Bustype,
                (ULONG)ids.Vendor,
                (ULONG)ids.Product,
                (ULONG)ids.Version,
                (ULONG)VIRTIO_INPUT_DEVIDS_BUSTYPE_VIRTUAL,
                (ULONG)VIRTIO_INPUT_DEVIDS_VENDOR_VIRTIO,
                (ULONG)expectedProduct,
                (ULONG)VIRTIO_INPUT_DEVIDS_VERSION);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        VIOINPUT_LOG(
            VIOINPUT_LOG_VIRTQ,
            "virtio-input config: devids bustype=0x%04X vendor=0x%04X product=0x%04X version=0x%04X\n",
            (ULONG)ids.Bustype,
            (ULONG)ids.Vendor,
            (ULONG)ids.Product,
            (ULONG)ids.Version);
    }

    {
        UCHAR bits[128];
        UCHAR size;

        if (deviceContext->DeviceKind == VioInputDeviceKindKeyboard) {
            RtlZeroMemory(bits, sizeof(bits));
            size = 0;

            status = VioInputQueryInputConfig(deviceContext,
                                              VIRTIO_INPUT_CFG_EV_BITS,
                                              VIRTIO_INPUT_EV_KEY,
                                              bits,
                                              sizeof(bits),
                                              &size);
            if (!NT_SUCCESS(status) || size == 0) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input EV_BITS(EV_KEY) query failed: %!STATUS!\n", status);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }
        } else {
            RtlZeroMemory(bits, sizeof(bits));
            size = 0;

            status = VioInputQueryInputConfig(deviceContext,
                                              VIRTIO_INPUT_CFG_EV_BITS,
                                              VIRTIO_INPUT_EV_REL,
                                              bits,
                                              sizeof(bits),
                                              &size);
            if (!NT_SUCCESS(status) || size == 0) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input EV_BITS(EV_REL) query failed: %!STATUS!\n", status);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }

            RtlZeroMemory(bits, sizeof(bits));
            size = 0;

            status = VioInputQueryInputConfig(deviceContext,
                                              VIRTIO_INPUT_CFG_EV_BITS,
                                              VIRTIO_INPUT_EV_KEY,
                                              bits,
                                              sizeof(bits),
                                              &size);
            if (!NT_SUCCESS(status) || size == 0) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input EV_BITS(EV_KEY) query failed: %!STATUS!\n", status);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }
        }
    }

    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
        VirtqSplitReset(deviceContext->EventVq);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
    } else {
        VirtqSplitReset(deviceContext->EventVq);
    }

    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 1) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[1]);
        VirtioStatusQReset(deviceContext->StatusQ);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[1]);
    } else {
        VirtioStatusQReset(deviceContext->StatusQ);
    }

    status = VirtioPciSetupQueue(&deviceContext->PciDevice,
                                 0,
                                 (ULONGLONG)deviceContext->EventVq->desc_pa,
                                 (ULONGLONG)deviceContext->EventVq->avail_pa,
                                 (ULONGLONG)deviceContext->EventVq->used_pa);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "VirtioPciSetupQueue(eventq) failed: %!STATUS!\n", status);
        VirtioPciResetDevice(&deviceContext->PciDevice);
        return status;
    }

    descPa = 0;
    availPa = 0;
    usedPa = 0;
    VirtioStatusQGetRingAddresses(deviceContext->StatusQ, &descPa, &availPa, &usedPa);

    status = VirtioPciSetupQueue(&deviceContext->PciDevice, 1, (ULONGLONG)descPa, (ULONGLONG)availPa, (ULONGLONG)usedPa);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "VirtioPciSetupQueue(statusq) failed: %!STATUS!\n", status);
        VirtioPciResetDevice(&deviceContext->PciDevice);
        return status;
    }

    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
        status = VioInputEventQPostRxBuffersLocked(deviceContext);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
    } else {
        status = VioInputEventQPostRxBuffersLocked(deviceContext);
    }
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "eventq post buffers failed: %!STATUS!\n", status);
        VirtioPciResetDevice(&deviceContext->PciDevice);
        return status;
    }

    {
        BOOLEAN emitResetReports;

        emitResetReports = FALSE;

        VioInputDrainInputReportRing(deviceContext);
        if (deviceContext->HidActivated) {
            VirtioInputReadReportQueuesStart(Device);
            emitResetReports = TRUE;
        } else {
            VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_DEVICE_NOT_READY);
        }

         deviceContext->VirtioStarted = 1;
         VirtioPciAddStatus(&deviceContext->PciDevice, VIRTIO_STATUS_DRIVER_OK);

         virtio_input_device_reset_state(&deviceContext->InputDevice, emitResetReports ? true : false);
         deviceContext->InD0 = TRUE;
     }

    VirtioInputApplyTransportState(deviceContext);
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceD0Exit(_In_ WDFDEVICE Device, _In_ WDF_POWER_DEVICE_STATE TargetState)
{
    UNREFERENCED_PARAMETER(TargetState);

    PDEVICE_CONTEXT deviceContext;

    deviceContext = VirtioInputGetDeviceContext(Device);

    deviceContext->VirtioStarted = 0;
    deviceContext->InD0 = FALSE;

    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_DEVICE_NOT_READY);
    VioInputDrainInputReportRing(deviceContext);
    virtio_input_device_reset_state(&deviceContext->InputDevice, false);

    VirtioInputApplyTransportState(deviceContext);

    if (deviceContext->PciDevice.CommonCfg != NULL) {
        VirtioPciResetDevice(&deviceContext->PciDevice);
    }

    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
        VirtqSplitReset(deviceContext->EventVq);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
    } else {
        VirtqSplitReset(deviceContext->EventVq);
    }

    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 1) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[1]);
        VirtioStatusQReset(deviceContext->StatusQ);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[1]);
    } else {
        VirtioStatusQReset(deviceContext->StatusQ);
    }

    return STATUS_SUCCESS;
}

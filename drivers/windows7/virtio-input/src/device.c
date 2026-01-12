#include "virtio_input.h"
#include "virtio_input_proto.h"
#include "virtqueue_split.h"

#include <wdmguid.h>

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

static VOID VioInputSetDeviceKind(_Inout_ PDEVICE_CONTEXT Ctx, _In_ VIOINPUT_DEVICE_KIND Kind);

/*
 * virtio-input EV_BITS parsing/validation.
 *
 * Aero contract v1 requires virtio-input devices to implement
 * VIRTIO_INPUT_CFG_EV_BITS and advertise a minimum set of supported event
 * codes (see docs/windows7-virtio-driver-contract.md §3.3.4–§3.3.5).
 *
 * The device returns up to 128 bytes of little-endian bitmaps. Bit numbering is
 * per the virtio-input spec (Linux input ABI): bit <code> corresponds to the
 * event code value.
 */
static __forceinline bool VioInputBitmapTestBit(_In_reads_(128) const UCHAR Bits[128], _In_ uint16_t Code)
{
    const uint16_t byteIndex = (uint16_t)(Code / 8u);
    const uint16_t bitIndex = (uint16_t)(Code % 8u);

    if (byteIndex >= 128u) {
        return false;
    }

    return (Bits[byteIndex] & (UCHAR)(1u << bitIndex)) != 0;
}

typedef struct _VIOINPUT_REQUIRED_EV_CODE {
    uint16_t Code;
    PCSTR Name;
} VIOINPUT_REQUIRED_EV_CODE;

static NTSTATUS VioInputValidateEvBitsRequired(
    _In_reads_(128) const UCHAR Bits[128],
    _In_reads_(RequiredCount) const VIOINPUT_REQUIRED_EV_CODE* Required,
    _In_ SIZE_T RequiredCount,
    _In_z_ PCSTR What)
{
    SIZE_T i;
    BOOLEAN ok;

    if (Bits == NULL || Required == NULL || RequiredCount == 0 || What == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    ok = TRUE;
    for (i = 0; i < RequiredCount; ++i) {
        if (!VioInputBitmapTestBit(Bits, Required[i].Code)) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "%s: missing required bit %s (code=%u)\n",
                What,
                Required[i].Name ? Required[i].Name : "<unknown>",
                (ULONG)Required[i].Code);
            ok = FALSE;
        }
    }

    if (!ok) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "%s: device does not satisfy Aero virtio-input EV_BITS contract\n", What);
        return STATUS_NOT_SUPPORTED;
    }

    return STATUS_SUCCESS;
}

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

        if (len != (UINT32)bufBytes) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "eventq used buffer length mismatch: len=%lu (expected %Iu)\n",
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

    /*
     * HID class IOCTLs are METHOD_NEITHER and may embed user-mode pointers even when delivered as internal IOCTLs.
     * The individual IOCTL handlers must probe/lock/map user buffers safely when RequestorMode==UserMode.
     */
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
        WDF_OBJECT_ATTRIBUTES dmaAttributes;
        WDF_DMA_PROFILE profile;
        PDEVICE_CONTEXT ctx;

        ctx = VirtioInputGetDeviceContext(device);

        profile = WdfDmaProfileScatterGather64Duplex;
        WDF_DMA_ENABLER_CONFIG_INIT(&dmaConfig, profile, 0x10000);

        WDF_OBJECT_ATTRIBUTES_INIT(&dmaAttributes);
        dmaAttributes.ParentObject = device;

        status = WdfDmaEnablerCreate(device, &dmaConfig, &dmaAttributes, &ctx->DmaEnabler);
        if (status == STATUS_NOT_SUPPORTED || status == STATUS_INVALID_DEVICE_REQUEST) {
            profile = WdfDmaProfileScatterGatherDuplex;
            WDF_DMA_ENABLER_CONFIG_INIT(&dmaConfig, profile, 0x10000);
            status = WdfDmaEnablerCreate(device, &dmaConfig, &dmaAttributes, &ctx->DmaEnabler);
        }
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    return VirtioInputQueueInitialize(device);
}

static VOID VirtioInputApplyTransportState(_In_ PDEVICE_CONTEXT DeviceContext)
{
    BOOLEAN active;

    active = VirtioInputIsHidActive(DeviceContext) && (DeviceContext->DeviceKind == VioInputDeviceKindKeyboard);

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

    RtlZeroMemory(deviceContext->QueueNotifyAddrCache, sizeof(deviceContext->QueueNotifyAddrCache));
    deviceContext->PciDevice.QueueNotifyAddrCache = deviceContext->QueueNotifyAddrCache;
    deviceContext->PciDevice.QueueNotifyAddrCacheCount = VIRTIO_INPUT_QUEUE_COUNT;

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

    {
        static const USHORT allowedIds[] = { 0x1052 };
        status = VirtioPciModernEnforceDeviceIds(&deviceContext->PciDevice, allowedIds, RTL_NUMBER_OF(allowedIds));
        if (!NT_SUCCESS(status)) {
            VirtioPciModernUninit(&deviceContext->PciDevice);
            return status;
        }
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

        /*
         * Contract v1 requires `queue_notify_off(q) = q` (docs/windows7-virtio-driver-contract.md §1.6).
         *
         * The transport can function with arbitrary notify offsets, but validate
         * this to catch device-model contract regressions early.
         */
        {
            USHORT notifyOff0;
            USHORT notifyOff1;

            notifyOff0 = VirtioPciReadQueueNotifyOffset(&deviceContext->PciDevice, 0);
            notifyOff1 = VirtioPciReadQueueNotifyOffset(&deviceContext->PciDevice, 1);

            if (notifyOff0 != 0 || notifyOff1 != 1) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "virtio-input queue_notify_off mismatch: q0=%u q1=%u (expected 0/1)\n",
                    (ULONG)notifyOff0,
                    (ULONG)notifyOff1);
                VirtioPciModernUninit(&deviceContext->PciDevice);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }
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
    status = VirtioPciNegotiateFeatures(
        &deviceContext->PciDevice,
        (1ui64 << VIRTIO_F_RING_INDIRECT_DESC),
        0,
        &negotiated);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "VirtioPciNegotiateFeatures failed: %!STATUS!\n", status);
        VirtioPciResetDevice(&deviceContext->PciDevice);
        return status;
    }
    deviceContext->NegotiatedFeatures = negotiated;

    /*
     * Contract v1: drivers MUST NOT negotiate EVENT_IDX (split-ring event index).
     * `VirtioPciNegotiateFeatures` only negotiates features explicitly requested,
     * so this should never be set, but keep the check as a guard against future
     * changes.
     */
    if ((negotiated & (1ui64 << VIRTIO_F_RING_EVENT_IDX)) != 0) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "negotiated forbidden feature: EVENT_IDX\n");
        VirtioPciFailDevice(&deviceContext->PciDevice);
        return STATUS_NOT_SUPPORTED;
    }

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
            /*
             * Contract v1: keyboard devices MUST implement EV_BITS(EV_KEY) and
             * advertise at least the minimum required key set.
             */
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredKeys[] = {
                /* KEY_A..KEY_Z */
                {VIRTIO_INPUT_KEY_A, "KEY_A"},
                {VIRTIO_INPUT_KEY_B, "KEY_B"},
                {VIRTIO_INPUT_KEY_C, "KEY_C"},
                {VIRTIO_INPUT_KEY_D, "KEY_D"},
                {VIRTIO_INPUT_KEY_E, "KEY_E"},
                {VIRTIO_INPUT_KEY_F, "KEY_F"},
                {VIRTIO_INPUT_KEY_G, "KEY_G"},
                {VIRTIO_INPUT_KEY_H, "KEY_H"},
                {VIRTIO_INPUT_KEY_I, "KEY_I"},
                {VIRTIO_INPUT_KEY_J, "KEY_J"},
                {VIRTIO_INPUT_KEY_K, "KEY_K"},
                {VIRTIO_INPUT_KEY_L, "KEY_L"},
                {VIRTIO_INPUT_KEY_M, "KEY_M"},
                {VIRTIO_INPUT_KEY_N, "KEY_N"},
                {VIRTIO_INPUT_KEY_O, "KEY_O"},
                {VIRTIO_INPUT_KEY_P, "KEY_P"},
                {VIRTIO_INPUT_KEY_Q, "KEY_Q"},
                {VIRTIO_INPUT_KEY_R, "KEY_R"},
                {VIRTIO_INPUT_KEY_S, "KEY_S"},
                {VIRTIO_INPUT_KEY_T, "KEY_T"},
                {VIRTIO_INPUT_KEY_U, "KEY_U"},
                {VIRTIO_INPUT_KEY_V, "KEY_V"},
                {VIRTIO_INPUT_KEY_W, "KEY_W"},
                {VIRTIO_INPUT_KEY_X, "KEY_X"},
                {VIRTIO_INPUT_KEY_Y, "KEY_Y"},
                {VIRTIO_INPUT_KEY_Z, "KEY_Z"},

                /* KEY_0..KEY_9 */
                {VIRTIO_INPUT_KEY_0, "KEY_0"},
                {VIRTIO_INPUT_KEY_1, "KEY_1"},
                {VIRTIO_INPUT_KEY_2, "KEY_2"},
                {VIRTIO_INPUT_KEY_3, "KEY_3"},
                {VIRTIO_INPUT_KEY_4, "KEY_4"},
                {VIRTIO_INPUT_KEY_5, "KEY_5"},
                {VIRTIO_INPUT_KEY_6, "KEY_6"},
                {VIRTIO_INPUT_KEY_7, "KEY_7"},
                {VIRTIO_INPUT_KEY_8, "KEY_8"},
                {VIRTIO_INPUT_KEY_9, "KEY_9"},

                /* Basic controls. */
                {VIRTIO_INPUT_KEY_ENTER, "KEY_ENTER"},
                {VIRTIO_INPUT_KEY_ESC, "KEY_ESC"},
                {VIRTIO_INPUT_KEY_BACKSPACE, "KEY_BACKSPACE"},
                {VIRTIO_INPUT_KEY_TAB, "KEY_TAB"},
                {VIRTIO_INPUT_KEY_SPACE, "KEY_SPACE"},

                /* Modifiers. */
                {VIRTIO_INPUT_KEY_LEFTSHIFT, "KEY_LEFTSHIFT"},
                {VIRTIO_INPUT_KEY_RIGHTSHIFT, "KEY_RIGHTSHIFT"},
                {VIRTIO_INPUT_KEY_LEFTCTRL, "KEY_LEFTCTRL"},
                {VIRTIO_INPUT_KEY_RIGHTCTRL, "KEY_RIGHTCTRL"},
                {VIRTIO_INPUT_KEY_LEFTALT, "KEY_LEFTALT"},
                {VIRTIO_INPUT_KEY_RIGHTALT, "KEY_RIGHTALT"},

                /* Lock. */
                {VIRTIO_INPUT_KEY_CAPSLOCK, "KEY_CAPSLOCK"},

                 /* KEY_F1..KEY_F12 (Linux input ABI). */
                 {VIRTIO_INPUT_KEY_F1, "KEY_F1"},
                 {VIRTIO_INPUT_KEY_F2, "KEY_F2"},
                 {VIRTIO_INPUT_KEY_F3, "KEY_F3"},
                 {VIRTIO_INPUT_KEY_F4, "KEY_F4"},
                 {VIRTIO_INPUT_KEY_F5, "KEY_F5"},
                 {VIRTIO_INPUT_KEY_F6, "KEY_F6"},
                 {VIRTIO_INPUT_KEY_F7, "KEY_F7"},
                 {VIRTIO_INPUT_KEY_F8, "KEY_F8"},
                 {VIRTIO_INPUT_KEY_F9, "KEY_F9"},
                 {VIRTIO_INPUT_KEY_F10, "KEY_F10"},
                 {VIRTIO_INPUT_KEY_F11, "KEY_F11"},
                 {VIRTIO_INPUT_KEY_F12, "KEY_F12"},

                /* Arrows. */
                {VIRTIO_INPUT_KEY_UP, "KEY_UP"},
                {VIRTIO_INPUT_KEY_DOWN, "KEY_DOWN"},
                {VIRTIO_INPUT_KEY_LEFT, "KEY_LEFT"},
                {VIRTIO_INPUT_KEY_RIGHT, "KEY_RIGHT"},

                /* Navigation/editing cluster. */
                {VIRTIO_INPUT_KEY_INSERT, "KEY_INSERT"},
                {VIRTIO_INPUT_KEY_DELETE, "KEY_DELETE"},
                {VIRTIO_INPUT_KEY_HOME, "KEY_HOME"},
                {VIRTIO_INPUT_KEY_END, "KEY_END"},
                {VIRTIO_INPUT_KEY_PAGEUP, "KEY_PAGEUP"},
                {VIRTIO_INPUT_KEY_PAGEDOWN, "KEY_PAGEDOWN"},
            };

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

            status = VioInputValidateEvBitsRequired(bits, kRequiredKeys, RTL_NUMBER_OF(kRequiredKeys), "virtio-input keyboard EV_BITS(EV_KEY)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
            }
        } else {
            /*
             * Contract v1: mouse devices MUST implement:
             *   - EV_BITS(EV_REL) with REL_X, REL_Y, REL_WHEEL
             *   - EV_BITS(EV_KEY) with BTN_LEFT, BTN_RIGHT, BTN_MIDDLE
             */
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredRel[] = {
                {VIRTIO_INPUT_REL_X, "REL_X"},
                {VIRTIO_INPUT_REL_Y, "REL_Y"},
                {VIRTIO_INPUT_REL_WHEEL, "REL_WHEEL"},
            };

            static const VIOINPUT_REQUIRED_EV_CODE kRequiredButtons[] = {
                {VIRTIO_INPUT_BTN_LEFT, "BTN_LEFT"},
                {VIRTIO_INPUT_BTN_RIGHT, "BTN_RIGHT"},
                {VIRTIO_INPUT_BTN_MIDDLE, "BTN_MIDDLE"},
            };

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

            status = VioInputValidateEvBitsRequired(bits, kRequiredRel, RTL_NUMBER_OF(kRequiredRel), "virtio-input mouse EV_BITS(EV_REL)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
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

            status = VioInputValidateEvBitsRequired(bits, kRequiredButtons, RTL_NUMBER_OF(kRequiredButtons), "virtio-input mouse EV_BITS(EV_KEY)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
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

        if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
            WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
            VioInputEventQProcessUsedBuffersLocked(deviceContext);
            WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
        } else {
            VioInputEventQProcessUsedBuffersLocked(deviceContext);
        }
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

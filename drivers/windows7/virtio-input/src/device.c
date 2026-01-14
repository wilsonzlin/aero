#include "virtio_input.h"
#include "virtio_input_proto.h"
#include "virtqueue_split.h"

#include <wdmguid.h>

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

static VOID VioInputSetDeviceKind(_Inout_ PDEVICE_CONTEXT Ctx, _In_ VIOINPUT_DEVICE_KIND Kind);
static VOID VirtioInputInterruptsQuiesceForReset(_Inout_ PDEVICE_CONTEXT DeviceContext, _In_z_ PCSTR Reason);
static NTSTATUS VirtioInputInterruptsResumeAfterReset(_Inout_ PDEVICE_CONTEXT DeviceContext, _In_z_ PCSTR Reason);

typedef struct _VIOINPUT_CONFIG_WORKITEM_CONTEXT {
    WDFDEVICE Device;
} VIOINPUT_CONFIG_WORKITEM_CONTEXT, *PVIOINPUT_CONFIG_WORKITEM_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIOINPUT_CONFIG_WORKITEM_CONTEXT, VioInputGetConfigWorkItemContext);

static EVT_WDF_WORKITEM VioInputEvtConfigChangeWorkItem;

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
    } else if (subsysDeviceId == VIOINPUT_PCI_SUBSYSTEM_ID_TABLET) {
        VioInputSetDeviceKind(Ctx, VioInputDeviceKindTablet);
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
        virtio_input_device_set_enabled_reports(&Ctx->InputDevice, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_CONSUMER);
        break;
    case VioInputDeviceKindMouse:
        virtio_input_device_set_enabled_reports(&Ctx->InputDevice, HID_TRANSLATE_REPORT_MASK_MOUSE);
        break;
    case VioInputDeviceKindTablet:
        virtio_input_device_set_enabled_reports(&Ctx->InputDevice, HID_TRANSLATE_REPORT_MASK_TABLET);
        break;
    case VioInputDeviceKindUnknown:
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

static BOOLEAN VioInputAsciiStartsWithInsensitiveZ(_In_z_ const CHAR* Haystack, _In_z_ const CHAR* Prefix)
{
    CHAR ca;
    CHAR cb;

    if (Haystack == NULL || Prefix == NULL) {
        return FALSE;
    }

    while (*Prefix != '\0') {
        if (*Haystack == '\0') {
            return FALSE;
        }

        ca = *Haystack++;
        cb = *Prefix++;

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

    return TRUE;
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
    ULONG attempt;
    UCHAR selectBytes[2];
    VIRTIO_INPUT_CONFIG cfg;
    UCHAR size;
    ULONG copyLen;
    UCHAR gen0;
    UCHAR gen1;
    BOOLEAN stable;
    const ULONG kMaxRetries = 5;

    if (SizeOut != NULL) {
        *SizeOut = 0;
    }

    if (Ctx == NULL || Out == NULL || OutBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    selectBytes[0] = Select;
    selectBytes[1] = Subsel;

    /*
     * virtio-pci provides common_cfg.config_generation to allow consistent reads
     * of the device-specific config space. virtio-input config reads are a
     * multi-step sequence:
     *   - Write Select/Subsel
     *   - Read Size + Payload
     *
     * To ensure we don't observe torn/mismatched config values, follow the
     * spec-recommended retry loop around the entire sequence:
     *   gen0 = config_generation
     *   write select/subsel
     *   read config
     *   gen1 = config_generation
     *   if gen0 != gen1: retry
     *
     * If CommonCfg is not mapped (unexpected for virtio modern), fall back to
     * a single-shot read without generation validation.
     */
    stable = FALSE;
    gen0 = 0;
    gen1 = 0;

    for (attempt = 0; attempt < kMaxRetries; ++attempt) {
        if (Ctx->PciDevice.CommonCfg != NULL) {
            gen0 = READ_REGISTER_UCHAR((volatile UCHAR *)&Ctx->PciDevice.CommonCfg->config_generation);
            KeMemoryBarrier();
        }

        status = VirtioPciWriteDeviceConfig(&Ctx->PciDevice, 0, selectBytes, sizeof(selectBytes));
        if (!NT_SUCCESS(status)) {
            return status;
        }

        RtlZeroMemory(&cfg, sizeof(cfg));
        if (Ctx->PciDevice.CommonCfg != NULL) {
            status = VirtioPciReadDeviceConfig(&Ctx->PciDevice, 0, &cfg, sizeof(cfg));
        } else {
            ULONG i;
            ULONGLONG end;
            PUCHAR outBytes;

            if (Ctx->PciDevice.DeviceCfg == NULL) {
                return STATUS_INVALID_DEVICE_STATE;
            }

            end = (ULONGLONG)sizeof(cfg);
            if (Ctx->PciDevice.Caps.DeviceCfg.Length != 0 && end > Ctx->PciDevice.Caps.DeviceCfg.Length) {
                return STATUS_INVALID_PARAMETER;
            }

            outBytes = (PUCHAR)&cfg;
            for (i = 0; i < sizeof(cfg); ++i) {
                outBytes[i] = READ_REGISTER_UCHAR((PUCHAR)((ULONG_PTR)Ctx->PciDevice.DeviceCfg + i));
            }
            KeMemoryBarrier();
            status = STATUS_SUCCESS;
        }

        if (!NT_SUCCESS(status)) {
            /*
             * VirtioPciReadDeviceConfig retries internally, but if it still
             * can't obtain a stable snapshot (STATUS_IO_TIMEOUT), allow our
             * outer loop to retry a bounded number of times.
             */
            if (status == STATUS_IO_TIMEOUT && Ctx->PciDevice.CommonCfg != NULL) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
                    "device cfg read unstable (status=%!STATUS!) select=%u subsel=%u retry=%lu/%lu\n",
                    status,
                    (ULONG)Select,
                    (ULONG)Subsel,
                    attempt + 1,
                    kMaxRetries);
                continue;
            }
            return status;
        }

        if (Ctx->PciDevice.CommonCfg == NULL) {
            stable = TRUE;
            break;
        }

        KeMemoryBarrier();
        gen1 = READ_REGISTER_UCHAR((volatile UCHAR *)&Ctx->PciDevice.CommonCfg->config_generation);
        KeMemoryBarrier();

        if (gen0 == gen1) {
            stable = TRUE;
            break;
        }

        VIOINPUT_LOG(
            VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
            "config_generation changed (gen0=%u gen1=%u) select=%u subsel=%u retry=%lu/%lu\n",
            (ULONG)gen0,
            (ULONG)gen1,
            (ULONG)Select,
            (ULONG)Subsel,
            attempt + 1,
            kMaxRetries);
    }

    if (!stable) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "device cfg read failed: config_generation did not stabilize (select=%u subsel=%u gen0=%u gen1=%u)\n",
            (ULONG)Select,
            (ULONG)Subsel,
            (ULONG)gen0,
            (ULONG)gen1);
        return STATUS_DEVICE_DATA_ERROR;
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

static __forceinline PCSTR VioInputDeviceKindToString(_In_ VIOINPUT_DEVICE_KIND Kind)
{
    switch (Kind) {
    case VioInputDeviceKindKeyboard:
        return "keyboard";
    case VioInputDeviceKindMouse:
        return "mouse";
    case VioInputDeviceKindTablet:
        return "tablet";
    default:
        return "unknown";
    }
}

static __forceinline ULONG VioInputPopcountU8(_In_ UCHAR Value)
{
    // 4-bit popcount table to avoid compiler intrinsics / headers in the WDK environment.
    static const UCHAR kPopcount4[16] = {0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4};
    return (ULONG)kPopcount4[Value & 0x0F] + (ULONG)kPopcount4[(Value >> 4) & 0x0F];
}

static ULONG VioInputBitmapPopcount(_In_reads_(128) const UCHAR Bits[128])
{
    ULONG count = 0;
    USHORT i;

    if (Bits == NULL) {
        return 0;
    }

    for (i = 0; i < 128u; ++i) {
        count += VioInputPopcountU8(Bits[i]);
    }

    return count;
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
        ExFreePoolWithTag(Ctx->EventVq, VIRTIOINPUT_POOL_TAG);
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
    Ctx->EventVq = (VIRTQ_SPLIT*)ExAllocatePoolWithTag(NonPagedPool, vqBytes, VIRTIOINPUT_POOL_TAG);
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
        } else if (InterlockedCompareExchange(&Ctx->VirtioStarted, 0, 0) != 0 && VirtioInputIsHidActive(Ctx)) {
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

static VOID VioInputEvtConfigChangeWorkItem(_In_ WDFWORKITEM WorkItem)
{
    PVIOINPUT_CONFIG_WORKITEM_CONTEXT wiCtx;
    WDFDEVICE device;
    PDEVICE_CONTEXT devCtx;
    LONG run;

    PAGED_CODE();

    wiCtx = VioInputGetConfigWorkItemContext(WorkItem);
    device = (wiCtx != NULL) ? wiCtx->Device : NULL;
    if (device == NULL) {
        return;
    }

    devCtx = VirtioInputGetDeviceContext(device);
    run = InterlockedIncrement(&devCtx->ConfigChangeWorkItemRuns);

    for (;;) {
        LONG pending;
        LONG started;
        UCHAR gen;
        UCHAR devStatus;
        NTSTATUS status;

        pending = InterlockedExchange(&devCtx->ConfigChangePending, 0);
        if (pending == 0) {
            break;
        }

        started = InterlockedCompareExchange(&devCtx->VirtioStarted, 0, 0);

        gen = 0;
        devStatus = 0;
        if (devCtx->PciDevice.CommonCfg != NULL) {
            gen = READ_REGISTER_UCHAR(&devCtx->PciDevice.CommonCfg->config_generation);
            devStatus = READ_REGISTER_UCHAR(&devCtx->PciDevice.CommonCfg->device_status);
        }

        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "virtio config change: attempting transport reinit (run=%ld gen=%u lastGen=%u status=0x%02X started=%ld inD0=%u)\n",
            run,
            (ULONG)gen,
            (ULONG)devCtx->LastConfigGeneration,
            (ULONG)devStatus,
            started,
            devCtx->InD0 ? 1u : 0u);

        InterlockedIncrement(&devCtx->ConfigChangeResetAttempts);

        status = VirtioInputHandleVirtioConfigChange(device);
        if (!NT_SUCCESS(status)) {
            InterlockedIncrement(&devCtx->ConfigChangeResetFailures);
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio config change: transport reinit failed: %!STATUS! (attempt=%ld failures=%ld)\n",
                status,
                devCtx->ConfigChangeResetAttempts,
                devCtx->ConfigChangeResetFailures);
        }
    }

    InterlockedExchange(&devCtx->ConfigChangeWorkItemActive, 0);

    //
    // If a config-change interrupt raced with the tail end of this work item
    // (after we drained ConfigChangePending but before we cleared Active),
    // re-queue ourselves.
    //
    if (InterlockedCompareExchange(&devCtx->ConfigChangePending, 0, 0) != 0 && devCtx->ConfigChangeWorkItem != NULL) {
        if (InterlockedCompareExchange(&devCtx->ConfigChangeWorkItemActive, 1, 0) == 0) {
            WdfWorkItemEnqueue(devCtx->ConfigChangeWorkItem);
        }
    }
}

static VOID VioInputEvtConfigChange(_In_ WDFDEVICE Device, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(Device);
    PDEVICE_CONTEXT devCtx = (PDEVICE_CONTEXT)Context;
    LONG cfgCount = -1;
    UCHAR gen = 0;
    UCHAR devStatus = 0;
    UCHAR lastGen = 0;
    BOOLEAN schedule = FALSE;

    if (devCtx != NULL) {
        cfgCount = InterlockedIncrement(&devCtx->ConfigInterruptCount);
        if (devCtx->PciDevice.CommonCfg != NULL) {
            gen = READ_REGISTER_UCHAR(&devCtx->PciDevice.CommonCfg->config_generation);
            devStatus = READ_REGISTER_UCHAR(&devCtx->PciDevice.CommonCfg->device_status);
            lastGen = devCtx->LastConfigGeneration;
        }

        //
        // The virtio-input config space is expected to be static. A config-change
        // interrupt while the device is started may indicate that the device has
        // been reset/reconfigured and that our virtqueue programming is stale.
        //
        if (InterlockedCompareExchange(&devCtx->VirtioStarted, 0, 0) != 0 && devCtx->InD0) {
            schedule = (gen != lastGen) || ((devStatus & VIRTIO_STATUS_DRIVER_OK) == 0);
        }

        if (schedule && devCtx->ConfigChangeWorkItem != NULL) {
            InterlockedExchange(&devCtx->ConfigChangePending, 1);
            if (InterlockedCompareExchange(&devCtx->ConfigChangeWorkItemActive, 1, 0) == 0) {
                WdfWorkItemEnqueue(devCtx->ConfigChangeWorkItem);
            }
        }
    }

    VIOINPUT_LOG(
        VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
        "config change interrupt: gen=%u lastGen=%u status=0x%02X schedule=%u cfgIrqs=%ld interrupts=%ld dpcs=%ld\n",
        (ULONG)gen,
        (ULONG)lastGen,
        (ULONG)devStatus,
        schedule ? 1u : 0u,
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
    if (devCtx != NULL && InterlockedCompareExchange(&devCtx->VirtioStarted, 0, 0) != 0) {
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
        "report ready: virtioEvents=%ld txRing=%ld pendingRing=%ld readQ=%ld eventDrops=%ld overruns=%ld\n",
        deviceContext->Counters.VirtioEvents,
        deviceContext->Counters.ReportRingDepth,
        deviceContext->Counters.PendingRingDepth,
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
            "report ready drained=%lu txRing=%ld pendingRing=%ld readQ=%ld\n",
            drained,
            deviceContext->Counters.ReportRingDepth,
            deviceContext->Counters.PendingRingDepth,
            deviceContext->Counters.ReadReportQueueDepth);
    }
}

static VOID VirtioInputInterruptsQuiesceForReset(_Inout_ PDEVICE_CONTEXT DeviceContext, _In_z_ PCSTR Reason)
{
    NTSTATUS status;
    LONG resetInProgress;
    volatile VIRTIO_PCI_COMMON_CFG* commonCfg;

    if (DeviceContext == NULL || Reason == NULL) {
        return;
    }

    commonCfg = DeviceContext->PciDevice.CommonCfg;
    if (DeviceContext->Interrupts.Mode == VirtioPciInterruptModeMsix && commonCfg == NULL) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
            "%s: interrupts quiesce skipped (CommonCfg NULL)\n",
            Reason);
        return;
    }

    status = VirtioPciInterruptsQuiesce(&DeviceContext->Interrupts, commonCfg);
    resetInProgress = InterlockedCompareExchange(&DeviceContext->Interrupts.ResetInProgress, 0, 0);

    VIOINPUT_LOG(
        VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
        "%s: interrupts quiesce mode=%u status=%!STATUS! resetInProgress=%ld\n",
        Reason,
        (ULONG)DeviceContext->Interrupts.Mode,
        status,
        resetInProgress);

    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "%s: VirtioPciInterruptsQuiesce failed: %!STATUS!\n",
            Reason,
            status);
    }
}

static NTSTATUS VirtioInputInterruptsResumeAfterReset(_Inout_ PDEVICE_CONTEXT DeviceContext, _In_z_ PCSTR Reason)
{
    NTSTATUS status;
    LONG resetInProgress;
    volatile VIRTIO_PCI_COMMON_CFG* commonCfg;

    if (DeviceContext == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Reason == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    commonCfg = DeviceContext->PciDevice.CommonCfg;
    if (DeviceContext->Interrupts.Mode == VirtioPciInterruptModeMsix && commonCfg == NULL) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "%s: interrupts resume failed (CommonCfg NULL)\n",
            Reason);
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioPciInterruptsResume(&DeviceContext->Interrupts, commonCfg);
    resetInProgress = InterlockedCompareExchange(&DeviceContext->Interrupts.ResetInProgress, 0, 0);

    VIOINPUT_LOG(
        VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
        "%s: interrupts resume mode=%u status=%!STATUS! resetInProgress=%ld\n",
        Reason,
        (ULONG)DeviceContext->Interrupts.Mode,
        status,
        resetInProgress);

    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "%s: VirtioPciInterruptsResume failed: %!STATUS!\n",
            Reason,
            status);
    }

    return status;
}

static VOID VirtioInputEvtDeviceSurpriseRemoval(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT ctx = VirtioInputGetDeviceContext(Device);
    BOOLEAN emitResetReports;

    /*
     * Policy: if the HID stack is activated, emit an all-zero report *before*
     * tearing down the read path so Windows releases any latched key state.
     *
     * The reset reports are delivered via the normal report ring/read queues,
     * so they will safely be dropped if the read queues have already been
     * stopped (e.g. a concurrent HID deactivate).
     */
    emitResetReports = VirtioInputIsHidActive(ctx) ? TRUE : FALSE;

    ctx->InD0 = FALSE;
    (VOID)InterlockedExchange(&ctx->VirtioStarted, 0);
    (VOID)InterlockedExchange64(&ctx->NegotiatedFeatures, 0);
    VirtioInputUpdateStatusQActiveState(ctx);

    /*
     * Synchronize with any in-flight queue DPC that may have already started
     * draining the event virtqueue before we cleared VirtioStarted. The DPC's
     * event processing and report delivery execute under this per-queue lock.
     */
    if (ctx->Interrupts.QueueLocks != NULL && ctx->Interrupts.QueueCount > 0) {
        WdfSpinLockAcquire(ctx->Interrupts.QueueLocks[0]);
        WdfSpinLockRelease(ctx->Interrupts.QueueLocks[0]);
    }

    /*
     * Emit the release report after clearing InD0 so any in-flight event
     * processing (interrupt/DPC) will no longer translate additional events
     * that could re-latch key state after the reset.
     */
    if (emitResetReports) {
        /*
         * Drop any queued input reports so the release report is the next thing
         * the HID stack observes (prevents an older "key down" report from being
         * delivered after the release report during teardown).
         */
        VirtioInputHidFlushQueue(Device);
        virtio_input_device_reset_state(&ctx->InputDevice, true);
    }

    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_CANCELLED);
    VioInputDrainInputReportRing(ctx);
    virtio_input_device_reset_state(&ctx->InputDevice, false);

    if (ctx->PciDevice.CommonCfg != NULL) {
        VirtioInputInterruptsQuiesceForReset(ctx, "SurpriseRemoval");
        VirtioPciResetDevice(&ctx->PciDevice);
    }

    if (ctx->EventVq != NULL) {
        if (ctx->Interrupts.QueueLocks != NULL && ctx->Interrupts.QueueCount > 0) {
            WdfSpinLockAcquire(ctx->Interrupts.QueueLocks[0]);
            VirtqSplitReset(ctx->EventVq);
            WdfSpinLockRelease(ctx->Interrupts.QueueLocks[0]);
        } else {
            VirtqSplitReset(ctx->EventVq);
        }
    }

    if (ctx->StatusQ != NULL) {
        VirtioStatusQReset(ctx->StatusQ);
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
        (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 0);
        deviceContext->DeviceKind = VioInputDeviceKindUnknown;
        deviceContext->PciSubsystemDeviceId = 0;
        deviceContext->PciRevisionId = 0;
        (VOID)InterlockedExchange64(&deviceContext->NegotiatedFeatures, 0);

        status = VirtioInputReadReportQueuesInitialize(device);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        RtlZeroMemory(&deviceContext->PciDevice, sizeof(deviceContext->PciDevice));
        RtlZeroMemory(&deviceContext->Interrupts, sizeof(deviceContext->Interrupts));
        deviceContext->ConfigInterruptCount = 0;
        RtlZeroMemory(deviceContext->QueueInterruptCount, sizeof(deviceContext->QueueInterruptCount));
        deviceContext->DmaEnabler = NULL;
        deviceContext->StatusQDropOnFull = FALSE;
        deviceContext->EventVq = NULL;
        deviceContext->EventRingCommonBuffer = NULL;
        deviceContext->EventRxCommonBuffer = NULL;
        deviceContext->EventRxVa = NULL;
        deviceContext->EventRxPa = 0;
        deviceContext->EventQueueSize = 0;
        deviceContext->ConfigChangeWorkItem = NULL;
        deviceContext->ConfigChangeWorkItemActive = 0;
        deviceContext->ConfigChangePending = 0;
        deviceContext->ConfigChangeWorkItemRuns = 0;
        deviceContext->ConfigChangeResetAttempts = 0;
        deviceContext->ConfigChangeResetFailures = 0;
        deviceContext->LastConfigGeneration = 0;

        {
            WDF_OBJECT_ATTRIBUTES lockAttributes;

            WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
            lockAttributes.ParentObject = device;
            status = WdfSpinLockCreate(&lockAttributes, &deviceContext->InputLock);
            if (!NT_SUCCESS(status)) {
                return status;
            }
        }

        {
            WDF_WORKITEM_CONFIG wiConfig;
            WDF_OBJECT_ATTRIBUTES wiAttributes;
            PVIOINPUT_CONFIG_WORKITEM_CONTEXT wiCtx;

            WDF_WORKITEM_CONFIG_INIT(&wiConfig, VioInputEvtConfigChangeWorkItem);

            WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&wiAttributes, VIOINPUT_CONFIG_WORKITEM_CONTEXT);
            wiAttributes.ParentObject = device;

            status = WdfWorkItemCreate(&wiConfig, &wiAttributes, &deviceContext->ConfigChangeWorkItem);
            if (!NT_SUCCESS(status)) {
                return status;
            }

            wiCtx = VioInputGetConfigWorkItemContext(deviceContext->ConfigChangeWorkItem);
            wiCtx->Device = device;
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

VOID VirtioInputUpdateStatusQActiveState(_In_ PDEVICE_CONTEXT Ctx)
{
    BOOLEAN active;

    if (Ctx == NULL || Ctx->StatusQ == NULL) {
        return;
    }

    active = VirtioInputIsHidActive(Ctx) && (Ctx->DeviceKind == VioInputDeviceKindKeyboard);
    VirtioStatusQSetActive(Ctx->StatusQ, active);
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
    (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 0);
    (VOID)InterlockedExchange64(&deviceContext->NegotiatedFeatures, 0);
    deviceContext->ConfigChangeWorkItemActive = 0;
    deviceContext->ConfigChangePending = 0;
    deviceContext->ConfigChangeWorkItemRuns = 0;
    deviceContext->ConfigChangeResetAttempts = 0;
    deviceContext->ConfigChangeResetFailures = 0;
    deviceContext->LastConfigGeneration = 0;

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
            ULONGLONG notifyOffsetBytes0;
            ULONGLONG notifyOffsetBytes1;

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

            /*
             * Pre-cache per-queue notify MMIO addresses.
             *
             * VirtioPciNotifyQueue() populates Dev->QueueNotifyAddrCache on-demand,
             * but that requires taking the CommonCfg lock the first time each queue
             * is notified (to read queue_notify_off). The virtio-input driver may
             * notify from hot paths while holding per-queue locks (eventq) and the
             * internal statusq lock, so pre-caching keeps notifications lock-free.
             *
             * Contract v1 additionally requires queue_notify_off(q)=q, but we use
             * the queried offsets to build the addresses defensively.
             */
            if (deviceContext->PciDevice.NotifyBase == NULL || deviceContext->PciDevice.NotifyOffMultiplier == 0 ||
                deviceContext->PciDevice.NotifyLength < sizeof(UINT16)) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "virtio-input notify capability invalid: base=%p mult=%lu len=%Iu\n",
                    deviceContext->PciDevice.NotifyBase,
                    deviceContext->PciDevice.NotifyOffMultiplier,
                    deviceContext->PciDevice.NotifyLength);
                VirtioPciModernUninit(&deviceContext->PciDevice);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            notifyOffsetBytes0 = (ULONGLONG)notifyOff0 * (ULONGLONG)deviceContext->PciDevice.NotifyOffMultiplier;
            notifyOffsetBytes1 = (ULONGLONG)notifyOff1 * (ULONGLONG)deviceContext->PciDevice.NotifyOffMultiplier;

            if (notifyOffsetBytes0 + sizeof(UINT16) > (ULONGLONG)deviceContext->PciDevice.NotifyLength ||
                notifyOffsetBytes1 + sizeof(UINT16) > (ULONGLONG)deviceContext->PciDevice.NotifyLength) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "virtio-input notify offsets out of range: q0_off=%u q1_off=%u mult=%lu notify_len=%Iu\n",
                    (ULONG)notifyOff0,
                    (ULONG)notifyOff1,
                    deviceContext->PciDevice.NotifyOffMultiplier,
                    deviceContext->PciDevice.NotifyLength);
                VirtioPciModernUninit(&deviceContext->PciDevice);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            deviceContext->QueueNotifyAddrCache[0] =
                (volatile UINT16*)((volatile UCHAR*)deviceContext->PciDevice.NotifyBase + (SIZE_T)notifyOffsetBytes0);
            deviceContext->QueueNotifyAddrCache[1] =
                (volatile UINT16*)((volatile UCHAR*)deviceContext->PciDevice.NotifyBase + (SIZE_T)notifyOffsetBytes1);
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

        /*
         * Optional debugging knob: allow dropping pending statusq writes when the queue is full.
         *
         * This is intentionally read during PrepareHardware so it can be set via:
         *   HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters\StatusQDropOnFull (DWORD)
         */
        {
            WDFKEY paramsKey;
            ULONG dropOnFullValue;
            BOOLEAN dropOnFull;
            UNICODE_STRING valueName;
            NTSTATUS regStatus;

            paramsKey = NULL;
            dropOnFullValue = 0;
            dropOnFull = FALSE;
            RtlInitUnicodeString(&valueName, VIOINPUT_REGVAL_STATUSQ_DROP_ON_FULL);

            regStatus = WdfDriverOpenParametersRegistryKey(
                WdfDeviceGetDriver(Device),
                KEY_READ,
                WDF_NO_OBJECT_ATTRIBUTES,
                &paramsKey);
            if (NT_SUCCESS(regStatus)) {
                regStatus = WdfRegistryQueryULong(paramsKey, &valueName, &dropOnFullValue);
                WdfRegistryClose(paramsKey);
                paramsKey = NULL;
            }

            // Default is "disabled" when the value is absent or cannot be queried.
            dropOnFull = (NT_SUCCESS(regStatus) && dropOnFullValue != 0) ? TRUE : FALSE;
            VirtioStatusQSetDropOnFull(deviceContext->StatusQ, dropOnFull);
            deviceContext->StatusQDropOnFull = dropOnFull;

            VIOINPUT_LOG(
                VIOINPUT_LOG_VIRTQ,
                "statusq DropOnFull=%s (StatusQDropOnFull=%lu query=%!STATUS!)\n",
                dropOnFull ? "enabled" : "disabled",
                dropOnFullValue,
                regStatus);
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
    VirtioInputUpdateStatusQActiveState(deviceContext);
    if (deviceContext->PciDevice.CommonCfg != NULL) {
        deviceContext->LastConfigGeneration = READ_REGISTER_UCHAR(&deviceContext->PciDevice.CommonCfg->config_generation);
    }
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceReleaseHardware(_In_ WDFDEVICE Device, _In_ WDFCMRESLIST ResourcesTranslated)
{
    UNREFERENCED_PARAMETER(ResourcesTranslated);

    PAGED_CODE();

    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_DEVICE_NOT_READY);

    {
        PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(Device);
        if (deviceContext->ConfigChangeWorkItem != NULL) {
            WdfWorkItemFlush(deviceContext->ConfigChangeWorkItem);
        }
        deviceContext->ConfigChangeWorkItemActive = 0;
        deviceContext->ConfigChangePending = 0;

        deviceContext->HardwareReady = FALSE;
        deviceContext->InD0 = FALSE;
        deviceContext->HidActivated = FALSE;
        (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 0);
        (VOID)InterlockedExchange64(&deviceContext->NegotiatedFeatures, 0);
        VirtioInputUpdateStatusQActiveState(deviceContext);

        virtio_input_device_reset_state(&deviceContext->InputDevice, false);

        if (deviceContext->PciDevice.CommonCfg != NULL) {
            VirtioInputInterruptsQuiesceForReset(deviceContext, "ReleaseHardware");
            VirtioPciResetDevice(&deviceContext->PciDevice);
        }

        if (deviceContext->EventVq != NULL) {
            if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
                WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
                VirtqSplitReset(deviceContext->EventVq);
                WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
            } else {
                VirtqSplitReset(deviceContext->EventVq);
            }
        }

        if (deviceContext->StatusQ != NULL) {
            VirtioStatusQReset(deviceContext->StatusQ);
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
    BOOLEAN compatDeviceKind;
    BOOLEAN strictIdName;

    deviceContext = VirtioInputGetDeviceContext(Device);
    compatDeviceKind = g_VioInputCompatIdName ? TRUE : FALSE;
    strictIdName = TRUE;

    deviceContext->InD0 = FALSE;
    (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 0);
    (VOID)InterlockedExchange64(&deviceContext->NegotiatedFeatures, 0);
    deviceContext->KeyboardLedSupportedBitmask = 0;

    if (!deviceContext->HardwareReady) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (deviceContext->EventVq == NULL || deviceContext->StatusQ == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    /*
     * Reset LED capability mask on each bring-up. If we fail to rediscover
     * EV_BITS(EV_LED), StatusQ will fall back to emitting only the required LEDs
     * (NumLock/CapsLock/ScrollLock).
     */
    VirtioStatusQSetKeyboardLedSupportedMask(deviceContext->StatusQ, 0);

    /*
     * Transport bring-up:
     *  - Negotiate features (includes reset, ACKNOWLEDGE|DRIVER, FEATURES_OK).
     *  - Configure queues.
     *  - Post initial RX buffers.
     *  - Program MSI-X vectors (if present) and enable OS interrupt delivery.
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
    (VOID)InterlockedExchange64(&deviceContext->NegotiatedFeatures, (LONG64)negotiated);

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

    /*
     * Device config discovery (contract v1 required selectors).
     *
     * Contract v1 uses ID_NAME as the authoritative device-kind classification.
     * For absolute-pointer devices (virtio-tablet), allow falling back to EV_BITS
     * detection so stock QEMU virtio-tablet devices can be supported even if
     * their ID_NAME strings are not contract-v1 values.
     */
    {
        CHAR name[129];
        UCHAR size;
        VIOINPUT_DEVICE_KIND kind;
        VIOINPUT_DEVICE_KIND subsysKind;
        PCSTR kindStr;

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

        kind = VioInputDeviceKindUnknown;
        strictIdName = TRUE;

        if (VioInputAsciiEqualsInsensitiveZ(name, "Aero Virtio Keyboard") ||
            (compatDeviceKind && VioInputAsciiStartsWithInsensitiveZ(name, "QEMU Virtio Keyboard"))) {
            kind = VioInputDeviceKindKeyboard;
        } else if (VioInputAsciiEqualsInsensitiveZ(name, "Aero Virtio Mouse") ||
                   (compatDeviceKind && VioInputAsciiStartsWithInsensitiveZ(name, "QEMU Virtio Mouse"))) {
            kind = VioInputDeviceKindMouse;
        } else if (VioInputAsciiEqualsInsensitiveZ(name, "Aero Virtio Tablet") ||
                   (compatDeviceKind && VioInputAsciiStartsWithInsensitiveZ(name, "QEMU Virtio Tablet"))) {
            kind = VioInputDeviceKindTablet;
        } else if (compatDeviceKind) {
            /*
             * Compat mode: if ID_NAME isn't one of the known strings, try to infer
             * the device kind from EV_BITS(types). This supports non-Aero virtio-input
             * frontends that expose different ID_NAME values.
             */
            UCHAR typesBits[128];
            UCHAR typesSize;
            BOOLEAN hasAbs;
            BOOLEAN hasRel;
            BOOLEAN hasKey;

            RtlZeroMemory(typesBits, sizeof(typesBits));
            typesSize = 0;
            status = VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_EV_BITS, 0, typesBits, sizeof(typesBits), &typesSize);
            if (NT_SUCCESS(status) && typesSize != 0) {
                hasAbs = VioInputBitmapTestBit(typesBits, VIRTIO_INPUT_EV_ABS) ? TRUE : FALSE;
                hasRel = VioInputBitmapTestBit(typesBits, VIRTIO_INPUT_EV_REL) ? TRUE : FALSE;
                hasKey = VioInputBitmapTestBit(typesBits, VIRTIO_INPUT_EV_KEY) ? TRUE : FALSE;

                if (hasAbs) {
                    kind = VioInputDeviceKindTablet;
                } else if (hasRel) {
                    kind = VioInputDeviceKindMouse;
                } else if (hasKey) {
                    kind = VioInputDeviceKindKeyboard;
                }

                if (kind != VioInputDeviceKindUnknown) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: inferred device kind %s from EV_BITS(types) for ID_NAME='%s'\n",
                        VioInputDeviceKindToString(kind),
                        name);
                }
            }
            if (!NT_SUCCESS(status) || typesSize == 0) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "compat: EV_BITS(types) query failed while inferring device kind: %!STATUS!\n",
                    status);
            }

            if (kind == VioInputDeviceKindUnknown) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "compat: unsupported ID_NAME: '%s' (expected 'Aero Virtio *' or 'QEMU Virtio *')\n",
                    name);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }
        } else if (deviceContext->PciSubsystemDeviceId != VIOINPUT_PCI_SUBSYSTEM_ID_KEYBOARD &&
                   deviceContext->PciSubsystemDeviceId != VIOINPUT_PCI_SUBSYSTEM_ID_MOUSE &&
                   deviceContext->PciSubsystemDeviceId != VIOINPUT_PCI_SUBSYSTEM_ID_TABLET) {
            /*
             * Optional: accept absolute-pointer devices (virtio-tablet) even when
             * the device does not advertise contract-v1 ID_NAME strings, by
             * classifying based on EV_BITS instead of ID_NAME.
             *
             * Restrict this fallback to tablets only; keep keyboard/mouse strict.
             *
             * Additionally, only do this when the PCI subsystem device ID is NOT
             * one of the Aero contract kinds (0x0010/0x0011/0x0012). If it *is*
             * a contract kind, then ID_NAME must be one of the contract strings
             * and we should not guess.
             */
            UCHAR typeBits[128];
            UCHAR typeSize;

            RtlZeroMemory(typeBits, sizeof(typeBits));
            typeSize = 0;
            status =
                VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_EV_BITS, 0, typeBits, sizeof(typeBits), &typeSize);
            if (NT_SUCCESS(status) && typeSize != 0 && VioInputBitmapTestBit(typeBits, VIRTIO_INPUT_EV_ABS)) {
                UCHAR absBits[128];
                UCHAR absSize;

                RtlZeroMemory(absBits, sizeof(absBits));
                absSize = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_EV_BITS,
                                                  VIRTIO_INPUT_EV_ABS,
                                                  absBits,
                                                  sizeof(absBits),
                                                  &absSize);
                if (NT_SUCCESS(status) && absSize != 0 && VioInputBitmapTestBit(absBits, VIRTIO_INPUT_ABS_X) &&
                    VioInputBitmapTestBit(absBits, VIRTIO_INPUT_ABS_Y)) {
                    kind = VioInputDeviceKindTablet;
                    strictIdName = FALSE;
                    VIOINPUT_LOG(VIOINPUT_LOG_VIRTQ, "virtio-input: inferred tablet via EV_BITS (EV_ABS: ABS_X/ABS_Y)\n");
                }
            }
        }

        if (kind == VioInputDeviceKindUnknown) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input unsupported ID_NAME: '%s' (expected 'Aero Virtio Keyboard', 'Aero Virtio Mouse', or 'Aero Virtio Tablet')\n",
                name);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        subsysKind = VioInputDeviceKindUnknown;
        if (deviceContext->PciSubsystemDeviceId == VIOINPUT_PCI_SUBSYSTEM_ID_KEYBOARD) {
            subsysKind = VioInputDeviceKindKeyboard;
        } else if (deviceContext->PciSubsystemDeviceId == VIOINPUT_PCI_SUBSYSTEM_ID_MOUSE) {
            subsysKind = VioInputDeviceKindMouse;
        } else if (deviceContext->PciSubsystemDeviceId == VIOINPUT_PCI_SUBSYSTEM_ID_TABLET) {
            subsysKind = VioInputDeviceKindTablet;
        }

        /*
         * Contract v1 cross-check: if the PCI subsystem device ID indicates a
         * specific kind (keyboard/mouse/tablet), it must agree with the kind inferred
         * from ID_NAME.
         *
         * If the subsystem ID is unknown (0 or other), allow ID_NAME to decide.
         */
        if (strictIdName && subsysKind != VioInputDeviceKindUnknown && subsysKind != kind) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input kind mismatch: ID_NAME='%s' implies %s but PCI subsystem device ID is 0x%04X (%s)\n",
                name,
                VioInputDeviceKindToString(kind),
                (ULONG)deviceContext->PciSubsystemDeviceId,
                VioInputDeviceKindToString(subsysKind));
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        VioInputSetDeviceKind(deviceContext, kind);

        switch (deviceContext->DeviceKind) {
        case VioInputDeviceKindKeyboard:
            kindStr = "keyboard";
            break;
        case VioInputDeviceKindMouse:
            kindStr = "mouse";
            break;
        case VioInputDeviceKindTablet:
            kindStr = "tablet";
            break;
        case VioInputDeviceKindUnknown:
        default:
            kindStr = "unknown";
            break;
        }

        VIOINPUT_LOG(
            VIOINPUT_LOG_VIRTQ,
            "virtio-input config: ID_NAME='%s' pci_subsys=0x%04X kind=%s\n",
            name,
            (ULONG)deviceContext->PciSubsystemDeviceId,
            kindStr);
    }

    {
        VIRTIO_INPUT_DEVIDS ids;
        UCHAR size;
        USHORT expectedProduct;
        BOOLEAN enforce;
        BOOLEAN productMismatchOk;

        RtlZeroMemory(&ids, sizeof(ids));
        size = 0;
        status = VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_ID_DEVIDS, 0, (UCHAR*)&ids, sizeof(ids), &size);

        /*
         * Enforce strict Aero contract ID_DEVIDS values unless:
         *   - CompatIdName is enabled (non-contract virtio-input frontends), or
         *   - This is an EV_BITS-inferred tablet in strict mode (non-contract
         *     absolute pointer).
         *
         * The latter keeps keyboard/mouse behavior deterministic while allowing
         * stock QEMU virtio-tablet devices (which may not report Aero contract
         * ID_DEVIDS values) to function without requiring CompatIdName.
         */
        enforce = (compatDeviceKind || (!strictIdName && deviceContext->DeviceKind == VioInputDeviceKindTablet)) ? FALSE : TRUE;

        if (!NT_SUCCESS(status) || size < sizeof(ids)) {
            VIOINPUT_LOG(
                (enforce ? VIOINPUT_LOG_ERROR : 0) | VIOINPUT_LOG_VIRTQ,
                "virtio-input ID_DEVIDS query failed: %!STATUS!\n",
                status);

            if (enforce) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }

            // Best-effort in compat mode.
            RtlZeroMemory(&ids, sizeof(ids));
            size = 0;
            expectedProduct = 0;
            goto devids_log_only;
        }

        expectedProduct = 0;
        productMismatchOk = FALSE;
        switch (deviceContext->DeviceKind) {
        case VioInputDeviceKindKeyboard:
            expectedProduct = VIRTIO_INPUT_DEVIDS_PRODUCT_KEYBOARD;
            break;
        case VioInputDeviceKindMouse:
            expectedProduct = VIRTIO_INPUT_DEVIDS_PRODUCT_MOUSE;
            break;
        case VioInputDeviceKindTablet:
            expectedProduct = VIRTIO_INPUT_DEVIDS_PRODUCT_TABLET;
            // If the kind was inferred (not a strict contract ID_NAME match), allow a Product mismatch.
            productMismatchOk = (!strictIdName) ? TRUE : FALSE;
            break;
        default:
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input ID_DEVIDS: unexpected device kind %u\n", (ULONG)deviceContext->DeviceKind);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        if (expectedProduct == 0) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input device kind unknown (ID_DEVIDS.Product=0x%04X)\n",
                (ULONG)ids.Product);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        if (ids.Bustype != VIRTIO_INPUT_DEVIDS_BUSTYPE_VIRTUAL || ids.Vendor != VIRTIO_INPUT_DEVIDS_VENDOR_VIRTIO ||
            ids.Version != VIRTIO_INPUT_DEVIDS_VERSION) {
            VIOINPUT_LOG(
                (enforce ? VIOINPUT_LOG_ERROR : 0) | VIOINPUT_LOG_VIRTQ,
                "virtio-input ID_DEVIDS mismatch: bustype=0x%04X vendor=0x%04X product=0x%04X version=0x%04X (expected bustype=0x%04X vendor=0x%04X product=0x%04X version=0x%04X)\n",
                (ULONG)ids.Bustype,
                (ULONG)ids.Vendor,
                (ULONG)ids.Product,
                (ULONG)ids.Version,
                (ULONG)VIRTIO_INPUT_DEVIDS_BUSTYPE_VIRTUAL,
                (ULONG)VIRTIO_INPUT_DEVIDS_VENDOR_VIRTIO,
                (ULONG)expectedProduct,
                (ULONG)VIRTIO_INPUT_DEVIDS_VERSION);

            if (enforce) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }
        }

        if (ids.Product != expectedProduct) {
            if (productMismatchOk || !enforce) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_VIRTQ,
                    "virtio-input ID_DEVIDS product mismatch (continuing): product=0x%04X expected=0x%04X\n",
                    (ULONG)ids.Product,
                    (ULONG)expectedProduct);
            } else {
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
        }

    devids_log_only:
        if (size >= sizeof(ids)) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_VIRTQ,
                "virtio-input config: devids bustype=0x%04X vendor=0x%04X product=0x%04X version=0x%04X\n",
                (ULONG)ids.Bustype,
                (ULONG)ids.Vendor,
                (ULONG)ids.Product,
                (ULONG)ids.Version);
        }
    }

    VIOINPUT_LOG(
        VIOINPUT_LOG_VIRTQ,
        "virtio-input config: pci_subsys=0x%04X kind=%s\n",
        (ULONG)deviceContext->PciSubsystemDeviceId,
        (deviceContext->DeviceKind == VioInputDeviceKindKeyboard)   ? "keyboard"
        : (deviceContext->DeviceKind == VioInputDeviceKindMouse)     ? "mouse"
        : (deviceContext->DeviceKind == VioInputDeviceKindTablet)    ? "tablet"
                                                                    : "unknown");

    {
        UCHAR bits[128];
        UCHAR size;

        if (!compatDeviceKind) {
        /*
         * Contract v1: devices MUST advertise supported event types via
         * EV_BITS(subsel=0).
         */
        RtlZeroMemory(bits, sizeof(bits));
        size = 0;
        status = VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_EV_BITS, 0, bits, sizeof(bits), &size);
        if (!NT_SUCCESS(status) || size == 0) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input EV_BITS(types) query failed: %!STATUS!\n", status);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

        if (deviceContext->DeviceKind == VioInputDeviceKindKeyboard) {
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredKeyboardEvTypes[] = {
                {VIRTIO_INPUT_EV_SYN, "EV_SYN"},
                {VIRTIO_INPUT_EV_KEY, "EV_KEY"},
                {VIRTIO_INPUT_EV_LED, "EV_LED"},
            };

            status = VioInputValidateEvBitsRequired(bits,
                                                   kRequiredKeyboardEvTypes,
                                                   RTL_NUMBER_OF(kRequiredKeyboardEvTypes),
                                                   "virtio-input keyboard EV_BITS(types)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
            }
        } else if (deviceContext->DeviceKind == VioInputDeviceKindMouse) {
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredMouseEvTypes[] = {
                {VIRTIO_INPUT_EV_SYN, "EV_SYN"},
                {VIRTIO_INPUT_EV_KEY, "EV_KEY"},
                {VIRTIO_INPUT_EV_REL, "EV_REL"},
            };

            status = VioInputValidateEvBitsRequired(bits,
                                                   kRequiredMouseEvTypes,
                                                   RTL_NUMBER_OF(kRequiredMouseEvTypes),
                                                   "virtio-input mouse EV_BITS(types)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
            }
        } else if (deviceContext->DeviceKind == VioInputDeviceKindTablet) {
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredTabletEvTypes[] = {
                {VIRTIO_INPUT_EV_SYN, "EV_SYN"},
                {VIRTIO_INPUT_EV_ABS, "EV_ABS"},
            };

            status = VioInputValidateEvBitsRequired(bits,
                                                   kRequiredTabletEvTypes,
                                                   RTL_NUMBER_OF(kRequiredTabletEvTypes),
                                                   "virtio-input tablet EV_BITS(types)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
            }

            if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_EV_KEY)) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_VIRTQ,
                    "virtio-input tablet EV_BITS(types): EV_KEY not advertised; tablet will report no buttons/touch\n");
            }
        } else {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input EV_BITS(types): invalid device kind %u\n",
                (ULONG)deviceContext->DeviceKind);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }

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

                /* Lock keys. */
                {VIRTIO_INPUT_KEY_CAPSLOCK, "KEY_CAPSLOCK"},
                {VIRTIO_INPUT_KEY_NUMLOCK, "KEY_NUMLOCK"},
                {VIRTIO_INPUT_KEY_SCROLLLOCK, "KEY_SCROLLLOCK"},

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

            /*
             * Contract v1: keyboards MUST advertise LED support. The device may
             * ignore the statusq contents, but it must accept the events.
             */
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredLeds[] = {
                {VIRTIO_INPUT_LED_NUML, "LED_NUML"},
                {VIRTIO_INPUT_LED_CAPSL, "LED_CAPSL"},
                {VIRTIO_INPUT_LED_SCROLLL, "LED_SCROLLL"},
            };

            RtlZeroMemory(bits, sizeof(bits));
            size = 0;
            status = VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_EV_BITS, VIRTIO_INPUT_EV_LED, bits, sizeof(bits), &size);
            if (!NT_SUCCESS(status) || size == 0) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input EV_BITS(EV_LED) query failed: %!STATUS!\n", status);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }

            status = VioInputValidateEvBitsRequired(bits,
                                                   kRequiredLeds,
                                                   RTL_NUMBER_OF(kRequiredLeds),
                                                   "virtio-input keyboard EV_BITS(EV_LED)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
            }

            /*
             * Cache the subset of LED codes we might emit from HID output reports
             * (NumLock/CapsLock/ScrollLock/Compose/Kana => codes 0..4).
             *
             * The contract requires at least Num/Caps/Scroll; other LED codes may
             * not be advertised by the device and must not be emitted.
             */
            {
                UCHAR ledSupportedMask;

                ledSupportedMask = 0;
                if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_NUML)) {
                    ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_NUML);
                }
                if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_CAPSL)) {
                    ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_CAPSL);
                }
                if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_SCROLLL)) {
                    ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_SCROLLL);
                }
                if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_COMPOSE)) {
                    ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_COMPOSE);
                }
                if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_KANA)) {
                    ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_KANA);
                }

                /* Keep a copy in the device context for debugging/telemetry. */
                deviceContext->KeyboardLedSupportedBitmask = ledSupportedMask;

                VirtioStatusQSetKeyboardLedSupportedMask(deviceContext->StatusQ, ledSupportedMask);

                VIOINPUT_LOG(
                    VIOINPUT_LOG_VIRTQ,
                    "virtio-input keyboard EV_BITS(EV_LED): supported_mask=0x%02X (codes 0..4)\n",
                    (ULONG)ledSupportedMask);
            }
        } else if (deviceContext->DeviceKind == VioInputDeviceKindMouse) {
            /*
             * Contract v1: mouse devices MUST implement:
             *   - EV_BITS(EV_REL) with REL_X, REL_Y, REL_WHEEL
             *   - EV_BITS(EV_KEY) with BTN_LEFT, BTN_RIGHT, BTN_MIDDLE
             *
             * Devices may also advertise additional relative axes (e.g. REL_HWHEEL)
             * but they are not required for contract v1.
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
        } else if (deviceContext->DeviceKind == VioInputDeviceKindTablet) {
            /*
             * Tablet devices MUST implement:
             *   - EV_BITS(EV_ABS) with ABS_X, ABS_Y
             *   - ABS_INFO for ABS_X and ABS_Y so we can scale into the HID
             *     logical range.
             */
            static const VIOINPUT_REQUIRED_EV_CODE kRequiredAbs[] = {
                {VIRTIO_INPUT_ABS_X, "ABS_X"},
                {VIRTIO_INPUT_ABS_Y, "ABS_Y"},
            };

            RtlZeroMemory(bits, sizeof(bits));
            size = 0;
            status = VioInputQueryInputConfig(deviceContext,
                                              VIRTIO_INPUT_CFG_EV_BITS,
                                              VIRTIO_INPUT_EV_ABS,
                                              bits,
                                              sizeof(bits),
                                              &size);
            if (!NT_SUCCESS(status) || size == 0) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input EV_BITS(EV_ABS) query failed: %!STATUS!\n", status);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }

            status = VioInputValidateEvBitsRequired(bits, kRequiredAbs, RTL_NUMBER_OF(kRequiredAbs), "virtio-input tablet EV_BITS(EV_ABS)");
            if (!NT_SUCCESS(status)) {
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return status;
            }

            {
                VIRTIO_INPUT_ABS_INFO absX;
                VIRTIO_INPUT_ABS_INFO absY;
                UCHAR absSize;
                BOOLEAN haveAbsX;
                BOOLEAN haveAbsY;
                LONG absXMin;
                LONG absXMax;
                LONG absYMin;
                LONG absYMax;

                haveAbsX = FALSE;
                haveAbsY = FALSE;
                absXMin = 0;
                absXMax = 32767;
                absYMin = 0;
                absYMax = 32767;

                RtlZeroMemory(&absX, sizeof(absX));
                absSize = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_ABS_INFO,
                                                  VIRTIO_INPUT_ABS_X,
                                                  (UCHAR*)&absX,
                                                  sizeof(absX),
                                                  &absSize);
                if (NT_SUCCESS(status) && absSize >= 8) {
                    haveAbsX = TRUE;
                    absXMin = absX.Min;
                    absXMax = absX.Max;
                } else {
                    VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input ABS_INFO(ABS_X) query failed: %!STATUS!\n", status);
                    if (strictIdName) {
                        VirtioPciResetDevice(&deviceContext->PciDevice);
                        return STATUS_NOT_SUPPORTED;
                    }
                }

                RtlZeroMemory(&absY, sizeof(absY));
                absSize = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_ABS_INFO,
                                                  VIRTIO_INPUT_ABS_Y,
                                                  (UCHAR*)&absY,
                                                  sizeof(absY),
                                                  &absSize);
                if (NT_SUCCESS(status) && absSize >= 8) {
                    haveAbsY = TRUE;
                    absYMin = absY.Min;
                    absYMax = absY.Max;
                } else {
                    VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "virtio-input ABS_INFO(ABS_Y) query failed: %!STATUS!\n", status);
                    if (strictIdName) {
                        VirtioPciResetDevice(&deviceContext->PciDevice);
                        return STATUS_NOT_SUPPORTED;
                    }
                }

                hid_translate_set_tablet_abs_range(&deviceContext->InputDevice.translate, absXMin, absXMax, absYMin, absYMax);

                if (haveAbsX && haveAbsY) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "virtio-input tablet ABS ranges: X=[%ld,%ld] Y=[%ld,%ld]\n",
                        absXMin,
                        absXMax,
                        absYMin,
                        absYMax);
                } else {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "virtio-input tablet ABS_INFO unavailable (x=%u y=%u); using default scaling X=[%ld,%ld] Y=[%ld,%ld]\n",
                        haveAbsX ? 1u : 0u,
                        haveAbsY ? 1u : 0u,
                        absXMin,
                        absXMax,
                        absYMin,
                        absYMax);
                }
            }
        } else {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "virtio-input EV_BITS validation: invalid device kind %u\n",
                (ULONG)deviceContext->DeviceKind);
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return STATUS_NOT_SUPPORTED;
        }
        } else {
            BOOLEAN hasSyn;
            BOOLEAN hasKey;
            BOOLEAN hasLed;
            BOOLEAN hasRel;
            BOOLEAN hasAbs;

            /*
             * Compat mode: relax the contract-v1 EV_BITS requirements.
             *
             * We still require EV_BITS(types) and enough information to drive our
             * HID translation layer, but avoid rejecting devices that don't
             * implement Aero's full minimum code sets (e.g. mice without wheel,
             * keyboards without all LEDs, tablets without ABS_INFO, etc).
             */
            RtlZeroMemory(bits, sizeof(bits));
            size = 0;
            status = VioInputQueryInputConfig(deviceContext, VIRTIO_INPUT_CFG_EV_BITS, 0, bits, sizeof(bits), &size);
            if (!NT_SUCCESS(status) || size == 0) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "compat: virtio-input EV_BITS(types) query failed: %!STATUS!\n",
                    status);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }

            hasSyn = VioInputBitmapTestBit(bits, VIRTIO_INPUT_EV_SYN) ? TRUE : FALSE;
            hasKey = VioInputBitmapTestBit(bits, VIRTIO_INPUT_EV_KEY) ? TRUE : FALSE;
            hasLed = VioInputBitmapTestBit(bits, VIRTIO_INPUT_EV_LED) ? TRUE : FALSE;
            hasRel = VioInputBitmapTestBit(bits, VIRTIO_INPUT_EV_REL) ? TRUE : FALSE;
            hasAbs = VioInputBitmapTestBit(bits, VIRTIO_INPUT_EV_ABS) ? TRUE : FALSE;

            if (!hasSyn) {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "compat: virtio-input EV_BITS(types) missing required EV_SYN\n");
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_NOT_SUPPORTED;
            }

            VIOINPUT_LOG(
                VIOINPUT_LOG_VIRTQ,
                "compat: relaxed EV_BITS validation kind=%s (types: key=%u led=%u rel=%u abs=%u)\n",
                VioInputDeviceKindToString(deviceContext->DeviceKind),
                hasKey ? 1u : 0u,
                hasLed ? 1u : 0u,
                hasRel ? 1u : 0u,
                hasAbs ? 1u : 0u);

            if (deviceContext->DeviceKind == VioInputDeviceKindKeyboard) {
                ULONG keyCount;

                if (!hasKey) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: keyboard requires EV_KEY in EV_BITS(types)\n");
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                if (!hasLed) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: keyboard EV_BITS(types) does not advertise EV_LED; LED output reports may be ignored\n");
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
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: keyboard EV_BITS(EV_KEY) query failed: %!STATUS!\n",
                        status);
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                keyCount = VioInputBitmapPopcount(bits);
                VIOINPUT_LOG(VIOINPUT_LOG_VIRTQ, "compat: keyboard EV_KEY bitcount=%lu\n", keyCount);
                if (keyCount < 8u) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: keyboard advertises unusually small EV_KEY bitmap (bitcount=%lu)\n",
                        keyCount);
                }
                if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_KEY_A) || !VioInputBitmapTestBit(bits, VIRTIO_INPUT_KEY_Z)) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: keyboard EV_KEY bitmap does not include KEY_A/KEY_Z; key mapping may be incomplete\n");
                }

                if (hasLed) {
                    RtlZeroMemory(bits, sizeof(bits));
                    size = 0;
                    status = VioInputQueryInputConfig(deviceContext,
                                                      VIRTIO_INPUT_CFG_EV_BITS,
                                                      VIRTIO_INPUT_EV_LED,
                                                      bits,
                                                      sizeof(bits),
                                                      &size);
                    if (!NT_SUCCESS(status) || size == 0) {
                        VIOINPUT_LOG(
                            VIOINPUT_LOG_VIRTQ,
                            "compat: keyboard EV_BITS(EV_LED) query failed: %!STATUS! (continuing)\n",
                            status);
                    } else {
                        /*
                         * Even in compat mode, if the device advertises EV_LED and
                         * provides an EV_BITS(EV_LED) bitmap, plumb it into the
                         * statusq so we only emit LED codes the device claims to
                         * support.
                         */
                        {
                            UCHAR ledSupportedMask;

                            ledSupportedMask = 0;
                            if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_NUML)) {
                                ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_NUML);
                            }
                            if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_CAPSL)) {
                                ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_CAPSL);
                            }
                            if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_SCROLLL)) {
                                ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_SCROLLL);
                            }
                            if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_COMPOSE)) {
                                ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_COMPOSE);
                            }
                            if (VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_KANA)) {
                                ledSupportedMask |= (UCHAR)(1u << VIRTIO_INPUT_LED_KANA);
                            }

                            deviceContext->KeyboardLedSupportedBitmask = ledSupportedMask;
                            VirtioStatusQSetKeyboardLedSupportedMask(deviceContext->StatusQ, ledSupportedMask);

                            VIOINPUT_LOG(
                                VIOINPUT_LOG_VIRTQ,
                                "compat: keyboard EV_BITS(EV_LED): supported_mask=0x%02X (codes 0..4)\n",
                                (ULONG)ledSupportedMask);
                        }

                        if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_NUML) || !VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_CAPSL) ||
                            !VioInputBitmapTestBit(bits, VIRTIO_INPUT_LED_SCROLLL)) {
                            VIOINPUT_LOG(
                                VIOINPUT_LOG_VIRTQ,
                                "compat: keyboard EV_LED bitmap is missing one or more of Num/Caps/Scroll lock LEDs\n");
                        }
                    }
                }
            } else if (deviceContext->DeviceKind == VioInputDeviceKindMouse) {
                if (!hasRel) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: mouse requires EV_REL in EV_BITS(types)\n");
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                RtlZeroMemory(bits, sizeof(bits));
                size = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_EV_BITS,
                                                  VIRTIO_INPUT_EV_REL,
                                                  bits,
                                                  sizeof(bits),
                                                  &size);
                if (!NT_SUCCESS(status) || size == 0) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: mouse EV_BITS(EV_REL) query failed: %!STATUS!\n",
                        status);
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_REL_X) || !VioInputBitmapTestBit(bits, VIRTIO_INPUT_REL_Y)) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: mouse EV_REL bitmap missing REL_X/REL_Y\n");
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_REL_WHEEL)) {
                    VIOINPUT_LOG(VIOINPUT_LOG_VIRTQ, "compat: mouse EV_REL bitmap missing REL_WHEEL; scroll wheel disabled\n");
                }
                if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_REL_HWHEEL)) {
                    VIOINPUT_LOG(VIOINPUT_LOG_VIRTQ, "compat: mouse EV_REL bitmap missing REL_HWHEEL; horizontal wheel disabled\n");
                }

                if (!hasKey) {
                    VIOINPUT_LOG(VIOINPUT_LOG_VIRTQ, "compat: mouse EV_BITS(types) does not advertise EV_KEY; mouse will report no buttons\n");
                } else {
                    RtlZeroMemory(bits, sizeof(bits));
                    size = 0;
                    status = VioInputQueryInputConfig(deviceContext,
                                                      VIRTIO_INPUT_CFG_EV_BITS,
                                                      VIRTIO_INPUT_EV_KEY,
                                                      bits,
                                                      sizeof(bits),
                                                      &size);
                    if (!NT_SUCCESS(status) || size == 0) {
                        VIOINPUT_LOG(
                            VIOINPUT_LOG_VIRTQ,
                            "compat: mouse EV_BITS(EV_KEY) query failed: %!STATUS! (continuing)\n",
                            status);
                    } else {
                        if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_BTN_LEFT) || !VioInputBitmapTestBit(bits, VIRTIO_INPUT_BTN_RIGHT)) {
                            VIOINPUT_LOG(
                                VIOINPUT_LOG_VIRTQ,
                                "compat: mouse EV_KEY bitmap missing BTN_LEFT/BTN_RIGHT; button mapping may be incomplete\n");
                        }
                    }
                }
            } else if (deviceContext->DeviceKind == VioInputDeviceKindTablet) {
                BOOLEAN haveAbsInfo;
                VIRTIO_INPUT_ABS_INFO absX;
                VIRTIO_INPUT_ABS_INFO absY;
                UCHAR absSize;

                if (!hasAbs) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: tablet requires EV_ABS in EV_BITS(types)\n");
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                RtlZeroMemory(bits, sizeof(bits));
                size = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_EV_BITS,
                                                  VIRTIO_INPUT_EV_ABS,
                                                  bits,
                                                  sizeof(bits),
                                                  &size);
                if (!NT_SUCCESS(status) || size == 0) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: tablet EV_BITS(EV_ABS) query failed: %!STATUS!\n",
                        status);
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                if (!VioInputBitmapTestBit(bits, VIRTIO_INPUT_ABS_X) || !VioInputBitmapTestBit(bits, VIRTIO_INPUT_ABS_Y)) {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                        "compat: tablet EV_ABS bitmap missing ABS_X/ABS_Y\n");
                    VirtioPciResetDevice(&deviceContext->PciDevice);
                    return STATUS_NOT_SUPPORTED;
                }

                /*
                 * ABS_INFO is best-effort in compat mode. If it's unavailable,
                 * keep the translation layer's default scaling range (0..32767).
                 */
                haveAbsInfo = TRUE;

                RtlZeroMemory(&absX, sizeof(absX));
                absSize = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_ABS_INFO,
                                                  VIRTIO_INPUT_ABS_X,
                                                  (UCHAR*)&absX,
                                                  sizeof(absX),
                                                  &absSize);
                if (!NT_SUCCESS(status) || absSize < 8) {
                    haveAbsInfo = FALSE;
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: tablet ABS_INFO(ABS_X) unavailable: %!STATUS! (continuing)\n",
                        status);
                }

                RtlZeroMemory(&absY, sizeof(absY));
                absSize = 0;
                status = VioInputQueryInputConfig(deviceContext,
                                                  VIRTIO_INPUT_CFG_ABS_INFO,
                                                  VIRTIO_INPUT_ABS_Y,
                                                  (UCHAR*)&absY,
                                                  sizeof(absY),
                                                  &absSize);
                if (!NT_SUCCESS(status) || absSize < 8) {
                    haveAbsInfo = FALSE;
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: tablet ABS_INFO(ABS_Y) unavailable: %!STATUS! (continuing)\n",
                        status);
                }

                if (haveAbsInfo) {
                    hid_translate_set_tablet_abs_range(&deviceContext->InputDevice.translate, absX.Min, absX.Max, absY.Min, absY.Max);
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: tablet ABS ranges: X=[%ld,%ld] Y=[%ld,%ld]\n",
                        (LONG)absX.Min,
                        (LONG)absX.Max,
                        (LONG)absY.Min,
                        (LONG)absY.Max);
                } else {
                    VIOINPUT_LOG(
                        VIOINPUT_LOG_VIRTQ,
                        "compat: tablet ABS ranges unknown; using default HID scaling range\n");
                }
            } else {
                VIOINPUT_LOG(
                    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                    "compat: virtio-input EV_BITS validation: invalid device kind %u\n",
                    (ULONG)deviceContext->DeviceKind);
                VirtioPciResetDevice(&deviceContext->PciDevice);
                return STATUS_INVALID_DEVICE_STATE;
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

    VirtioStatusQReset(deviceContext->StatusQ);

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

        virtio_input_device_reset_state(&deviceContext->InputDevice, emitResetReports ? true : false);
        deviceContext->InD0 = TRUE;

        (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 1);

        status = VirtioInputInterruptsResumeAfterReset(deviceContext, "D0Entry");
        if (!NT_SUCCESS(status)) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "VirtioPciInterruptsResume failed: %!STATUS!\n",
                status);
            (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 0);
            deviceContext->InD0 = FALSE;
            VirtioPciResetDevice(&deviceContext->PciDevice);
            return status;
        }
        VirtioPciAddStatus(&deviceContext->PciDevice, VIRTIO_STATUS_DRIVER_OK);

        if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
            WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
            VioInputEventQProcessUsedBuffersLocked(deviceContext);
            WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
        } else {
            VioInputEventQProcessUsedBuffersLocked(deviceContext);
        }
    }

    VirtioInputUpdateStatusQActiveState(deviceContext);
    if (deviceContext->PciDevice.CommonCfg != NULL) {
        deviceContext->LastConfigGeneration = READ_REGISTER_UCHAR(&deviceContext->PciDevice.CommonCfg->config_generation);
    }
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceD0Exit(_In_ WDFDEVICE Device, _In_ WDF_POWER_DEVICE_STATE TargetState)
{
    UNREFERENCED_PARAMETER(TargetState);

    PDEVICE_CONTEXT deviceContext;
    BOOLEAN emitResetReports;

    deviceContext = VirtioInputGetDeviceContext(Device);

    emitResetReports = VirtioInputIsHidActive(deviceContext) ? TRUE : FALSE;
    (VOID)InterlockedExchange(&deviceContext->VirtioStarted, 0);
    (VOID)InterlockedExchange64(&deviceContext->NegotiatedFeatures, 0);
    deviceContext->InD0 = FALSE;

    /*
     * Synchronize with any in-flight queue DPC that may have already started
     * draining the event virtqueue before we cleared VirtioStarted.
     */
    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
    }

    /*
     * Policy: if HID has been activated, emit an all-zero report before stopping
     * the read queues so Windows releases any latched key state during the
     * transition away from D0.
     *
     * Note: we clear InD0 (and VirtioStarted) first so any in-flight queue/DPC
     * processing will stop translating further events that could re-latch key
     * state after the release report.
     *
     * This report is sent through the normal read-report path, so it will be
     * dropped automatically if reads have already been disabled.
     */
    if (emitResetReports) {
        /*
         * Drop any queued input reports so the release report is the next thing
         * the HID stack observes during power-down.
         */
        VirtioInputHidFlushQueue(Device);
        virtio_input_device_reset_state(&deviceContext->InputDevice, true);
    }

    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_DEVICE_NOT_READY);
    VioInputDrainInputReportRing(deviceContext);
    virtio_input_device_reset_state(&deviceContext->InputDevice, false);

    VirtioInputUpdateStatusQActiveState(deviceContext);

    if (deviceContext->PciDevice.CommonCfg != NULL) {
        VirtioInputInterruptsQuiesceForReset(deviceContext, "D0Exit");
        VirtioPciResetDevice(&deviceContext->PciDevice);
    }

    if (deviceContext->Interrupts.QueueLocks != NULL && deviceContext->Interrupts.QueueCount > 0) {
        WdfSpinLockAcquire(deviceContext->Interrupts.QueueLocks[0]);
        VirtqSplitReset(deviceContext->EventVq);
        WdfSpinLockRelease(deviceContext->Interrupts.QueueLocks[0]);
    } else {
        VirtqSplitReset(deviceContext->EventVq);
    }

    VirtioStatusQReset(deviceContext->StatusQ);

    return STATUS_SUCCESS;
}

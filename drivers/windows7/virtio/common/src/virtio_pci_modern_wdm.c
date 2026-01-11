/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtio_pci_modern_wdm.h"

#include "../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.h"

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

#define VIRTIO_PCI_RESET_TIMEOUT_US        1000000u
#define VIRTIO_PCI_RESET_POLL_DELAY_US     1000u
#define VIRTIO_PCI_CONFIG_MAX_READ_RETRIES 10u

typedef struct _VIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT {
    KEVENT Event;
} VIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT, *PVIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT;

static NTSTATUS
VirtioPciWdmQueryInterfaceCompletionRoutine(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PVOID Context)
{
    PVIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT ctx;

    UNREFERENCED_PARAMETER(DeviceObject);
    UNREFERENCED_PARAMETER(Irp);

    ctx = (PVIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT)Context;
    KeSetEvent(&ctx->Event, IO_NO_INCREMENT, FALSE);
    return STATUS_MORE_PROCESSING_REQUIRED;
}

static NTSTATUS
VirtioPciWdmQueryInterface(_In_ PDEVICE_OBJECT LowerDeviceObject,
                           _In_ const GUID *InterfaceGuid,
                           _In_ USHORT InterfaceSize,
                           _In_ USHORT InterfaceVersion,
                           _Out_ PINTERFACE InterfaceOut)
{
    VIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT ctx;
    PIRP irp;
    PIO_STACK_LOCATION irpSp;
    NTSTATUS status;

    if (LowerDeviceObject == NULL || InterfaceGuid == NULL || InterfaceOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeInitializeEvent(&ctx.Event, NotificationEvent, FALSE);

    irp = IoAllocateIrp(LowerDeviceObject->StackSize, FALSE);
    if (irp == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    irp->IoStatus.Information = 0;

    irpSp = IoGetNextIrpStackLocation(irp);
    irpSp->MajorFunction = IRP_MJ_PNP;
    irpSp->MinorFunction = IRP_MN_QUERY_INTERFACE;
    irpSp->Parameters.QueryInterface.InterfaceType = (LPGUID)InterfaceGuid;
    irpSp->Parameters.QueryInterface.Size = InterfaceSize;
    irpSp->Parameters.QueryInterface.Version = InterfaceVersion;
    irpSp->Parameters.QueryInterface.Interface = InterfaceOut;
    irpSp->Parameters.QueryInterface.InterfaceSpecificData = NULL;

    IoSetCompletionRoutine(
        irp, VirtioPciWdmQueryInterfaceCompletionRoutine, &ctx, /*InvokeOnSuccess=*/TRUE, /*InvokeOnError=*/TRUE, /*InvokeOnCancel=*/TRUE);

    status = IoCallDriver(LowerDeviceObject, irp);
    if (status == STATUS_PENDING) {
        KeWaitForSingleObject(&ctx.Event, Executive, KernelMode, FALSE, NULL);
    }

    status = irp->IoStatus.Status;
    IoFreeIrp(irp);
    return status;
}

static ULONG
VirtioPciReadConfig(_In_ PPCI_BUS_INTERFACE_STANDARD PciInterface,
                    _Out_writes_bytes_(Length) PVOID Buffer,
                    _In_ ULONG Offset,
                    _In_ ULONG Length)
{
    if (PciInterface->ReadConfig != NULL) {
        return PciInterface->ReadConfig(PciInterface->Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
    }

    return 0;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmUnmapBars(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    Dev->CommonCfg = NULL;
    Dev->NotifyBase = NULL;
    Dev->NotifyOffMultiplier = 0;
    Dev->NotifyLength = 0;
    Dev->IsrStatus = NULL;
    Dev->DeviceCfg = NULL;

    /*
     * Any cached notify addresses point into the NOTIFY capability mapping.
     * Invalidate the cache when BARs are unmapped (PnP stop/start).
     */
    if (Dev->QueueNotifyAddrCache != NULL && Dev->QueueNotifyAddrCacheCount != 0) {
        RtlZeroMemory((PVOID)Dev->QueueNotifyAddrCache,
                      (SIZE_T)Dev->QueueNotifyAddrCacheCount * sizeof(Dev->QueueNotifyAddrCache[0]));
    }

    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        if (Dev->Bars[i].Va != NULL) {
            MmUnmapIoSpace(Dev->Bars[i].Va, Dev->Bars[i].Length);
            Dev->Bars[i].Va = NULL;
        }

        Dev->Bars[i].RawStart.QuadPart = 0;
        Dev->Bars[i].TranslatedStart.QuadPart = 0;
        Dev->Bars[i].Length = 0;
    }
}

static NTSTATUS
VirtioPciModernWdmReadBarsFromConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    ULONG barRegs[VIRTIO_PCI_MAX_BARS];
    ULONG bytesRead;
    ULONG barRegsLen;
    ULONG i;

    RtlZeroMemory(barRegs, sizeof(barRegs));
    barRegsLen = (ULONG)sizeof(barRegs);
    bytesRead = VirtioPciReadConfig(&Dev->PciInterface, barRegs, 0x10, barRegsLen);
    if (bytesRead != barRegsLen) {
        VIRTIO_PCI_MODERN_WDM_PRINT("PCI BAR config read failed (%lu/%lu)\n", bytesRead, barRegsLen);
        return STATUS_DEVICE_DATA_ERROR;
    }

    /*
     * Preserve mapped VA/length until VirtioPciModernWdmUnmapBars is called, but
     * refresh the BAR programming (base address, 32/64-bit).
     */
    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        Dev->Bars[i].Present = FALSE;
        Dev->Bars[i].IsMemory = FALSE;
        Dev->Bars[i].Is64Bit = FALSE;
        Dev->Bars[i].IsUpperHalf = FALSE;
        Dev->Bars[i].Base = 0;
    }

    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        ULONG val;

        val = barRegs[i];
        if (val == 0) {
            continue;
        }

        if ((val & 0x1) != 0) {
            /* I/O BAR (not expected for virtio modern). */
            Dev->Bars[i].Present = TRUE;
            Dev->Bars[i].IsMemory = FALSE;
            Dev->Bars[i].Base = (ULONGLONG)(val & ~0x3u);
            continue;
        }

        /* Memory BAR. */
        {
            ULONG memType;
            memType = (val >> 1) & 0x3u;

            if (memType == 0x2u) {
                /* 64-bit BAR uses this and the next BAR dword. */
                ULONGLONG base;
                ULONG high;

                if (i == (VIRTIO_PCI_MAX_BARS - 1)) {
                    VIRTIO_PCI_MODERN_WDM_PRINT("BAR%lu claims to be 64-bit but has no high dword\n", i);
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }

                high = barRegs[i + 1];
                base = ((ULONGLONG)high << 32) | (ULONGLONG)(val & ~0xFu);

                Dev->Bars[i].Present = TRUE;
                Dev->Bars[i].IsMemory = TRUE;
                Dev->Bars[i].Is64Bit = TRUE;
                Dev->Bars[i].Base = base;

                Dev->Bars[i + 1].IsUpperHalf = TRUE;

                /* Skip the high dword slot. */
                i++;
            } else {
                Dev->Bars[i].Present = TRUE;
                Dev->Bars[i].IsMemory = TRUE;
                Dev->Bars[i].Base = (ULONGLONG)(val & ~0xFu);
            }
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtioPciModernWdmDiscoverCaps(_In_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    UCHAR cfg[256];
    ULONG bytesRead;
    ULONG cfgLen;
    uint64_t barAddrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t parsed;
    virtio_pci_cap_parse_result_t parseRes;
    ULONG i;

    RtlZeroMemory(&Dev->Caps, sizeof(Dev->Caps));
    RtlZeroMemory(cfg, sizeof(cfg));

    cfgLen = (ULONG)sizeof(cfg);
    bytesRead = VirtioPciReadConfig(&Dev->PciInterface, cfg, 0, cfgLen);
    if (bytesRead != cfgLen) {
        VIRTIO_PCI_MODERN_WDM_PRINT("PCI config read failed (%lu/%lu)\n", bytesRead, cfgLen);
        return STATUS_DEVICE_DATA_ERROR;
    }

    for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
        barAddrs[i] = (uint64_t)Dev->Bars[i].Base;
    }

    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), barAddrs, &parsed);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        VIRTIO_PCI_MODERN_WDM_PRINT("Virtio PCI capability parse failed: %s (%d)\n",
                                    virtio_pci_cap_parse_result_str(parseRes),
                                    (int)parseRes);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    Dev->Caps.CommonCfg.Present = TRUE;
    Dev->Caps.CommonCfg.CfgType = VIRTIO_PCI_CAP_COMMON_CFG;
    Dev->Caps.CommonCfg.Bar = (UCHAR)parsed.common_cfg.bar;
    Dev->Caps.CommonCfg.Id = (UCHAR)parsed.common_cfg.id;
    Dev->Caps.CommonCfg.CapLen = (UCHAR)parsed.common_cfg.cap_len;
    Dev->Caps.CommonCfg.CapOffset = (ULONG)parsed.common_cfg.cap_offset;
    Dev->Caps.CommonCfg.Offset = (ULONG)parsed.common_cfg.offset;
    Dev->Caps.CommonCfg.Length = (ULONG)parsed.common_cfg.length;

    Dev->Caps.NotifyCfg.Present = TRUE;
    Dev->Caps.NotifyCfg.CfgType = VIRTIO_PCI_CAP_NOTIFY_CFG;
    Dev->Caps.NotifyCfg.Bar = (UCHAR)parsed.notify_cfg.bar;
    Dev->Caps.NotifyCfg.Id = (UCHAR)parsed.notify_cfg.id;
    Dev->Caps.NotifyCfg.CapLen = (UCHAR)parsed.notify_cfg.cap_len;
    Dev->Caps.NotifyCfg.CapOffset = (ULONG)parsed.notify_cfg.cap_offset;
    Dev->Caps.NotifyCfg.Offset = (ULONG)parsed.notify_cfg.offset;
    Dev->Caps.NotifyCfg.Length = (ULONG)parsed.notify_cfg.length;

    Dev->Caps.IsrCfg.Present = TRUE;
    Dev->Caps.IsrCfg.CfgType = VIRTIO_PCI_CAP_ISR_CFG;
    Dev->Caps.IsrCfg.Bar = (UCHAR)parsed.isr_cfg.bar;
    Dev->Caps.IsrCfg.Id = (UCHAR)parsed.isr_cfg.id;
    Dev->Caps.IsrCfg.CapLen = (UCHAR)parsed.isr_cfg.cap_len;
    Dev->Caps.IsrCfg.CapOffset = (ULONG)parsed.isr_cfg.cap_offset;
    Dev->Caps.IsrCfg.Offset = (ULONG)parsed.isr_cfg.offset;
    Dev->Caps.IsrCfg.Length = (ULONG)parsed.isr_cfg.length;

    Dev->Caps.DeviceCfg.Present = TRUE;
    Dev->Caps.DeviceCfg.CfgType = VIRTIO_PCI_CAP_DEVICE_CFG;
    Dev->Caps.DeviceCfg.Bar = (UCHAR)parsed.device_cfg.bar;
    Dev->Caps.DeviceCfg.Id = (UCHAR)parsed.device_cfg.id;
    Dev->Caps.DeviceCfg.CapLen = (UCHAR)parsed.device_cfg.cap_len;
    Dev->Caps.DeviceCfg.CapOffset = (ULONG)parsed.device_cfg.cap_offset;
    Dev->Caps.DeviceCfg.Offset = (ULONG)parsed.device_cfg.offset;
    Dev->Caps.DeviceCfg.Length = (ULONG)parsed.device_cfg.length;

    Dev->Caps.NotifyOffMultiplier = (ULONG)parsed.notify_off_multiplier;

    /*
     * The portable parser returns the required modern capabilities, but not an
     * itemized list of every virtio vendor capability. Populate All[] with the
     * selected required capabilities so VirtioPciModernWdmMapBars knows which
     * BARs to map.
     */
    Dev->Caps.AllCount = 0;
    Dev->Caps.All[Dev->Caps.AllCount++] = Dev->Caps.CommonCfg;
    Dev->Caps.All[Dev->Caps.AllCount++] = Dev->Caps.NotifyCfg;
    Dev->Caps.All[Dev->Caps.AllCount++] = Dev->Caps.IsrCfg;
    Dev->Caps.All[Dev->Caps.AllCount++] = Dev->Caps.DeviceCfg;

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtioPciModernWdmValidateCapInBar(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE *Dev,
                                  _In_ const VIRTIO_PCI_CAP_INFO *Cap,
                                  _In_ SIZE_T RequiredMinLength,
                                  _In_ PCSTR Name)
{
    ULONGLONG end;
    SIZE_T barLength;

    if (!Cap->Present) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Cap->Bar >= VIRTIO_PCI_MAX_BARS) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s references invalid BAR %u\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Dev->Bars[Cap->Bar].IsUpperHalf) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s references upper-half of 64-bit BAR slot %u\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (!Dev->Bars[Cap->Bar].Present || !Dev->Bars[Cap->Bar].IsMemory) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s references non-memory or missing BAR %u\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Dev->Bars[Cap->Bar].Length == 0) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s references BAR %u with no matched resource\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if ((SIZE_T)Cap->Length < RequiredMinLength) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s capability window too small (len=%lu, need>=%Iu)\n",
                                    Name,
                                    (ULONG)Cap->Length,
                                    RequiredMinLength);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    end = (ULONGLONG)Cap->Offset + (ULONGLONG)Cap->Length;
    if (end < (ULONGLONG)Cap->Offset) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s capability window offset/length overflow (off=0x%lx len=0x%lx)\n",
                                    Name,
                                    (ULONG)Cap->Offset,
                                    (ULONG)Cap->Length);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    barLength = Dev->Bars[Cap->Bar].Length;
    if (end > (ULONGLONG)barLength) {
        VIRTIO_PCI_MODERN_WDM_PRINT("%s capability overruns BAR%u (off=0x%lx len=0x%lx bar_len=0x%Ix)\n",
                                    Name,
                                    (UINT)Cap->Bar,
                                    (ULONG)Cap->Offset,
                                    (ULONG)Cap->Length,
                                    barLength);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    return STATUS_SUCCESS;
}

static PCSTR
VirtioPciCfgTypeToString(_In_ UCHAR CfgType)
{
    switch (CfgType) {
    case VIRTIO_PCI_CAP_COMMON_CFG:
        return "COMMON_CFG";
    case VIRTIO_PCI_CAP_NOTIFY_CFG:
        return "NOTIFY_CFG";
    case VIRTIO_PCI_CAP_ISR_CFG:
        return "ISR_CFG";
    case VIRTIO_PCI_CAP_DEVICE_CFG:
        return "DEVICE_CFG";
    case VIRTIO_PCI_CAP_PCI_CFG:
        return "PCI_CFG";
    default:
        return "UNKNOWN";
    }
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernWdmInit(_In_ PDEVICE_OBJECT LowerDeviceObject, _Out_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    NTSTATUS status;
    UCHAR revId;
    ULONG bytesRead;
    ULONG revIdLen;

    if (LowerDeviceObject == NULL || Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
    KeInitializeSpinLock(&Dev->CommonCfgLock);

#if DBG
    Dev->CommonCfgLockOwner = NULL;
#endif

    status = VirtioPciWdmQueryInterface(LowerDeviceObject,
                                        &GUID_PCI_BUS_INTERFACE_STANDARD,
                                        (USHORT)sizeof(Dev->PciInterface),
                                        (USHORT)PCI_BUS_INTERFACE_STANDARD_VERSION,
                                        (PINTERFACE)&Dev->PciInterface);
    if (!NT_SUCCESS(status)) {
        VIRTIO_PCI_MODERN_WDM_PRINT("IRP_MN_QUERY_INTERFACE(PCI_BUS_INTERFACE_STANDARD) failed: 0x%08x\n", status);
        VirtioPciModernWdmUninit(Dev);
        return status;
    }

    if (Dev->PciInterface.InterfaceReference != NULL) {
        Dev->PciInterface.InterfaceReference(Dev->PciInterface.Context);
        Dev->PciInterfaceAcquired = TRUE;
    }

    /* Optional contract enforcement: PCI Revision ID must be 0x01 (AERO-W7-VIRTIO v1). */
    revId = 0;
    revIdLen = (ULONG)sizeof(revId);
    bytesRead = VirtioPciReadConfig(&Dev->PciInterface, &revId, 0x08, revIdLen);
    if (bytesRead != revIdLen) {
        VIRTIO_PCI_MODERN_WDM_PRINT("PCI revision ID config read failed (%lu/%lu)\n", bytesRead, revIdLen);
        VirtioPciModernWdmUninit(Dev);
        return STATUS_DEVICE_DATA_ERROR;
    }
    Dev->PciRevisionId = revId;

    if (Dev->PciRevisionId != 0x01) {
        VIRTIO_PCI_MODERN_WDM_PRINT("Unsupported PCI Revision ID 0x%02X (expected 0x01)\n", Dev->PciRevisionId);
        VirtioPciModernWdmUninit(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    status = VirtioPciModernWdmReadBarsFromConfig(Dev);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUninit(Dev);
        return status;
    }

    status = VirtioPciModernWdmDiscoverCaps(Dev);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUninit(Dev);
        return status;
    }

    /*
     * Aero contract v1 fixes all capabilities to BAR0. Keep the mapping logic
     * generic, but reject non-conforming devices up front.
     */
    if (Dev->Caps.CommonCfg.Bar != 0 || Dev->Caps.NotifyCfg.Bar != 0 || Dev->Caps.IsrCfg.Bar != 0 || Dev->Caps.DeviceCfg.Bar != 0) {
        VIRTIO_PCI_MODERN_WDM_PRINT(
            "Device does not conform to Aero contract: expected all virtio caps in BAR0 (common=%u notify=%u isr=%u dev=%u)\n",
            (UINT)Dev->Caps.CommonCfg.Bar,
            (UINT)Dev->Caps.NotifyCfg.Bar,
            (UINT)Dev->Caps.IsrCfg.Bar,
            (UINT)Dev->Caps.DeviceCfg.Bar);
        VirtioPciModernWdmUninit(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernWdmMapBars(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                          _In_ PCM_RESOURCE_LIST ResourcesRaw,
                          _In_ PCM_RESOURCE_LIST ResourcesTranslated)
{
    NTSTATUS status;
    ULONG requiredMask;
    ULONG bar;
    ULONG full;

    if (Dev == NULL || ResourcesRaw == NULL || ResourcesTranslated == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Re-start is possible (PnP stop/start). Always start from a clean state. */
    VirtioPciModernWdmUnmapBars(Dev);

    status = VirtioPciModernWdmReadBarsFromConfig(Dev);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    requiredMask = 0;
    for (bar = 0; bar < Dev->Caps.AllCount; bar++) {
        const VIRTIO_PCI_CAP_INFO *c;
        c = &Dev->Caps.All[bar];
        if (!c->Present) {
            continue;
        }

        if (c->Bar < VIRTIO_PCI_MAX_BARS) {
            requiredMask |= (1u << c->Bar);
        }
    }

    /*
     * Match BARs to resources: locate memory descriptors in ResourcesRaw that
     * correspond to the base addresses programmed in PCI config space.
     *
     * The raw and translated resource lists are index-aligned: descriptor N in
     * ResourcesRaw corresponds to descriptor N in ResourcesTranslated.
     */
    {
        ULONG fullCount;

        fullCount = ResourcesRaw->Count;
        if (ResourcesTranslated->Count < fullCount) {
            fullCount = ResourcesTranslated->Count;
        }

        for (full = 0; full < fullCount; full++) {
            PCM_FULL_RESOURCE_DESCRIPTOR rawFull;
            PCM_FULL_RESOURCE_DESCRIPTOR transFull;
            ULONG partialCount;
            ULONG i;

            rawFull = &ResourcesRaw->List[full];
            transFull = &ResourcesTranslated->List[full];

            partialCount = rawFull->PartialResourceList.Count;
            if (transFull->PartialResourceList.Count < partialCount) {
                partialCount = transFull->PartialResourceList.Count;
            }

            for (i = 0; i < partialCount; i++) {
                PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
                PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
                ULONGLONG rawStart;
                SIZE_T rawLen;

                rawDesc = &rawFull->PartialResourceList.PartialDescriptors[i];
                transDesc = &transFull->PartialResourceList.PartialDescriptors[i];

                if (rawDesc->Type != CmResourceTypeMemory) {
                    continue;
                }

                rawStart = (ULONGLONG)rawDesc->u.Memory.Start.QuadPart;
                rawLen = (SIZE_T)rawDesc->u.Memory.Length;

                for (bar = 0; bar < VIRTIO_PCI_MAX_BARS; bar++) {
                    if ((requiredMask & (1u << bar)) == 0) {
                        continue;
                    }

                    if (!Dev->Bars[bar].Present || !Dev->Bars[bar].IsMemory || Dev->Bars[bar].IsUpperHalf) {
                        continue;
                    }

                    if (Dev->Bars[bar].Base != rawStart) {
                        continue;
                    }

                    if (Dev->Bars[bar].Length != 0) {
                        VIRTIO_PCI_MODERN_WDM_PRINT("BAR%lu matches multiple resources (keeping first)\n", bar);
                        continue;
                    }

                    Dev->Bars[bar].RawStart = rawDesc->u.Memory.Start;
                    Dev->Bars[bar].TranslatedStart = transDesc->u.Memory.Start;
                    Dev->Bars[bar].Length = rawLen;
                }
            }
        }
    }

    /* Ensure every required BAR has a matched resource. */
    for (bar = 0; bar < VIRTIO_PCI_MAX_BARS; bar++) {
        if ((requiredMask & (1u << bar)) == 0) {
            continue;
        }

        if (Dev->Bars[bar].Length == 0) {
            VIRTIO_PCI_MODERN_WDM_PRINT("Required BAR%lu (base=0x%I64x) has no matching CM resource\n", bar, Dev->Bars[bar].Base);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    /* Map each required BAR once. */
    for (bar = 0; bar < VIRTIO_PCI_MAX_BARS; bar++) {
        if ((requiredMask & (1u << bar)) == 0) {
            continue;
        }

        Dev->Bars[bar].Va = MmMapIoSpace(Dev->Bars[bar].TranslatedStart, Dev->Bars[bar].Length, MmNonCached);
        if (Dev->Bars[bar].Va == NULL) {
            VIRTIO_PCI_MODERN_WDM_PRINT("MmMapIoSpace failed for BAR%lu (phys=0x%I64x len=0x%Ix)\n",
                                        bar,
                                        (ULONGLONG)Dev->Bars[bar].TranslatedStart.QuadPart,
                                        Dev->Bars[bar].Length);
            VirtioPciModernWdmUnmapBars(Dev);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
    }

    /* Validate required capability windows against BAR lengths. */
    status = VirtioPciModernWdmValidateCapInBar(Dev, &Dev->Caps.CommonCfg, sizeof(virtio_pci_common_cfg), "COMMON_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUnmapBars(Dev);
        return status;
    }

    /* Notify register writes are 16-bit MMIO. */
    status = VirtioPciModernWdmValidateCapInBar(Dev, &Dev->Caps.NotifyCfg, sizeof(USHORT), "NOTIFY_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUnmapBars(Dev);
        return status;
    }

    status = VirtioPciModernWdmValidateCapInBar(Dev, &Dev->Caps.IsrCfg, 1, "ISR_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUnmapBars(Dev);
        return status;
    }

    status = VirtioPciModernWdmValidateCapInBar(Dev, &Dev->Caps.DeviceCfg, 1, "DEVICE_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUnmapBars(Dev);
        return status;
    }

    if (Dev->Caps.NotifyOffMultiplier == 0) {
        VIRTIO_PCI_MODERN_WDM_PRINT("NOTIFY_CFG has invalid notify_off_multiplier=0\n");
        VirtioPciModernWdmUnmapBars(Dev);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    /*
     * Validate every discovered virtio vendor capability against the
     * corresponding BAR resource length (defensive against malformed devices).
     */
    for (bar = 0; bar < Dev->Caps.AllCount; bar++) {
        const VIRTIO_PCI_CAP_INFO *c;
        ULONGLONG end;

        c = &Dev->Caps.All[bar];
        if (!c->Present) {
            continue;
        }

        if (c->Bar >= VIRTIO_PCI_MAX_BARS) {
            VIRTIO_PCI_MODERN_WDM_PRINT("Virtio cap at 0x%02lx references invalid BAR %u\n",
                                        (ULONG)c->CapOffset,
                                        (UINT)c->Bar);
            VirtioPciModernWdmUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (Dev->Bars[c->Bar].IsUpperHalf) {
            VIRTIO_PCI_MODERN_WDM_PRINT("Virtio cap at 0x%02lx references upper-half BAR slot %u\n",
                                        (ULONG)c->CapOffset,
                                        (UINT)c->Bar);
            VirtioPciModernWdmUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (!Dev->Bars[c->Bar].Present || !Dev->Bars[c->Bar].IsMemory || Dev->Bars[c->Bar].Length == 0) {
            VIRTIO_PCI_MODERN_WDM_PRINT("Virtio cap at 0x%02lx references unmapped BAR %u\n",
                                        (ULONG)c->CapOffset,
                                        (UINT)c->Bar);
            VirtioPciModernWdmUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        end = (ULONGLONG)c->Offset + (ULONGLONG)c->Length;
        if (end < (ULONGLONG)c->Offset || end > (ULONGLONG)Dev->Bars[c->Bar].Length) {
            VIRTIO_PCI_MODERN_WDM_PRINT("Virtio cap at 0x%02lx overruns BAR%u (off=0x%lx len=0x%lx bar_len=0x%Ix)\n",
                                        (ULONG)c->CapOffset,
                                        (UINT)c->Bar,
                                        (ULONG)c->Offset,
                                        (ULONG)c->Length,
                                        Dev->Bars[c->Bar].Length);
            VirtioPciModernWdmUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    /* Expose per-capability virtual addresses. */
    Dev->CommonCfg =
        (volatile virtio_pci_common_cfg *)((PUCHAR)Dev->Bars[Dev->Caps.CommonCfg.Bar].Va + Dev->Caps.CommonCfg.Offset);

    Dev->NotifyBase = (volatile UCHAR *)((PUCHAR)Dev->Bars[Dev->Caps.NotifyCfg.Bar].Va + Dev->Caps.NotifyCfg.Offset);
    Dev->NotifyOffMultiplier = Dev->Caps.NotifyOffMultiplier;
    Dev->NotifyLength = (SIZE_T)Dev->Caps.NotifyCfg.Length;

    Dev->IsrStatus = (volatile UCHAR *)((PUCHAR)Dev->Bars[Dev->Caps.IsrCfg.Bar].Va + Dev->Caps.IsrCfg.Offset);

    Dev->DeviceCfg = (volatile UCHAR *)((PUCHAR)Dev->Bars[Dev->Caps.DeviceCfg.Bar].Va + Dev->Caps.DeviceCfg.Offset);

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmUninit(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernWdmUnmapBars(Dev);

    if (Dev->PciInterfaceAcquired && Dev->PciInterface.InterfaceDereference != NULL) {
        Dev->PciInterface.InterfaceDereference(Dev->PciInterface.Context);
        Dev->PciInterfaceAcquired = FALSE;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmDumpCaps(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE *Dev)
{
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    VIRTIO_PCI_MODERN_WDM_PRINT("Virtio modern capabilities (%lu total):\n", Dev->Caps.AllCount);
    for (i = 0; i < Dev->Caps.AllCount; i++) {
        const VIRTIO_PCI_CAP_INFO *c;
        c = &Dev->Caps.All[i];
        VIRTIO_PCI_MODERN_WDM_PRINT("  - cfg_type=%u (%s) bar=%u off=0x%lx len=0x%lx cap_off=0x%02lx cap_len=%u\n",
                                    (UINT)c->CfgType,
                                    VirtioPciCfgTypeToString(c->CfgType),
                                    (UINT)c->Bar,
                                    (ULONG)c->Offset,
                                    (ULONG)c->Length,
                                    (ULONG)c->CapOffset,
                                    (UINT)c->CapLen);
    }

    VIRTIO_PCI_MODERN_WDM_PRINT("Selected:\n");
    VIRTIO_PCI_MODERN_WDM_PRINT("  COMMON_CFG: bar=%u off=0x%lx len=0x%lx va=%p\n",
                                (UINT)Dev->Caps.CommonCfg.Bar,
                                (ULONG)Dev->Caps.CommonCfg.Offset,
                                (ULONG)Dev->Caps.CommonCfg.Length,
                                Dev->CommonCfg);
    VIRTIO_PCI_MODERN_WDM_PRINT("  NOTIFY_CFG: bar=%u off=0x%lx len=0x%lx va=%p mult=0x%lx\n",
                                (UINT)Dev->Caps.NotifyCfg.Bar,
                                (ULONG)Dev->Caps.NotifyCfg.Offset,
                                (ULONG)Dev->Caps.NotifyCfg.Length,
                                Dev->NotifyBase,
                                (ULONG)Dev->NotifyOffMultiplier);
    VIRTIO_PCI_MODERN_WDM_PRINT("  ISR_CFG:    bar=%u off=0x%lx len=0x%lx va=%p\n",
                                (UINT)Dev->Caps.IsrCfg.Bar,
                                (ULONG)Dev->Caps.IsrCfg.Offset,
                                (ULONG)Dev->Caps.IsrCfg.Length,
                                Dev->IsrStatus);
    VIRTIO_PCI_MODERN_WDM_PRINT("  DEVICE_CFG: bar=%u off=0x%lx len=0x%lx va=%p\n",
                                (UINT)Dev->Caps.DeviceCfg.Bar,
                                (ULONG)Dev->Caps.DeviceCfg.Offset,
                                (ULONG)Dev->Caps.DeviceCfg.Length,
                                Dev->DeviceCfg);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmDumpBars(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE *Dev)
{
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    VIRTIO_PCI_MODERN_WDM_PRINT("PCI BARs:\n");
    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        const VIRTIO_PCI_MODERN_WDM_BAR *b;
        b = &Dev->Bars[i];

        VIRTIO_PCI_MODERN_WDM_PRINT(
            "  BAR%lu: present=%u mem=%u 64=%u upper=%u base=0x%I64x raw=0x%I64x trans=0x%I64x len=0x%Ix va=%p\n",
            i,
            (UINT)b->Present,
            (UINT)b->IsMemory,
            (UINT)b->Is64Bit,
            (UINT)b->IsUpperHalf,
            b->Base,
            (ULONGLONG)b->RawStart.QuadPart,
            (ULONGLONG)b->TranslatedStart.QuadPart,
            b->Length,
            b->Va);
    }
}

/* -------------------------------------------------------------------------- */
/* CommonCfg selector serialization helpers                                    */
/* -------------------------------------------------------------------------- */

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgAcquire(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _Out_ PKIRQL OldIrql)
{
#if DBG
    PKTHREAD currentThread;
#endif

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(OldIrql != NULL);
    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

#if DBG
    currentThread = KeGetCurrentThread();
    NT_ASSERT(Dev->CommonCfgLockOwner != currentThread);
#endif

    KeAcquireSpinLock(&Dev->CommonCfgLock, OldIrql);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == NULL);
    Dev->CommonCfgLockOwner = currentThread;
#endif
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgRelease(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ KIRQL OldIrql)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(KeGetCurrentIrql() == DISPATCH_LEVEL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
    Dev->CommonCfgLockOwner = NULL;
#endif

    KeReleaseSpinLock(&Dev->CommonCfgLock, OldIrql);
}

static __forceinline VOID
VirtioPciSelectQueueLocked(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex)
{
#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_select, QueueIndex);
    KeMemoryBarrier();
}

static __forceinline UCHAR
VirtioPciReadDeviceStatus(_In_ const PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->device_status);
}

static __forceinline VOID
VirtioPciWriteDeviceStatus(_In_ const PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UCHAR Status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->device_status, Status);
}

/* -------------------------------------------------------------------------- */
/* Status/reset helpers                                                       */
/* -------------------------------------------------------------------------- */

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciResetDevice(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    ULONG waitedUs;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    VirtioPciWriteDeviceStatus(Dev, 0);
    KeMemoryBarrier();

    for (waitedUs = 0; waitedUs < VIRTIO_PCI_RESET_TIMEOUT_US; waitedUs += VIRTIO_PCI_RESET_POLL_DELAY_US) {
        if (VirtioPciReadDeviceStatus(Dev) == 0) {
            KeMemoryBarrier();
            return;
        }

        KeStallExecutionProcessor(VIRTIO_PCI_RESET_POLL_DELAY_US);
    }

    VIRTIO_PCI_MODERN_WDM_PRINT("Device reset timed out (status still 0x%02X)\n", VirtioPciReadDeviceStatus(Dev));
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciAddStatus(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UCHAR Bits)
{
    UCHAR status;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    status = VirtioPciReadDeviceStatus(Dev);
    status |= Bits;
    VirtioPciWriteDeviceStatus(Dev, status);
    KeMemoryBarrier();
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UCHAR
VirtioPciGetStatus(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    KeMemoryBarrier();
    return VirtioPciReadDeviceStatus(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciFailDevice(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FAILED);
}

/* -------------------------------------------------------------------------- */
/* Feature negotiation                                                        */
/* -------------------------------------------------------------------------- */

static UINT64
VirtioPciReadDeviceFeaturesLocked(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    ULONG lo;
    ULONG hi;

    lo = 0;
    hi = 0;

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature_select, 0);
    KeMemoryBarrier();
    lo = READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature);
    KeMemoryBarrier();

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature_select, 1);
    KeMemoryBarrier();
    hi = READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature);
    KeMemoryBarrier();

    return ((UINT64)hi << 32) | lo;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    KIRQL oldIrql;
    UINT64 features;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    features = 0;

    VirtioPciCommonCfgAcquire(Dev, &oldIrql);
    features = VirtioPciReadDeviceFeaturesLocked(Dev);
    VirtioPciCommonCfgRelease(Dev, oldIrql);

    return features;
}

static VOID
VirtioPciWriteDriverFeaturesLocked(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UINT64 Features)
{
    ULONG lo;
    ULONG hi;

    lo = (ULONG)(Features & 0xFFFFFFFFui64);
    hi = (ULONG)(Features >> 32);

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature_select, 0);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature, lo);
    KeMemoryBarrier();

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature_select, 1);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature, hi);
    KeMemoryBarrier();
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UINT64 Features)
{
    KIRQL oldIrql;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    VirtioPciCommonCfgAcquire(Dev, &oldIrql);
    VirtioPciWriteDriverFeaturesLocked(Dev, Features);
    VirtioPciCommonCfgRelease(Dev, oldIrql);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                           _In_ UINT64 Required,
                           _In_ UINT64 Wanted,
                           _Out_ UINT64 *NegotiatedOut)
{
    UINT64 deviceFeatures;
    UINT64 negotiated;
    UCHAR status;

    if (NegotiatedOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *NegotiatedOut = 0;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    Required |= VIRTIO_F_VERSION_1;

    VirtioPciResetDevice(Dev);

    VirtioPciAddStatus(Dev, VIRTIO_STATUS_ACKNOWLEDGE);
    VirtioPciAddStatus(Dev, VIRTIO_STATUS_DRIVER);

    deviceFeatures = VirtioPciReadDeviceFeatures(Dev);

    if ((deviceFeatures & Required) != Required) {
        VIRTIO_PCI_MODERN_WDM_PRINT("Device missing required features: device=0x%I64X required=0x%I64X\n",
                                    deviceFeatures,
                                    Required);
        VirtioPciFailDevice(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    negotiated = (deviceFeatures & Wanted) | Required;

    VIRTIO_PCI_MODERN_WDM_PRINT("Virtio feature negotiation: device=0x%I64X required=0x%I64X wanted=0x%I64X negotiated=0x%I64X\n",
                                deviceFeatures,
                                Required,
                                Wanted,
                                negotiated);

    VirtioPciWriteDriverFeatures(Dev, negotiated);
    KeMemoryBarrier();

    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FEATURES_OK);

    status = VirtioPciGetStatus(Dev);
    if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
        VIRTIO_PCI_MODERN_WDM_PRINT("Device rejected FEATURES_OK (status=0x%02X)\n", status);
        VirtioPciFailDevice(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    *NegotiatedOut = negotiated;
    return STATUS_SUCCESS;
}

/* -------------------------------------------------------------------------- */
/* Device config access                                                       */
/* -------------------------------------------------------------------------- */

static UCHAR
VirtioPciReadCfg8(_In_ volatile const VOID *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_UCHAR((PUCHAR)((ULONG_PTR)Base + Offset));
}

static VOID
VirtioPciWriteCfg8(_In_ volatile VOID *Base, _In_ ULONG Offset, _In_ UCHAR Value)
{
    WRITE_REGISTER_UCHAR((PUCHAR)((ULONG_PTR)Base + Offset), Value);
}

static USHORT
VirtioPciReadCfg16(_In_ volatile const VOID *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_USHORT((PUSHORT)((ULONG_PTR)Base + Offset));
}

static VOID
VirtioPciWriteCfg16(_In_ volatile VOID *Base, _In_ ULONG Offset, _In_ USHORT Value)
{
    WRITE_REGISTER_USHORT((PUSHORT)((ULONG_PTR)Base + Offset), Value);
}

static ULONG
VirtioPciReadCfg32(_In_ volatile const VOID *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_ULONG((PULONG)((ULONG_PTR)Base + Offset));
}

static VOID
VirtioPciWriteCfg32(_In_ volatile VOID *Base, _In_ ULONG Offset, _In_ ULONG Value)
{
    WRITE_REGISTER_ULONG((PULONG)((ULONG_PTR)Base + Offset), Value);
}

static VOID
VirtioPciCopyFromDevice(_In_ volatile const UCHAR *Base,
                        _In_ ULONG Offset,
                        _Out_writes_bytes_(Length) UCHAR *OutBytes,
                        _In_ ULONG Length)
{
    ULONG i = 0;

    while (i < Length && ((Offset + i) & 3u) != 0) {
        OutBytes[i] = VirtioPciReadCfg8(Base, Offset + i);
        i++;
    }

    while (Length - i >= sizeof(ULONG)) {
        ULONG v32 = VirtioPciReadCfg32(Base, Offset + i);
        RtlCopyMemory(OutBytes + i, &v32, sizeof(v32));
        i += sizeof(ULONG);
    }

    while (Length - i >= sizeof(USHORT)) {
        USHORT v16 = VirtioPciReadCfg16(Base, Offset + i);
        RtlCopyMemory(OutBytes + i, &v16, sizeof(v16));
        i += sizeof(USHORT);
    }

    while (i < Length) {
        OutBytes[i] = VirtioPciReadCfg8(Base, Offset + i);
        i++;
    }
}

static VOID
VirtioPciCopyToDevice(_In_ volatile UCHAR *Base,
                      _In_ ULONG Offset,
                      _In_reads_bytes_(Length) const UCHAR *InBytes,
                      _In_ ULONG Length)
{
    ULONG i = 0;

    while (i < Length && ((Offset + i) & 3u) != 0) {
        VirtioPciWriteCfg8(Base, Offset + i, InBytes[i]);
        i++;
    }

    while (Length - i >= sizeof(ULONG)) {
        ULONG v32;
        RtlCopyMemory(&v32, InBytes + i, sizeof(v32));
        VirtioPciWriteCfg32(Base, Offset + i, v32);
        i += sizeof(ULONG);
    }

    while (Length - i >= sizeof(USHORT)) {
        USHORT v16;
        RtlCopyMemory(&v16, InBytes + i, sizeof(v16));
        VirtioPciWriteCfg16(Base, Offset + i, v16);
        i += sizeof(USHORT);
    }

    while (i < Length) {
        VirtioPciWriteCfg8(Base, Offset + i, InBytes[i]);
        i++;
    }
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciReadDeviceConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                          _In_ ULONG Offset,
                          _Out_writes_bytes_(Length) PVOID Buffer,
                          _In_ ULONG Length)
{
    ULONG attempt;
    UCHAR gen0;
    UCHAR gen1;
    PUCHAR outBytes;
    ULONGLONG end;

    if (Length == 0) {
        return STATUS_SUCCESS;
    }

    if (Dev == NULL || Dev->CommonCfg == NULL || Dev->DeviceCfg == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    end = (ULONGLONG)Offset + (ULONGLONG)Length;
    if (end < Offset) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dev->Caps.DeviceCfg.Length != 0 && end > Dev->Caps.DeviceCfg.Length) {
        return STATUS_INVALID_PARAMETER;
    }

    outBytes = (PUCHAR)Buffer;

    for (attempt = 0; attempt < VIRTIO_PCI_CONFIG_MAX_READ_RETRIES; attempt++) {
        gen0 = READ_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->config_generation);
        KeMemoryBarrier();

        VirtioPciCopyFromDevice(Dev->DeviceCfg, Offset, outBytes, Length);

        KeMemoryBarrier();
        gen1 = READ_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->config_generation);
        KeMemoryBarrier();

        if (gen0 == gen1) {
            return STATUS_SUCCESS;
        }
    }

    return STATUS_IO_TIMEOUT;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciWriteDeviceConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                           _In_ ULONG Offset,
                           _In_reads_bytes_(Length) const VOID *Buffer,
                           _In_ ULONG Length)
{
    const UCHAR *inBytes;
    ULONGLONG end;

    if (Length == 0) {
        return STATUS_SUCCESS;
    }

    if (Dev == NULL || Dev->DeviceCfg == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    end = (ULONGLONG)Offset + (ULONGLONG)Length;
    if (end < Offset) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dev->Caps.DeviceCfg.Length != 0 && end > Dev->Caps.DeviceCfg.Length) {
        return STATUS_INVALID_PARAMETER;
    }

    inBytes = (const UCHAR *)Buffer;
    VirtioPciCopyToDevice(Dev->DeviceCfg, Offset, inBytes, Length);

    KeMemoryBarrier();
    return STATUS_SUCCESS;
}

/* -------------------------------------------------------------------------- */
/* Queue helpers                                                              */
/* -------------------------------------------------------------------------- */

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciGetNumQueues(_In_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->num_queues);
}

static USHORT
VirtioPciReadQueueSizeLocked(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex)
{
    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueSize(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex, _Out_ USHORT *SizeOut)
{
    KIRQL oldIrql;
    USHORT size;

    if (SizeOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *SizeOut = 0;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    VirtioPciCommonCfgAcquire(Dev, &oldIrql);
    size = VirtioPciReadQueueSizeLocked(Dev, QueueIndex);
    VirtioPciCommonCfgRelease(Dev, oldIrql);

    if (size == 0) {
        return STATUS_NOT_FOUND;
    }

    *SizeOut = size;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciSetupQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                    _In_ USHORT QueueIndex,
                    _In_ ULONGLONG DescPa,
                    _In_ ULONGLONG AvailPa,
                    _In_ ULONGLONG UsedPa)
{
    NTSTATUS status;
    KIRQL oldIrql;
    USHORT size;
    USHORT enabled;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = STATUS_SUCCESS;
    enabled = 0;

    VirtioPciCommonCfgAcquire(Dev, &oldIrql);

    VirtioPciSelectQueueLocked(Dev, QueueIndex);

    size = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
    if (size == 0) {
        status = STATUS_NOT_FOUND;
        goto Exit;
    }

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_lo, (ULONG)(DescPa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_hi, (ULONG)(DescPa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_lo, (ULONG)(AvailPa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_hi, (ULONG)(AvailPa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_lo, (ULONG)(UsedPa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_hi, (ULONG)(UsedPa >> 32));

    /*
     * The device must observe the ring addresses before queue_enable is set.
     */
    KeMemoryBarrier();

    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable, 1);

    /* Optional readback confirmation. */
    enabled = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable);
    if (enabled != 1) {
        status = STATUS_IO_DEVICE_ERROR;
        goto Exit;
    }

Exit:
    VirtioPciCommonCfgRelease(Dev, oldIrql);
    return status;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDisableQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex)
{
    KIRQL oldIrql;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    VirtioPciCommonCfgAcquire(Dev, &oldIrql);
    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable, 0);
    KeMemoryBarrier();
    VirtioPciCommonCfgRelease(Dev, oldIrql);
}
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                               _In_ USHORT QueueIndex,
                               _Out_ volatile UINT16 **NotifyAddrOut)
{
    KIRQL oldIrql;
    USHORT size;
    USHORT notifyOff;
    ULONGLONG offset;

    if (NotifyAddrOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *NotifyAddrOut = NULL;

    if (Dev == NULL || Dev->CommonCfg == NULL || Dev->NotifyBase == NULL || Dev->NotifyOffMultiplier == 0 ||
        Dev->NotifyLength < sizeof(UINT16)) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    VirtioPciCommonCfgAcquire(Dev, &oldIrql);
    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    size = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
    notifyOff = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_notify_off);
    VirtioPciCommonCfgRelease(Dev, oldIrql);

    if (size == 0) {
        return STATUS_NOT_FOUND;
    }

    offset = (ULONGLONG)notifyOff * (ULONGLONG)Dev->NotifyOffMultiplier;
    if (offset + sizeof(UINT16) > Dev->NotifyLength) {
        return STATUS_IO_DEVICE_ERROR;
    }

    *NotifyAddrOut = (volatile UINT16 *)((volatile UCHAR *)Dev->NotifyBase + offset);
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciNotifyQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex)
{
    volatile UINT16 *notifyAddr;

    if (Dev == NULL) {
        return;
    }

    notifyAddr = NULL;
    if (Dev->QueueNotifyAddrCache != NULL && QueueIndex < Dev->QueueNotifyAddrCacheCount) {
        notifyAddr = Dev->QueueNotifyAddrCache[QueueIndex];
    }

    if (notifyAddr == NULL) {
        if (!NT_SUCCESS(VirtioPciGetQueueNotifyAddress(Dev, QueueIndex, &notifyAddr))) {
            return;
        }

        if (Dev->QueueNotifyAddrCache != NULL && QueueIndex < Dev->QueueNotifyAddrCacheCount) {
            Dev->QueueNotifyAddrCache[QueueIndex] = notifyAddr;
        }
    }

    WRITE_REGISTER_USHORT((volatile USHORT *)notifyAddr, QueueIndex);

    /* Compiler/CPU barrier after notify write (hot path, safe at DISPATCH_LEVEL). */
    KeMemoryBarrier();
}

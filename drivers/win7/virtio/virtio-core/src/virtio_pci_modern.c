#include "../include/virtio_pci_modern.h"

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

#define VIRTIO_PCI_RESET_TIMEOUT_US        1000000u
#define VIRTIO_PCI_RESET_POLL_DELAY_US     1000u
#define VIRTIO_PCI_CONFIG_MAX_READ_RETRIES 10u

static ULONG
VirtioPciReadConfig(
    _In_ PPCI_BUS_INTERFACE_STANDARD PciInterface,
    _Out_writes_bytes_(Length) PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length)
{
    if (PciInterface->ReadConfig != NULL) {
        return PciInterface->ReadConfig(
            PciInterface->Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
    }

    return 0;
}

static VOID
VirtioPciModernUnmapBars(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    ULONG i;

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
VirtioPciModernReadBarsFromConfig(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    ULONG barRegs[VIRTIO_PCI_MAX_BARS];
    ULONG bytesRead;
    ULONG i;

    RtlZeroMemory(barRegs, sizeof(barRegs));
    bytesRead = VirtioPciReadConfig(&Dev->PciInterface, barRegs, 0x10, sizeof(barRegs));
    if (bytesRead != sizeof(barRegs)) {
        VIRTIO_CORE_PRINT("PCI BAR config read failed (%lu/%lu)\n", bytesRead, (ULONG)sizeof(barRegs));
        return STATUS_DEVICE_DATA_ERROR;
    }

    /*
     * Preserve mapped VA/length until VirtioPciModernUnmapBars is called, but
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
                    VIRTIO_CORE_PRINT("BAR%lu claims to be 64-bit but has no high dword\n", i);
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
VirtioPciModernValidateCapInBar(
    _In_ const VIRTIO_PCI_MODERN_DEVICE *Dev,
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
        VIRTIO_CORE_PRINT("%s references invalid BAR %u\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Dev->Bars[Cap->Bar].IsUpperHalf) {
        VIRTIO_CORE_PRINT("%s references upper-half of 64-bit BAR slot %u\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (!Dev->Bars[Cap->Bar].Present || !Dev->Bars[Cap->Bar].IsMemory) {
        VIRTIO_CORE_PRINT("%s references non-memory or missing BAR %u\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Dev->Bars[Cap->Bar].Length == 0) {
        VIRTIO_CORE_PRINT("%s references BAR %u with no matched resource\n", Name, (UINT)Cap->Bar);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Cap->Length < RequiredMinLength) {
        VIRTIO_CORE_PRINT("%s capability window too small (len=%lu, need>=%lu)\n",
                          Name,
                          (ULONG)Cap->Length,
                          (ULONG)RequiredMinLength);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    end = (ULONGLONG)Cap->Offset + (ULONGLONG)Cap->Length;
    barLength = Dev->Bars[Cap->Bar].Length;
    if (end > (ULONGLONG)barLength) {
        VIRTIO_CORE_PRINT("%s capability overruns BAR%u (off=0x%lx len=0x%lx bar_len=0x%Ix)\n",
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
VirtioPciModernInit(_In_ WDFDEVICE WdfDevice, _Out_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    NTSTATUS status;
    WDF_OBJECT_ATTRIBUTES attributes;

    if (Dev == NULL || WdfDevice == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
    Dev->WdfDevice = WdfDevice;

    /* Create the CommonCfg selector serialization lock. */
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = WdfDevice;

    status = WdfSpinLockCreate(&attributes, &Dev->CommonCfgLock);
    if (!NT_SUCCESS(status)) {
        Dev->CommonCfgLock = NULL;
        VirtioPciModernUninit(Dev);
        return status;
    }

#if DBG
    Dev->CommonCfgLockOwner = NULL;
#endif

    status = WdfFdoQueryForInterface(WdfDevice,
                                     &GUID_PCI_BUS_INTERFACE_STANDARD,
                                     (PINTERFACE)&Dev->PciInterface,
                                     (USHORT)sizeof(Dev->PciInterface),
                                     (USHORT)PCI_BUS_INTERFACE_STANDARD_VERSION,
                                     NULL);
    if (!NT_SUCCESS(status)) {
        VIRTIO_CORE_PRINT("WdfFdoQueryForInterface(PCI_BUS_INTERFACE_STANDARD) failed: 0x%08x\n", status);
        VirtioPciModernUninit(Dev);
        return status;
    }

    if (Dev->PciInterface.InterfaceReference != NULL) {
        Dev->PciInterface.InterfaceReference(Dev->PciInterface.Context);
        Dev->PciInterfaceAcquired = TRUE;
    }

    status = VirtioPciModernReadBarsFromConfig(Dev);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUninit(Dev);
        return status;
    }

    {
        ULONGLONG barBases[VIRTIO_PCI_MAX_BARS];
        ULONG i;

        RtlZeroMemory(barBases, sizeof(barBases));
        for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
            barBases[i] = Dev->Bars[i].Base;
        }

        status = VirtioPciCapsDiscover(&Dev->PciInterface, barBases, &Dev->Caps);
    }
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUninit(Dev);
        return status;
    }

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernMapBars(
    _Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    NTSTATUS status;
    ULONG requiredMask;
    ULONG i;
    ULONG resCount;

    if (Dev == NULL || ResourcesRaw == NULL || ResourcesTranslated == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Re-prepare is possible (PnP stop/start). Always start from a clean state. */
    VirtioPciModernUnmapBars(Dev);

    status = VirtioPciModernReadBarsFromConfig(Dev);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    requiredMask = 0;
    for (i = 0; i < Dev->Caps.AllCount; i++) {
        const VIRTIO_PCI_CAP_INFO *c;
        c = &Dev->Caps.All[i];
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
     * The WDF resource lists are index-aligned: descriptor N in ResourcesRaw
     * corresponds to descriptor N in ResourcesTranslated.
     */
    resCount = WdfCmResourceListGetCount(ResourcesRaw);
    for (i = 0; i < resCount; i++) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
        PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
        ULONGLONG rawStart;
        SIZE_T rawLen;
        ULONG bar;

        rawDesc = WdfCmResourceListGetDescriptor(ResourcesRaw, i);
        transDesc = WdfCmResourceListGetDescriptor(ResourcesTranslated, i);

        if (rawDesc == NULL || transDesc == NULL) {
            continue;
        }

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
                VIRTIO_CORE_PRINT("BAR%lu matches multiple resources (keeping first)\n", bar);
                continue;
            }

            Dev->Bars[bar].RawStart = rawDesc->u.Memory.Start;
            Dev->Bars[bar].TranslatedStart = transDesc->u.Memory.Start;
            Dev->Bars[bar].Length = rawLen;
        }
    }

    /* Ensure every required BAR has a matched resource. */
    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        if ((requiredMask & (1u << i)) == 0) {
            continue;
        }

        if (Dev->Bars[i].Length == 0) {
            VIRTIO_CORE_PRINT("Required BAR%lu (base=0x%I64x) has no matching CM resource\n",
                              i,
                              Dev->Bars[i].Base);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    /* Map each required BAR once. */
    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        if ((requiredMask & (1u << i)) == 0) {
            continue;
        }

        Dev->Bars[i].Va = MmMapIoSpace(Dev->Bars[i].TranslatedStart, Dev->Bars[i].Length, MmNonCached);
        if (Dev->Bars[i].Va == NULL) {
            VIRTIO_CORE_PRINT("MmMapIoSpace failed for BAR%lu (phys=0x%I64x len=0x%Ix)\n",
                              i,
                              (ULONGLONG)Dev->Bars[i].TranslatedStart.QuadPart,
                              Dev->Bars[i].Length);
            VirtioPciModernUnmapBars(Dev);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
    }

    /* Validate required capability windows against BAR lengths. */
    status = VirtioPciModernValidateCapInBar(Dev, &Dev->Caps.CommonCfg, sizeof(virtio_pci_common_cfg), "COMMON_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUnmapBars(Dev);
        return status;
    }

    /* Notify register writes are 16-bit MMIO. */
    status = VirtioPciModernValidateCapInBar(Dev, &Dev->Caps.NotifyCfg, sizeof(USHORT), "NOTIFY_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUnmapBars(Dev);
        return status;
    }

    status = VirtioPciModernValidateCapInBar(Dev, &Dev->Caps.IsrCfg, 1, "ISR_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUnmapBars(Dev);
        return status;
    }

    status = VirtioPciModernValidateCapInBar(Dev, &Dev->Caps.DeviceCfg, 1, "DEVICE_CFG");
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUnmapBars(Dev);
        return status;
    }

    /*
     * Validate every discovered virtio vendor capability against the
     * corresponding BAR resource length (defensive against malformed devices).
     */
    for (i = 0; i < Dev->Caps.AllCount; i++) {
        const VIRTIO_PCI_CAP_INFO *c;
        ULONGLONG end;

        c = &Dev->Caps.All[i];
        if (!c->Present) {
            continue;
        }

        if (c->Bar >= VIRTIO_PCI_MAX_BARS) {
            VIRTIO_CORE_PRINT("Virtio cap at 0x%02lx references invalid BAR %u\n",
                              (ULONG)c->CapOffset,
                              (UINT)c->Bar);
            VirtioPciModernUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (Dev->Bars[c->Bar].IsUpperHalf) {
            VIRTIO_CORE_PRINT("Virtio cap at 0x%02lx references upper-half BAR slot %u\n",
                              (ULONG)c->CapOffset,
                              (UINT)c->Bar);
            VirtioPciModernUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (!Dev->Bars[c->Bar].Present || !Dev->Bars[c->Bar].IsMemory || Dev->Bars[c->Bar].Length == 0) {
            VIRTIO_CORE_PRINT("Virtio cap at 0x%02lx references unmapped BAR %u\n",
                              (ULONG)c->CapOffset,
                              (UINT)c->Bar);
            VirtioPciModernUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        end = (ULONGLONG)c->Offset + (ULONGLONG)c->Length;
        if (end > (ULONGLONG)Dev->Bars[c->Bar].Length) {
            VIRTIO_CORE_PRINT(
                "Virtio cap at 0x%02lx overruns BAR%u (off=0x%lx len=0x%lx bar_len=0x%Ix)\n",
                (ULONG)c->CapOffset,
                (UINT)c->Bar,
                (ULONG)c->Offset,
                (ULONG)c->Length,
                Dev->Bars[c->Bar].Length);
            VirtioPciModernUnmapBars(Dev);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    /* Expose per-capability virtual addresses. */
    Dev->CommonCfg = (volatile virtio_pci_common_cfg *)((PUCHAR)Dev->Bars[Dev->Caps.CommonCfg.Bar].Va +
                                                        Dev->Caps.CommonCfg.Offset);
    Dev->NotifyBase = (volatile UCHAR *)((PUCHAR)Dev->Bars[Dev->Caps.NotifyCfg.Bar].Va +
                                         Dev->Caps.NotifyCfg.Offset);
    Dev->NotifyOffMultiplier = Dev->Caps.NotifyOffMultiplier;
    Dev->NotifyLength = (SIZE_T)Dev->Caps.NotifyCfg.Length;
    Dev->IsrStatus = (volatile UCHAR *)((PUCHAR)Dev->Bars[Dev->Caps.IsrCfg.Bar].Va + Dev->Caps.IsrCfg.Offset);
    Dev->DeviceCfg =
        (volatile UCHAR *)((PUCHAR)Dev->Bars[Dev->Caps.DeviceCfg.Bar].Va + Dev->Caps.DeviceCfg.Offset);

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernUninit(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    WDFSPINLOCK lockToDelete;

    if (Dev == NULL) {
        return;
    }

    VirtioPciModernUnmapBars(Dev);

    if (Dev->PciInterfaceAcquired && Dev->PciInterface.InterfaceDereference != NULL) {
        Dev->PciInterface.InterfaceDereference(Dev->PciInterface.Context);
        Dev->PciInterfaceAcquired = FALSE;
    }

    lockToDelete = Dev->CommonCfgLock;
    if (lockToDelete != NULL) {
        Dev->CommonCfgLock = NULL;
        WdfObjectDelete(lockToDelete);
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernDumpCaps(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev)
{
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    VIRTIO_CORE_PRINT("Virtio modern capabilities (%lu total):\n", Dev->Caps.AllCount);
    for (i = 0; i < Dev->Caps.AllCount; i++) {
        const VIRTIO_PCI_CAP_INFO *c;
        c = &Dev->Caps.All[i];
        VIRTIO_CORE_PRINT("  - cfg_type=%u (%s) bar=%u off=0x%lx len=0x%lx cap_off=0x%02lx cap_len=%u\n",
                          (UINT)c->CfgType,
                          VirtioPciCfgTypeToString(c->CfgType),
                          (UINT)c->Bar,
                          (ULONG)c->Offset,
                          (ULONG)c->Length,
                          (ULONG)c->CapOffset,
                          (UINT)c->CapLen);
    }

    VIRTIO_CORE_PRINT("Selected:\n");
    VIRTIO_CORE_PRINT("  COMMON_CFG: bar=%u off=0x%lx len=0x%lx va=%p\n",
                      (UINT)Dev->Caps.CommonCfg.Bar,
                      (ULONG)Dev->Caps.CommonCfg.Offset,
                      (ULONG)Dev->Caps.CommonCfg.Length,
                      Dev->CommonCfg);
    VIRTIO_CORE_PRINT("  NOTIFY_CFG: bar=%u off=0x%lx len=0x%lx va=%p mult=0x%lx\n",
                      (UINT)Dev->Caps.NotifyCfg.Bar,
                      (ULONG)Dev->Caps.NotifyCfg.Offset,
                      (ULONG)Dev->Caps.NotifyCfg.Length,
                      Dev->NotifyBase,
                      (ULONG)Dev->NotifyOffMultiplier);
    VIRTIO_CORE_PRINT("  ISR_CFG:    bar=%u off=0x%lx len=0x%lx va=%p\n",
                      (UINT)Dev->Caps.IsrCfg.Bar,
                      (ULONG)Dev->Caps.IsrCfg.Offset,
                      (ULONG)Dev->Caps.IsrCfg.Length,
                      Dev->IsrStatus);
    VIRTIO_CORE_PRINT("  DEVICE_CFG: bar=%u off=0x%lx len=0x%lx va=%p\n",
                      (UINT)Dev->Caps.DeviceCfg.Bar,
                      (ULONG)Dev->Caps.DeviceCfg.Offset,
                      (ULONG)Dev->Caps.DeviceCfg.Length,
                      Dev->DeviceCfg);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernDumpBars(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev)
{
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    VIRTIO_CORE_PRINT("PCI BARs:\n");
    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        const VIRTIO_PCI_BAR *b;
        b = &Dev->Bars[i];

        VIRTIO_CORE_PRINT(
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

/*
 * CommonCfg selector serialization helpers.
 *
 * Many CommonCfg fields depend on selector registers:
 *   - device_feature_select / driver_feature_select
 *   - queue_select
 *
 * These sequences must be serialized across threads.
 */

static __forceinline VOID
VirtioPciSelectQueueLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_select, QueueIndex);
    KeMemoryBarrier();
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgLock(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
#if DBG
    PKTHREAD currentThread;
#endif

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfgLock != NULL);
    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

#if DBG
    currentThread = KeGetCurrentThread();
    NT_ASSERT(Dev->CommonCfgLockOwner != currentThread);

    WdfSpinLockAcquire(Dev->CommonCfgLock);

    NT_ASSERT(Dev->CommonCfgLockOwner == NULL);
    Dev->CommonCfgLockOwner = currentThread;
#else
    WdfSpinLockAcquire(Dev->CommonCfgLock);
#endif
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgUnlock(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfgLock != NULL);
    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
    Dev->CommonCfgLockOwner = NULL;
#endif

    WdfSpinLockRelease(Dev->CommonCfgLock);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    UINT64 features;

    features = 0;

    VirtioPciCommonCfgLock(Dev);

    features = VirtioPciReadDeviceFeaturesLocked(Dev);

    VirtioPciCommonCfgUnlock(Dev);

    return features;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeaturesLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    ULONG lo;
    ULONG hi;

    lo = 0;
    hi = 0;

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

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
VOID
VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ UINT64 Features)
{
    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteDriverFeaturesLocked(Dev, Features);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteDriverFeaturesLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ UINT64 Features)
{
    ULONG lo;
    ULONG hi;

    lo = (ULONG)(Features & 0xFFFFFFFFui64);
    hi = (ULONG)(Features >> 32);

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

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
USHORT
VirtioPciReadQueueSize(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
    USHORT size;

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

    VirtioPciCommonCfgLock(Dev);

    size = VirtioPciReadQueueSizeLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);

    return size;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueSizeLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueMsixVector(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
    USHORT vector;

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

    VirtioPciCommonCfgLock(Dev);
    vector = VirtioPciReadQueueMsixVectorLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);

    return vector;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueMsixVectorLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_msix_vector);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueMsixVector(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                              _In_ USHORT QueueIndex,
                              _In_ USHORT Vector)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteQueueMsixVectorLocked(Dev, QueueIndex, Vector);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueMsixVectorLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                                    _In_ USHORT QueueIndex,
                                    _In_ USHORT Vector)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_msix_vector, Vector);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueNotifyOffset(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
    USHORT notifyOff;

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

    VirtioPciCommonCfgLock(Dev);

    notifyOff = VirtioPciReadQueueNotifyOffsetLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);

    return notifyOff;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueNotifyOffsetLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_notify_off);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueAddresses(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                             _In_ USHORT QueueIndex,
                             _In_ UINT64 Desc,
                             _In_ UINT64 Avail,
                              _In_ UINT64 Used)
{
    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteQueueAddressesLocked(Dev, QueueIndex, Desc, Avail, Used);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueAddressesLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                                   _In_ USHORT QueueIndex,
                                   _In_ UINT64 Desc,
                                   _In_ UINT64 Avail,
                                   _In_ UINT64 Used)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_lo, (ULONG)(Desc & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_hi, (ULONG)(Desc >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_lo, (ULONG)(Avail & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_hi, (ULONG)(Avail >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_lo, (ULONG)(Used & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_hi, (ULONG)(Used >> 32));
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueEnable(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                          _In_ USHORT QueueIndex,
                          _In_ BOOLEAN Enable)
{
    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteQueueEnableLocked(Dev, QueueIndex, Enable);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueEnableLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                                _In_ USHORT QueueIndex,
                                _In_ BOOLEAN Enable)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable, Enable ? 1 : 0);
}

static __forceinline UCHAR
VirtioPciReadDeviceStatus(_In_ const PVIRTIO_PCI_DEVICE Dev)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->device_status);
}

static __forceinline VOID
VirtioPciWriteDeviceStatus(_In_ const PVIRTIO_PCI_DEVICE Dev, _In_ UCHAR Status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->device_status, Status);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciResetDevice(_Inout_ PVIRTIO_PCI_DEVICE Dev)
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
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciAddStatus(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ UCHAR Bits)
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
VirtioPciGetStatus(_Inout_ PVIRTIO_PCI_DEVICE Dev)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    KeMemoryBarrier();
    return VirtioPciReadDeviceStatus(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciFailDevice(_Inout_ PVIRTIO_PCI_DEVICE Dev)
{
    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FAILED);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernResetDevice(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev)
{
    VirtioPciResetDevice(Dev);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ PVIRTIO_PCI_DEVICE Dev,
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
        VirtioPciFailDevice(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    negotiated = (deviceFeatures & Wanted) | Required;

    VIRTIO_CORE_PRINT("Virtio feature negotiation: device=0x%I64X required=0x%I64X wanted=0x%I64X negotiated=0x%I64X\n",
                      deviceFeatures,
                      Required,
                      Wanted,
                      negotiated);

    VirtioPciWriteDriverFeatures(Dev, negotiated);
    KeMemoryBarrier();

    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FEATURES_OK);

    status = VirtioPciGetStatus(Dev);
    if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
        VIRTIO_CORE_PRINT("Device rejected FEATURES_OK (status=0x%02X)\n", status);
        VirtioPciFailDevice(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    *NegotiatedOut = negotiated;

    //
    // Leave the device at FEATURES_OK for the transport smoke test.
    //
    return STATUS_SUCCESS;
}

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
VirtioPciReadDeviceConfig(_Inout_ PVIRTIO_PCI_DEVICE Dev,
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
VirtioPciWriteDeviceConfig(_Inout_ PVIRTIO_PCI_DEVICE Dev,
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

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciGetNumQueues(_In_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->num_queues);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueSize(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex, _Out_ USHORT *SizeOut)
{
    USHORT size;

    if (SizeOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *SizeOut = 0;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    VirtioPciCommonCfgLock(Dev);
    size = VirtioPciReadQueueSizeLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);

    if (size == 0) {
        return STATUS_NOT_FOUND;
    }

    *SizeOut = size;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciSetupQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                    _In_ USHORT QueueIndex,
                    _In_ ULONGLONG DescPa,
                    _In_ ULONGLONG AvailPa,
                    _In_ ULONGLONG UsedPa)
{
    NTSTATUS status;
    USHORT size;
    USHORT enabled;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = STATUS_SUCCESS;
    enabled = 0;

    VirtioPciCommonCfgLock(Dev);

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
    VirtioPciCommonCfgUnlock(Dev);
    return status;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDisableQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    VirtioPciCommonCfgLock(Dev);
    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable, 0);
    KeMemoryBarrier();
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                               _In_ USHORT QueueIndex,
                               _Out_ volatile UINT16 **NotifyAddrOut)
{
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

    VirtioPciCommonCfgLock(Dev);
    notifyOff = VirtioPciReadQueueNotifyOffsetLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);

    offset = (ULONGLONG)notifyOff * (ULONGLONG)Dev->NotifyOffMultiplier;
    if (offset + sizeof(UINT16) > Dev->NotifyLength) {
        return STATUS_IO_DEVICE_ERROR;
    }

    *NotifyAddrOut = (volatile UINT16 *)((volatile UCHAR *)Dev->NotifyBase + offset);
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciNotifyQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
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

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDumpQueueState(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    USHORT size;
    USHORT notifyOff;
    USHORT enable;
    UINT64 desc;
    UINT64 avail;
    UINT64 used;
    ULONGLONG notifyOffsetBytes;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    size = 0;
    notifyOff = 0;
    enable = 0;
    desc = 0;
    avail = 0;
    used = 0;
    notifyOffsetBytes = 0;

    VirtioPciCommonCfgLock(Dev);

    VirtioPciSelectQueueLocked(Dev, QueueIndex);

    size = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
    notifyOff = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_notify_off);
    enable = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable);

    desc = (UINT64)READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_lo) |
           ((UINT64)READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_hi) << 32);

    avail = (UINT64)READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_lo) |
            ((UINT64)READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_hi) << 32);

    used = (UINT64)READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_lo) |
           ((UINT64)READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_hi) << 32);

    VirtioPciCommonCfgUnlock(Dev);

    notifyOffsetBytes = (ULONGLONG)notifyOff * (ULONGLONG)Dev->NotifyOffMultiplier;

    VIRTIO_CORE_PRINT(
        "queue[%u]: size=%u enable=%u notify_off=%u (byte_off=0x%I64x) desc=0x%I64x avail=0x%I64x used=0x%I64x\n",
        QueueIndex,
        size,
        enable,
        notifyOff,
        notifyOffsetBytes,
        desc,
        avail,
        used);
}

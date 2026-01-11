/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtio_pci_modern_wdm.h"

#include "pci_interface.h"

#define AERO_VIRTIO_PCI_CONTRACT_REVISION_ID 0x01u
#define AERO_VIRTIO_PCI_VENDOR_ID 0x1AF4u
#define AERO_VIRTIO_PCI_DEVICE_ID_VIRTIO_SND 0x1059u

/* Aero contract v1 fixed BAR0 MMIO layout (docs/windows7-virtio-driver-contract.md). */
#define AERO_VIRTIO_PCI_BAR0_LEN     0x4000u
#define AERO_VIRTIO_PCI_COMMON_OFF  0x0000u
#define AERO_VIRTIO_PCI_COMMON_LEN  0x0100u
#define AERO_VIRTIO_PCI_NOTIFY_OFF  0x1000u
#define AERO_VIRTIO_PCI_NOTIFY_LEN  0x0100u
#define AERO_VIRTIO_PCI_ISR_OFF     0x2000u
#define AERO_VIRTIO_PCI_ISR_LEN     0x0020u
#define AERO_VIRTIO_PCI_DEVICE_OFF  0x3000u
#define AERO_VIRTIO_PCI_DEVICE_LEN  0x0100u

/* Feature bit masks required by Aero contract v1. */
#define AERO_VIRTIO_F_RING_INDIRECT_DESC (1ui64 << 28)

/* Bounded reset poll (virtio status reset handshake). */
#define VIRTIO_PCI_RESET_TIMEOUT_US    1000000u
#define VIRTIO_PCI_RESET_POLL_DELAY_US 1000u

/*
 * DEVICE_CFG reads should use config_generation to detect concurrent config
 * updates. Retry a small bounded number of times.
 */
#define VIRTIO_PCI_CONFIG_MAX_READ_RETRIES 10u

static ULONG
VirtIoSndReadLe32FromCfg(_In_reads_bytes_(256) const UCHAR *Cfg, _In_ ULONG Offset)
{
    ULONG v;

    v = 0;
    if (Offset + sizeof(v) > 256u) {
        return 0;
    }

    RtlCopyMemory(&v, Cfg + Offset, sizeof(v));
    return v;
}

static NTSTATUS
VirtIoSndTransportParseBars(_In_reads_bytes_(256) const UCHAR *Cfg,
                            _Out_writes_(VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT) uint64_t *BarAddrs,
                            _Out_writes_(VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT) BOOLEAN *BarIsMemory)
{
    ULONG i;

    if (Cfg == NULL || BarAddrs == NULL || BarIsMemory == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
        BarAddrs[i] = 0;
        BarIsMemory[i] = FALSE;
    }

    for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
        ULONG val;

        val = VirtIoSndReadLe32FromCfg(Cfg, 0x10u + (i * 4u));
        if (val == 0) {
            continue;
        }

        if ((val & 0x1u) != 0) {
            /* I/O BAR (unsupported for virtio-pci modern in the Aero contract). */
            BarAddrs[i] = (uint64_t)(val & ~0x3u);
            BarIsMemory[i] = FALSE;
            continue;
        }

        /* Memory BAR. */
        {
            ULONG memType;

            memType = (val >> 1) & 0x3u;
            BarIsMemory[i] = TRUE;

            if (memType == 0x2u) {
                /* 64-bit BAR uses this and the next BAR dword. */
                ULONG high;
                uint64_t base;

                if (i == (VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT - 1u)) {
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }

                high = VirtIoSndReadLe32FromCfg(Cfg, 0x10u + ((i + 1u) * 4u));
                base = ((uint64_t)high << 32) | (uint64_t)(val & ~0xFu);

                BarAddrs[i] = base;
                BarIsMemory[i] = TRUE;

                /* Upper half slot of a 64-bit BAR is not a separate BAR. */
                BarAddrs[i + 1u] = 0;
                BarIsMemory[i + 1u] = FALSE;

                i++;
            } else {
                BarAddrs[i] = (uint64_t)(val & ~0xFu);
            }
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndTransportValidateCaps(_In_ const virtio_pci_parsed_caps_t *Caps)
{
    if (Caps == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Caps->common_cfg.bar != 0 || Caps->notify_cfg.bar != 0 || Caps->isr_cfg.bar != 0 || Caps->device_cfg.bar != 0) {
        return STATUS_NOT_SUPPORTED;
    }

    if (Caps->notify_off_multiplier != 4u) {
        return STATUS_NOT_SUPPORTED;
    }

    /*
     * Aero contract v1 fixes the capability windows within BAR0. Be strict about
     * offsets (must match) but allow lengths to grow as long as they include the
     * contract minimum window.
     */
    if (Caps->common_cfg.offset != AERO_VIRTIO_PCI_COMMON_OFF || Caps->common_cfg.length < AERO_VIRTIO_PCI_COMMON_LEN) {
        return STATUS_NOT_SUPPORTED;
    }
    if (Caps->notify_cfg.offset != AERO_VIRTIO_PCI_NOTIFY_OFF || Caps->notify_cfg.length < AERO_VIRTIO_PCI_NOTIFY_LEN) {
        return STATUS_NOT_SUPPORTED;
    }
    if (Caps->isr_cfg.offset != AERO_VIRTIO_PCI_ISR_OFF || Caps->isr_cfg.length < AERO_VIRTIO_PCI_ISR_LEN) {
        return STATUS_NOT_SUPPORTED;
    }
    if (Caps->device_cfg.offset != AERO_VIRTIO_PCI_DEVICE_OFF || Caps->device_cfg.length < AERO_VIRTIO_PCI_DEVICE_LEN) {
        return STATUS_NOT_SUPPORTED;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndTransportFindBar0Resource(_In_ ULONGLONG Bar0Base,
                                  _In_ PCM_RESOURCE_LIST ResourcesRaw,
                                  _In_ PCM_RESOURCE_LIST ResourcesTranslated,
                                  _Out_ PHYSICAL_ADDRESS *RawStartOut,
                                  _Out_ PHYSICAL_ADDRESS *TranslatedStartOut,
                                  _Out_ SIZE_T *LengthOut)
{
    ULONG fullIndex;
    ULONG fullCount;

    if (ResourcesRaw == NULL || ResourcesTranslated == NULL || RawStartOut == NULL || TranslatedStartOut == NULL ||
        LengthOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RawStartOut->QuadPart = 0;
    TranslatedStartOut->QuadPart = 0;
    *LengthOut = 0;

    fullCount = ResourcesRaw->Count;
    if (ResourcesTranslated->Count < fullCount) {
        fullCount = ResourcesTranslated->Count;
    }

    for (fullIndex = 0; fullIndex < fullCount; fullIndex++) {
        PCM_FULL_RESOURCE_DESCRIPTOR rawFull;
        PCM_FULL_RESOURCE_DESCRIPTOR transFull;
        PCM_PARTIAL_RESOURCE_LIST rawList;
        PCM_PARTIAL_RESOURCE_LIST transList;
        ULONG descCount;
        ULONG descIndex;

        rawFull = &ResourcesRaw->List[fullIndex];
        transFull = &ResourcesTranslated->List[fullIndex];

        rawList = &rawFull->PartialResourceList;
        transList = &transFull->PartialResourceList;

        descCount = rawList->Count;
        if (transList->Count < descCount) {
            descCount = transList->Count;
        }

        for (descIndex = 0; descIndex < descCount; descIndex++) {
            PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
            PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
            ULONGLONG rawStart;
            SIZE_T len;

            rawDesc = &rawList->PartialDescriptors[descIndex];
            transDesc = &transList->PartialDescriptors[descIndex];

            if (rawDesc->Type != CmResourceTypeMemory || transDesc->Type != CmResourceTypeMemory) {
                continue;
            }

            rawStart = (ULONGLONG)rawDesc->u.Memory.Start.QuadPart;
            if (rawStart != Bar0Base) {
                continue;
            }

            len = (SIZE_T)rawDesc->u.Memory.Length;
            if (len == 0) {
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            *RawStartOut = rawDesc->u.Memory.Start;
            *TranslatedStartOut = transDesc->u.Memory.Start;
            *LengthOut = len;
            return STATUS_SUCCESS;
        }
    }

    return STATUS_DEVICE_CONFIGURATION_ERROR;
}

static NTSTATUS
VirtIoSndTransportValidateBar0Bounds(_In_ SIZE_T Bar0Length, _In_ ULONG Offset, _In_ ULONG Length)
{
    ULONGLONG end;

    end = (ULONGLONG)Offset + (ULONGLONG)Length;
    if (end < Offset) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (end > (ULONGLONG)Bar0Length) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    return STATUS_SUCCESS;
}

static __forceinline VOID
VirtIoSndCommonCfgLock(_Inout_ PVIRTIOSND_TRANSPORT Transport, _Out_ PKIRQL OldIrqlOut)
{
    KeAcquireSpinLock(&Transport->CommonCfgLock, OldIrqlOut);
}

static __forceinline VOID
VirtIoSndCommonCfgUnlock(_Inout_ PVIRTIOSND_TRANSPORT Transport, _In_ KIRQL OldIrql)
{
    KeReleaseSpinLock(&Transport->CommonCfgLock, OldIrql);
}

static __forceinline UCHAR
VirtIoSndReadDeviceStatus(_In_ const VIRTIOSND_TRANSPORT *Transport)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->device_status);
}

static __forceinline VOID
VirtIoSndWriteDeviceStatus(_In_ const VIRTIOSND_TRANSPORT *Transport, _In_ UCHAR Status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->device_status, Status);
}

static NTSTATUS
VirtIoSndTransportResetDevice(_Inout_ PVIRTIOSND_TRANSPORT Transport)
{
    ULONG waitedUs;

    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeMemoryBarrier();
    VirtIoSndWriteDeviceStatus(Transport, 0);
    KeMemoryBarrier();

    for (waitedUs = 0; waitedUs < VIRTIO_PCI_RESET_TIMEOUT_US; waitedUs += VIRTIO_PCI_RESET_POLL_DELAY_US) {
        if (VirtIoSndReadDeviceStatus(Transport) == 0) {
            KeMemoryBarrier();
            return STATUS_SUCCESS;
        }

        KeStallExecutionProcessor(VIRTIO_PCI_RESET_POLL_DELAY_US);
    }

    return STATUS_IO_TIMEOUT;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtIoSndTransportAddStatus(_Inout_ PVIRTIOSND_TRANSPORT Transport, _In_ UCHAR Bits)
{
    UCHAR status;

    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    status = VirtIoSndReadDeviceStatus(Transport);
    status |= Bits;
    VirtIoSndWriteDeviceStatus(Transport, status);
    KeMemoryBarrier();
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtIoSndTransportSetDriverOk(_Inout_ PVIRTIOSND_TRANSPORT Transport)
{
    VirtIoSndTransportAddStatus(Transport, VIRTIO_STATUS_DRIVER_OK);
}

static UCHAR
VirtIoSndTransportGetStatus(_Inout_ PVIRTIOSND_TRANSPORT Transport)
{
    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return 0;
    }

    KeMemoryBarrier();
    return VirtIoSndReadDeviceStatus(Transport);
}

static VOID
VirtIoSndTransportFailDevice(_Inout_ PVIRTIOSND_TRANSPORT Transport)
{
    VirtIoSndTransportAddStatus(Transport, VIRTIO_STATUS_FAILED);
}

static __forceinline VOID
VirtIoSndTransportSelectQueueLocked(_Inout_ PVIRTIOSND_TRANSPORT Transport, _In_ USHORT QueueIndex)
{
    WRITE_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_select, QueueIndex);
    KeMemoryBarrier();
}

static UINT64
VirtIoSndTransportReadDeviceFeaturesLocked(_Inout_ PVIRTIOSND_TRANSPORT Transport)
{
    ULONG lo;
    ULONG hi;

    lo = 0;
    hi = 0;

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->device_feature_select, 0);
    KeMemoryBarrier();
    lo = READ_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->device_feature);
    KeMemoryBarrier();

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->device_feature_select, 1);
    KeMemoryBarrier();
    hi = READ_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->device_feature);
    KeMemoryBarrier();

    return ((UINT64)hi << 32) | (UINT64)lo;
}

static VOID
VirtIoSndTransportWriteDriverFeaturesLocked(_Inout_ PVIRTIOSND_TRANSPORT Transport, _In_ UINT64 Features)
{
    ULONG lo;
    ULONG hi;

    lo = (ULONG)(Features & 0xFFFFFFFFui64);
    hi = (ULONG)(Features >> 32);

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->driver_feature_select, 0);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->driver_feature, lo);
    KeMemoryBarrier();

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->driver_feature_select, 1);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->driver_feature, hi);
    KeMemoryBarrier();
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtIoSndTransportInit(_Out_ PVIRTIOSND_TRANSPORT Transport,
                       _In_ PDEVICE_OBJECT LowerDeviceObject,
                       _In_ PCM_RESOURCE_LIST ResourcesRaw,
                       _In_ PCM_RESOURCE_LIST ResourcesTranslated)
{
    NTSTATUS status;
    UCHAR cfg[256];
    ULONG bytesRead;
    USHORT vendorId;
    USHORT deviceId;
    uint64_t bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    BOOLEAN bar_is_memory[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_cap_parse_result_t parseRes;
    virtio_pci_parsed_caps_t caps;
    ULONG bar0Reg;

    if (Transport == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Transport, sizeof(*Transport));
    Transport->LowerDeviceObject = LowerDeviceObject;
    KeInitializeSpinLock(&Transport->CommonCfgLock);

    if (LowerDeviceObject == NULL || ResourcesRaw == NULL || ResourcesTranslated == NULL) {
        status = STATUS_INVALID_PARAMETER;
        goto Fail;
    }

    status = VirtIoSndAcquirePciBusInterface(LowerDeviceObject, &Transport->PciInterface, &Transport->PciInterfaceAcquired);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    RtlZeroMemory(cfg, sizeof(cfg));
    bytesRead = VirtIoSndPciReadConfig(&Transport->PciInterface, cfg, 0, (ULONG)sizeof(cfg));
    if (bytesRead != (ULONG)sizeof(cfg)) {
        status = STATUS_DEVICE_DATA_ERROR;
        goto Fail;
    }

    vendorId = 0;
    deviceId = 0;
    RtlCopyMemory(&vendorId, cfg + 0x00u, sizeof(vendorId));
    RtlCopyMemory(&deviceId, cfg + 0x02u, sizeof(deviceId));

    if (vendorId != AERO_VIRTIO_PCI_VENDOR_ID || deviceId != AERO_VIRTIO_PCI_DEVICE_ID_VIRTIO_SND) {
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }

    Transport->PciRevisionId = cfg[0x08u];
    if (Transport->PciRevisionId != AERO_VIRTIO_PCI_CONTRACT_REVISION_ID) {
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }

    /*
     * Aero contract v1 exposes BAR0 as a 64-bit MMIO BAR (PCI memory type 64-bit).
     * Validate the BAR type before attempting to parse/match resources.
     */
    bar0Reg = VirtIoSndReadLe32FromCfg(cfg, 0x10u);
    if (bar0Reg == 0 || (bar0Reg & 0x1u) != 0) {
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }
    if (((bar0Reg >> 1) & 0x3u) != 0x2u) {
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }

    status = VirtIoSndTransportParseBars(cfg, bar_addrs, bar_is_memory);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    if (!bar_is_memory[0]) {
        status = STATUS_NOT_SUPPORTED;
        goto Fail;
    }

    Transport->Bar0Base = (ULONGLONG)bar_addrs[0];
    if (Transport->Bar0Base == 0) {
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Fail;
    }

    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), bar_addrs, &caps);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Fail;
    }

    status = VirtIoSndTransportValidateCaps(&caps);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    Transport->Caps = caps;
    Transport->NotifyOffMultiplier = (ULONG)caps.notify_off_multiplier;
    Transport->NotifyLength = (SIZE_T)caps.notify_cfg.length;

    status = VirtIoSndTransportFindBar0Resource(Transport->Bar0Base,
                                               ResourcesRaw,
                                               ResourcesTranslated,
                                               &Transport->Bar0RawStart,
                                               &Transport->Bar0TranslatedStart,
                                               &Transport->Bar0Length);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    if (Transport->Bar0Length != (SIZE_T)AERO_VIRTIO_PCI_BAR0_LEN) {
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Fail;
    }

    Transport->Bar0Va = MmMapIoSpace(Transport->Bar0TranslatedStart, Transport->Bar0Length, MmNonCached);
    if (Transport->Bar0Va == NULL) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Fail;
    }

    /* Validate every capability window against the BAR0 resource length. */
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, caps.common_cfg.offset, caps.common_cfg.length);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, caps.notify_cfg.offset, caps.notify_cfg.length);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, caps.isr_cfg.offset, caps.isr_cfg.length);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, caps.device_cfg.offset, caps.device_cfg.length);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    /* Ensure the contract minimum windows fit (defensive against mismatched CM resources). */
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, AERO_VIRTIO_PCI_COMMON_OFF, AERO_VIRTIO_PCI_COMMON_LEN);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, AERO_VIRTIO_PCI_NOTIFY_OFF, AERO_VIRTIO_PCI_NOTIFY_LEN);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, AERO_VIRTIO_PCI_ISR_OFF, AERO_VIRTIO_PCI_ISR_LEN);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }
    status = VirtIoSndTransportValidateBar0Bounds(Transport->Bar0Length, AERO_VIRTIO_PCI_DEVICE_OFF, AERO_VIRTIO_PCI_DEVICE_LEN);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    Transport->CommonCfg =
        (volatile virtio_pci_common_cfg *)((PUCHAR)Transport->Bar0Va + (ULONG_PTR)caps.common_cfg.offset);
    Transport->NotifyBase = (volatile UCHAR *)((PUCHAR)Transport->Bar0Va + (ULONG_PTR)caps.notify_cfg.offset);
    Transport->IsrStatus = (volatile UCHAR *)((PUCHAR)Transport->Bar0Va + (ULONG_PTR)caps.isr_cfg.offset);
    Transport->DeviceCfg = (volatile UCHAR *)((PUCHAR)Transport->Bar0Va + (ULONG_PTR)caps.device_cfg.offset);

    return STATUS_SUCCESS;

Fail:
    VirtIoSndTransportUninit(Transport);
    return status;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtIoSndTransportUninit(_Inout_ PVIRTIOSND_TRANSPORT Transport)
{
    if (Transport == NULL) {
        return;
    }

    if (Transport->Bar0Va != NULL) {
        MmUnmapIoSpace(Transport->Bar0Va, Transport->Bar0Length);
        Transport->Bar0Va = NULL;
    }

    Transport->CommonCfg = NULL;
    Transport->NotifyBase = NULL;
    Transport->IsrStatus = NULL;
    Transport->DeviceCfg = NULL;
    Transport->NotifyOffMultiplier = 0;
    Transport->NotifyLength = 0;

    VirtIoSndReleasePciBusInterface(&Transport->PciInterface, &Transport->PciInterfaceAcquired);
    Transport->LowerDeviceObject = NULL;

    Transport->Bar0Base = 0;
    Transport->Bar0RawStart.QuadPart = 0;
    Transport->Bar0TranslatedStart.QuadPart = 0;
    Transport->Bar0Length = 0;

    RtlZeroMemory(&Transport->Caps, sizeof(Transport->Caps));
    Transport->PciRevisionId = 0;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtIoSndTransportNegotiateFeatures(_Inout_ PVIRTIOSND_TRANSPORT Transport, _Out_ UINT64 *NegotiatedOut)
{
    NTSTATUS status;
    UINT64 deviceFeatures;
    UINT64 negotiated;
    UCHAR devStatus;
    KIRQL oldIrql;
    const UINT64 required = VIRTIO_F_VERSION_1 | AERO_VIRTIO_F_RING_INDIRECT_DESC;

    if (NegotiatedOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *NegotiatedOut = 0;

    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtIoSndTransportResetDevice(Transport);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    VirtIoSndTransportAddStatus(Transport, VIRTIO_STATUS_ACKNOWLEDGE);
    VirtIoSndTransportAddStatus(Transport, VIRTIO_STATUS_DRIVER);

    VirtIoSndCommonCfgLock(Transport, &oldIrql);
    deviceFeatures = VirtIoSndTransportReadDeviceFeaturesLocked(Transport);
    VirtIoSndCommonCfgUnlock(Transport, oldIrql);

    if ((deviceFeatures & required) != required) {
        VirtIoSndTransportFailDevice(Transport);
        return STATUS_NOT_SUPPORTED;
    }

    negotiated = required;

    VirtIoSndCommonCfgLock(Transport, &oldIrql);
    VirtIoSndTransportWriteDriverFeaturesLocked(Transport, negotiated);
    VirtIoSndCommonCfgUnlock(Transport, oldIrql);

    KeMemoryBarrier();
    VirtIoSndTransportAddStatus(Transport, VIRTIO_STATUS_FEATURES_OK);

    devStatus = VirtIoSndTransportGetStatus(Transport);
    if ((devStatus & VIRTIO_STATUS_FEATURES_OK) == 0) {
        VirtIoSndTransportFailDevice(Transport);
        return STATUS_NOT_SUPPORTED;
    }

    *NegotiatedOut = negotiated;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportReadQueueSize(_Inout_ PVIRTIOSND_TRANSPORT Transport, _In_ USHORT QueueIndex, _Out_ USHORT *SizeOut)
{
    KIRQL oldIrql;
    USHORT size;

    if (SizeOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *SizeOut = 0;

    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    VirtIoSndCommonCfgLock(Transport, &oldIrql);
    VirtIoSndTransportSelectQueueLocked(Transport, QueueIndex);
    size = READ_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_size);
    VirtIoSndCommonCfgUnlock(Transport, oldIrql);

    if (size == 0) {
        return STATUS_NOT_FOUND;
    }

    *SizeOut = size;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportReadQueueNotifyOff(_Inout_ PVIRTIOSND_TRANSPORT Transport,
                                     _In_ USHORT QueueIndex,
                                     _Out_ USHORT *NotifyOffOut)
{
    KIRQL oldIrql;
    USHORT size;
    USHORT notifyOff;

    if (NotifyOffOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *NotifyOffOut = 0;

    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    VirtIoSndCommonCfgLock(Transport, &oldIrql);
    VirtIoSndTransportSelectQueueLocked(Transport, QueueIndex);
    size = READ_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_size);
    notifyOff = READ_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_notify_off);
    VirtIoSndCommonCfgUnlock(Transport, oldIrql);

    if (size == 0) {
        return STATUS_NOT_FOUND;
    }

    *NotifyOffOut = notifyOff;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportSetupQueue(_Inout_ PVIRTIOSND_TRANSPORT Transport,
                             _In_ USHORT QueueIndex,
                             _In_ UINT64 QueueDescPa,
                             _In_ UINT64 QueueAvailPa,
                             _In_ UINT64 QueueUsedPa,
                             _Out_opt_ USHORT *NotifyOffOut)
{
    NTSTATUS status;
    KIRQL oldIrql;
    USHORT size;
    USHORT notifyOff;
    USHORT enabled;

    if (NotifyOffOut != NULL) {
        *NotifyOffOut = 0;
    }

    if (Transport == NULL || Transport->CommonCfg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = STATUS_SUCCESS;
    size = 0;
    notifyOff = 0;
    enabled = 0;

    VirtIoSndCommonCfgLock(Transport, &oldIrql);

    VirtIoSndTransportSelectQueueLocked(Transport, QueueIndex);

    size = READ_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_size);
    if (size == 0) {
        status = STATUS_NOT_FOUND;
        goto Exit;
    }

    notifyOff = READ_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_notify_off);

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->queue_desc_lo, (ULONG)(QueueDescPa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->queue_desc_hi, (ULONG)(QueueDescPa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->queue_avail_lo, (ULONG)(QueueAvailPa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->queue_avail_hi, (ULONG)(QueueAvailPa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->queue_used_lo, (ULONG)(QueueUsedPa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Transport->CommonCfg->queue_used_hi, (ULONG)(QueueUsedPa >> 32));

    /*
     * The device must observe the ring addresses before queue_enable is set.
     */
    KeMemoryBarrier();

    WRITE_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_enable, 1);

    /* Readback confirmation. */
    enabled = READ_REGISTER_USHORT((volatile USHORT *)&Transport->CommonCfg->queue_enable);
    if (enabled != 1) {
        status = STATUS_IO_DEVICE_ERROR;
        goto Exit;
    }

Exit:
    VirtIoSndCommonCfgUnlock(Transport, oldIrql);

    if (NT_SUCCESS(status) && NotifyOffOut != NULL) {
        *NotifyOffOut = notifyOff;
    }

    return status;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
volatile UINT16 *
VirtIoSndTransportComputeNotifyAddr(_In_ const VIRTIOSND_TRANSPORT *Transport, _In_ USHORT QueueNotifyOff)
{
    ULONGLONG offset;

    if (Transport == NULL || Transport->NotifyBase == NULL || Transport->NotifyOffMultiplier == 0 ||
        Transport->NotifyLength < sizeof(UINT16)) {
        return NULL;
    }

    offset = (ULONGLONG)QueueNotifyOff * (ULONGLONG)Transport->NotifyOffMultiplier;
    if (offset + sizeof(UINT16) > (ULONGLONG)Transport->NotifyLength) {
        return NULL;
    }

    return (volatile UINT16 *)((volatile UCHAR *)Transport->NotifyBase + offset);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtIoSndTransportNotifyQueue(_In_ const VIRTIOSND_TRANSPORT *Transport,
                              _In_ USHORT QueueIndex,
                              _In_ USHORT QueueNotifyOff)
{
    volatile UINT16 *addr;

    addr = VirtIoSndTransportComputeNotifyAddr(Transport, QueueNotifyOff);
    if (addr == NULL) {
        return;
    }

    WRITE_REGISTER_USHORT((volatile USHORT *)addr, QueueIndex);
    KeMemoryBarrier();
}

static __forceinline UCHAR
VirtIoSndReadDeviceConfig8(_In_ volatile const UCHAR *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)((ULONG_PTR)Base + Offset));
}

static __forceinline VOID
VirtIoSndWriteDeviceConfig8(_In_ volatile UCHAR *Base, _In_ ULONG Offset, _In_ UCHAR Value)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)((ULONG_PTR)Base + Offset), Value);
}

static VOID
VirtIoSndCopyFromDeviceCfg(_In_ volatile const UCHAR *Base,
                           _In_ ULONG Offset,
                           _Out_writes_bytes_(Length) UCHAR *OutBytes,
                           _In_ ULONG Length)
{
    ULONG i;

    for (i = 0; i < Length; i++) {
        OutBytes[i] = VirtIoSndReadDeviceConfig8(Base, Offset + i);
    }
}

static VOID
VirtIoSndCopyToDeviceCfg(_In_ volatile UCHAR *Base,
                         _In_ ULONG Offset,
                         _In_reads_bytes_(Length) const UCHAR *InBytes,
                         _In_ ULONG Length)
{
    ULONG i;

    for (i = 0; i < Length; i++) {
        VirtIoSndWriteDeviceConfig8(Base, Offset + i, InBytes[i]);
    }
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportReadDeviceConfig(_Inout_ PVIRTIOSND_TRANSPORT Transport,
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

    if (Transport == NULL || Transport->CommonCfg == NULL || Transport->DeviceCfg == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    end = (ULONGLONG)Offset + (ULONGLONG)Length;
    if (end < Offset) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Transport->Caps.device_cfg.length != 0 && end > (ULONGLONG)Transport->Caps.device_cfg.length) {
        return STATUS_INVALID_PARAMETER;
    }

    outBytes = (PUCHAR)Buffer;

    for (attempt = 0; attempt < VIRTIO_PCI_CONFIG_MAX_READ_RETRIES; attempt++) {
        gen0 = READ_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->config_generation);
        KeMemoryBarrier();

        VirtIoSndCopyFromDeviceCfg(Transport->DeviceCfg, Offset, outBytes, Length);

        KeMemoryBarrier();
        gen1 = READ_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->config_generation);
        KeMemoryBarrier();

        if (gen0 == gen1) {
            return STATUS_SUCCESS;
        }
    }

    return STATUS_IO_TIMEOUT;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportWriteDeviceConfig(_Inout_ PVIRTIOSND_TRANSPORT Transport,
                                    _In_ ULONG Offset,
                                    _In_reads_bytes_(Length) const VOID *Buffer,
                                    _In_ ULONG Length)
{
    const UCHAR *inBytes;
    ULONGLONG end;

    if (Length == 0) {
        return STATUS_SUCCESS;
    }

    if (Transport == NULL || Transport->DeviceCfg == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    end = (ULONGLONG)Offset + (ULONGLONG)Length;
    if (end < Offset) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Transport->Caps.device_cfg.length != 0 && end > (ULONGLONG)Transport->Caps.device_cfg.length) {
        return STATUS_INVALID_PARAMETER;
    }

    inBytes = (const UCHAR *)Buffer;
    VirtIoSndCopyToDeviceCfg(Transport->DeviceCfg, Offset, inBytes, Length);
    KeMemoryBarrier();
    return STATUS_SUCCESS;
}

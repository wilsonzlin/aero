/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_modern_miniport.h"

#include "../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.h"

#define VIRTIO_PCI_RESET_TIMEOUT_US        1000000u
#define VIRTIO_PCI_RESET_POLL_DELAY_US     1000u
#define VIRTIO_PCI_RESET_HIGH_IRQL_POLL_DELAY_US 100u
#define VIRTIO_PCI_RESET_HIGH_IRQL_TIMEOUT_US 10000u
#define VIRTIO_PCI_CONFIG_MAX_READ_RETRIES 10u

static __forceinline ULONG
VirtioPciReadLe32(_In_reads_bytes_(Offset + sizeof(ULONG)) const UCHAR *Bytes, _In_ ULONG Offset)
{
    return (ULONG)Bytes[Offset + 0] | ((ULONG)Bytes[Offset + 1] << 8) | ((ULONG)Bytes[Offset + 2] << 16) |
           ((ULONG)Bytes[Offset + 3] << 24);
}

static NTSTATUS
VirtioPciParseBarsFromConfig(_In_reads_bytes_(CfgLen) const UCHAR *Cfg,
                             _In_ ULONG CfgLen,
                             _Out_writes_(VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT) UINT64 BarAddrs[])
{
    ULONG i;

    if (Cfg == NULL || BarAddrs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (CfgLen < 0x10 + (VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT * sizeof(ULONG))) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlZeroMemory(BarAddrs, VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT * sizeof(BarAddrs[0]));

    for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
        ULONG off;
        ULONG val;

        off = 0x10 + (i * sizeof(ULONG));
        val = VirtioPciReadLe32(Cfg, off);
        if (val == 0) {
            continue;
        }

        if ((val & 0x1u) != 0) {
            /* I/O BAR. Not expected for virtio modern but parse defensively. */
            BarAddrs[i] = (UINT64)(val & ~0x3u);
            continue;
        }

        /* Memory BAR. */
        {
            ULONG memType;
            memType = (val >> 1) & 0x3u;

            if (memType == 0x2u) {
                ULONG high;
                UINT64 base;

                if (i == (VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT - 1)) {
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }

                high = VirtioPciReadLe32(Cfg, off + sizeof(ULONG));
                base = ((UINT64)high << 32) | (UINT64)(val & ~0xFu);
                BarAddrs[i] = base;

                /* Skip the high dword slot (upper-half BAR entry). */
                i++;
            } else {
                BarAddrs[i] = (UINT64)(val & ~0xFu);
            }
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtioPciValidateCapInBar0(_In_ const VIRTIO_PCI_DEVICE *Dev,
                           _In_ const virtio_pci_cap_region_t *Cap,
                           _In_ ULONG RequiredMinLength)
{
    ULONGLONG end;

    if (Dev == NULL || Cap == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Cap->bar != 0) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (Cap->length < RequiredMinLength) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    end = (ULONGLONG)Cap->offset + (ULONGLONG)Cap->length;
    if (end < Cap->offset || end > Dev->Bar0Length) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    return STATUS_SUCCESS;
}

static __forceinline VOID
VirtioPciCommonCfgLock(_Inout_ VIRTIO_PCI_DEVICE *Dev, _Out_ KIRQL *OldIrql)
{
    KeAcquireSpinLock(&Dev->CommonCfgLock, OldIrql);
}

static __forceinline VOID
VirtioPciCommonCfgUnlock(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ KIRQL OldIrql)
{
    KeReleaseSpinLock(&Dev->CommonCfgLock, OldIrql);
}

static __forceinline VOID
VirtioPciSelectQueueLocked(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_select, QueueIndex);
    KeMemoryBarrier();
}

NTSTATUS
VirtioPciModernMiniportInit(_Out_ VIRTIO_PCI_DEVICE *Dev,
                            _In_ PUCHAR Bar0Va,
                            _In_ ULONG Bar0Length,
                            _In_reads_bytes_(PciCfgLength) const UCHAR *PciCfg,
                            _In_ ULONG PciCfgLength)
{
    UINT64 barAddrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t parseRes;
    NTSTATUS status;

    if (Dev == NULL || Bar0Va == NULL || Bar0Length == 0 || PciCfg == NULL || PciCfgLength == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
    Dev->Bar0Va = Bar0Va;
    Dev->Bar0Length = Bar0Length;
    KeInitializeSpinLock(&Dev->CommonCfgLock);

    status = VirtioPciParseBarsFromConfig(PciCfg, PciCfgLength, barAddrs);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(&caps, sizeof(caps));
    parseRes = virtio_pci_cap_parse((const uint8_t *)PciCfg, (size_t)PciCfgLength, barAddrs, &caps);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    Dev->NotifyOffMultiplier = (ULONG)caps.notify_off_multiplier;
    if (Dev->NotifyOffMultiplier == 0) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    status = VirtioPciValidateCapInBar0(Dev, &caps.common_cfg, sizeof(virtio_pci_common_cfg));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioPciValidateCapInBar0(Dev, &caps.notify_cfg, sizeof(UINT16));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioPciValidateCapInBar0(Dev, &caps.isr_cfg, 1);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioPciValidateCapInBar0(Dev, &caps.device_cfg, 1);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    Dev->CommonCfgOffset = (ULONG)caps.common_cfg.offset;
    Dev->CommonCfgLength = (ULONG)caps.common_cfg.length;
    Dev->CommonCfg = (volatile virtio_pci_common_cfg *)(Dev->Bar0Va + Dev->CommonCfgOffset);

    Dev->NotifyOffset = (ULONG)caps.notify_cfg.offset;
    Dev->NotifyLength = (ULONG)caps.notify_cfg.length;
    Dev->NotifyBase = (volatile UCHAR *)(Dev->Bar0Va + Dev->NotifyOffset);

    Dev->IsrOffset = (ULONG)caps.isr_cfg.offset;
    Dev->IsrLength = (ULONG)caps.isr_cfg.length;
    Dev->IsrStatus = (volatile UCHAR *)(Dev->Bar0Va + Dev->IsrOffset);

    Dev->DeviceCfgOffset = (ULONG)caps.device_cfg.offset;
    Dev->DeviceCfgLength = (ULONG)caps.device_cfg.length;
    Dev->DeviceCfg = (volatile UCHAR *)(Dev->Bar0Va + Dev->DeviceCfgOffset);

    return STATUS_SUCCESS;
}

static __forceinline UCHAR
VirtioPciReadDeviceStatus(_In_ const VIRTIO_PCI_DEVICE *Dev)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->device_status);
}

static __forceinline VOID
VirtioPciWriteDeviceStatus(_In_ const VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&Dev->CommonCfg->device_status, Status);
}

VOID
VirtioPciResetDevice(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    KIRQL irql;
    ULONG waitedUs;
    UCHAR status;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    VirtioPciWriteDeviceStatus(Dev, 0);
    KeMemoryBarrier();

    /*
     * Immediate read-back to both flush the MMIO write and fast-path the common
     * case where the device resets synchronously.
     */
    status = VirtioPciReadDeviceStatus(Dev);
    if (status == 0) {
        KeMemoryBarrier();
        return;
    }

    /*
     * Some callers (e.g. StorPort reset paths) may invoke reset at DISPATCH/DIRQL.
     * A 1-second busy-wait at elevated IRQL is unacceptable: it can severely
     * impact system responsiveness. Be IRQL-aware:
     *
     * - PASSIVE_LEVEL: sleep/yield in small increments up to 1s total.
     * - > PASSIVE_LEVEL: only busy-wait for a small budget (<=10ms), then give up.
     */
    irql = KeGetCurrentIrql();

    if (irql == PASSIVE_LEVEL) {
        const ULONGLONG timeout100ns = (ULONGLONG)VIRTIO_PCI_RESET_TIMEOUT_US * 10ull;
        const ULONGLONG pollDelay100ns = (ULONGLONG)VIRTIO_PCI_RESET_POLL_DELAY_US * 10ull;
        const ULONGLONG start100ns = KeQueryInterruptTime();
        const ULONGLONG deadline100ns = start100ns + timeout100ns;

        for (;;) {
            ULONGLONG now100ns;
            ULONGLONG remaining100ns;
            LARGE_INTEGER delay;

            status = VirtioPciReadDeviceStatus(Dev);
            if (status == 0) {
                KeMemoryBarrier();
                return;
            }

            now100ns = KeQueryInterruptTime();
            if (now100ns >= deadline100ns) {
                break;
            }

            remaining100ns = deadline100ns - now100ns;
            if (remaining100ns > pollDelay100ns) {
                remaining100ns = pollDelay100ns;
            }

            delay.QuadPart = -((LONGLONG)remaining100ns);
            (void)KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }

        DbgPrintEx(DPFLTR_IHVDRIVER_ID,
                   DPFLTR_ERROR_LEVEL,
                   "[aero-virtio] VirtioPciResetDevice: device_status did not clear within %lu us (IRQL=%lu), last=%lu\n",
                   (ULONG)VIRTIO_PCI_RESET_TIMEOUT_US,
                   (ULONG)irql,
                   (ULONG)status);
        return;
    }

    for (waitedUs = 0; waitedUs < VIRTIO_PCI_RESET_HIGH_IRQL_TIMEOUT_US;
         waitedUs += VIRTIO_PCI_RESET_HIGH_IRQL_POLL_DELAY_US) {
        KeStallExecutionProcessor(VIRTIO_PCI_RESET_HIGH_IRQL_POLL_DELAY_US);

        status = VirtioPciReadDeviceStatus(Dev);
        if (status == 0) {
            KeMemoryBarrier();
            return;
        }
    }

    DbgPrintEx(DPFLTR_IHVDRIVER_ID,
               DPFLTR_ERROR_LEVEL,
               "[aero-virtio] VirtioPciResetDevice: device_status did not clear within %lu us at IRQL=%lu, last=%lu\n",
               (ULONG)VIRTIO_PCI_RESET_HIGH_IRQL_TIMEOUT_US,
               (ULONG)irql,
               (ULONG)status);
}

VOID
VirtioPciAddStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Bits)
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

UCHAR
VirtioPciGetStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    KeMemoryBarrier();
    return VirtioPciReadDeviceStatus(Dev);
}

VOID
VirtioPciSetStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Status)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    VirtioPciWriteDeviceStatus(Dev, Status);
    KeMemoryBarrier();
}

VOID
VirtioPciFailDevice(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FAILED);
}

UINT64
VirtioPciReadDeviceFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    KIRQL oldIrql;
    ULONG lo;
    ULONG hi;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    lo = 0;
    hi = 0;

    VirtioPciCommonCfgLock(Dev, &oldIrql);

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature_select, 0);
    KeMemoryBarrier();
    lo = READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature);
    KeMemoryBarrier();

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature_select, 1);
    KeMemoryBarrier();
    hi = READ_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->device_feature);
    KeMemoryBarrier();

    VirtioPciCommonCfgUnlock(Dev, oldIrql);

    return ((UINT64)hi << 32) | lo;
}

VOID
VirtioPciWriteDriverFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UINT64 Features)
{
    KIRQL oldIrql;
    ULONG lo;
    ULONG hi;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    lo = (ULONG)(Features & 0xFFFFFFFFULL);
    hi = (ULONG)(Features >> 32);

    VirtioPciCommonCfgLock(Dev, &oldIrql);

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature_select, 0);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature, lo);
    KeMemoryBarrier();

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature_select, 1);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->driver_feature, hi);
    KeMemoryBarrier();

    VirtioPciCommonCfgUnlock(Dev, oldIrql);
}

NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev,
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

    VirtioPciWriteDriverFeatures(Dev, negotiated);
    KeMemoryBarrier();

    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FEATURES_OK);

    status = VirtioPciGetStatus(Dev);
    if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
        VirtioPciFailDevice(Dev);
        return STATUS_NOT_SUPPORTED;
    }

    *NegotiatedOut = negotiated;
    return STATUS_SUCCESS;
}

static __forceinline UCHAR
VirtioPciReadCfg8(_In_ volatile const VOID *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)((ULONG_PTR)Base + Offset));
}

static __forceinline USHORT
VirtioPciReadCfg16(_In_ volatile const VOID *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_USHORT((volatile USHORT *)((ULONG_PTR)Base + Offset));
}

static __forceinline ULONG
VirtioPciReadCfg32(_In_ volatile const VOID *Base, _In_ ULONG Offset)
{
    return READ_REGISTER_ULONG((volatile ULONG *)((ULONG_PTR)Base + Offset));
}

static VOID
VirtioPciCopyFromDevice(_In_ volatile const UCHAR *Base,
                        _In_ ULONG Offset,
                        _Out_writes_bytes_(Length) UCHAR *OutBytes,
                        _In_ ULONG Length)
{
    ULONG i;

    i = 0;

    while (i < Length && ((Offset + i) & 3u) != 0) {
        OutBytes[i] = VirtioPciReadCfg8(Base, Offset + i);
        i++;
    }

    while (Length - i >= sizeof(ULONG)) {
        ULONG v32;
        v32 = VirtioPciReadCfg32(Base, Offset + i);
        RtlCopyMemory(OutBytes + i, &v32, sizeof(v32));
        i += sizeof(ULONG);
    }

    while (Length - i >= sizeof(USHORT)) {
        USHORT v16;
        v16 = VirtioPciReadCfg16(Base, Offset + i);
        RtlCopyMemory(OutBytes + i, &v16, sizeof(v16));
        i += sizeof(USHORT);
    }

    while (i < Length) {
        OutBytes[i] = VirtioPciReadCfg8(Base, Offset + i);
        i++;
    }
}

NTSTATUS
VirtioPciReadDeviceConfig(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                          _In_ ULONG Offset,
                          _Out_writes_bytes_(Length) VOID *Buffer,
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

    if (Dev->DeviceCfgLength != 0 && end > Dev->DeviceCfgLength) {
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

USHORT
VirtioPciGetNumQueues(_In_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    return READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->num_queues);
}

USHORT
VirtioPciGetQueueSize(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    KIRQL oldIrql;
    USHORT size;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return 0;
    }

    size = 0;

    VirtioPciCommonCfgLock(Dev, &oldIrql);
    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    size = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
    VirtioPciCommonCfgUnlock(Dev, oldIrql);

    return size;
}

NTSTATUS
VirtioPciSetupQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                    _In_ USHORT QueueIndex,
                    _In_ UINT64 DescPa,
                    _In_ UINT64 AvailPa,
                    _In_ UINT64 UsedPa)
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

    VirtioPciCommonCfgLock(Dev, &oldIrql);

    VirtioPciSelectQueueLocked(Dev, QueueIndex);

    size = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
    if (size == 0) {
        status = STATUS_NOT_FOUND;
        goto Exit;
    }

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_lo, (ULONG)(DescPa & 0xFFFFFFFFULL));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_desc_hi, (ULONG)(DescPa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_lo, (ULONG)(AvailPa & 0xFFFFFFFFULL));
    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_avail_hi, (ULONG)(AvailPa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&Dev->CommonCfg->queue_used_lo, (ULONG)(UsedPa & 0xFFFFFFFFULL));
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
    VirtioPciCommonCfgUnlock(Dev, oldIrql);
    return status;
}

VOID
VirtioPciDisableQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    KIRQL oldIrql;

    if (Dev == NULL || Dev->CommonCfg == NULL) {
        return;
    }

    VirtioPciCommonCfgLock(Dev, &oldIrql);

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    WRITE_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_enable, 0);
    KeMemoryBarrier();

    VirtioPciCommonCfgUnlock(Dev, oldIrql);
}

NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ VIRTIO_PCI_DEVICE *Dev,
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

    VirtioPciCommonCfgLock(Dev, &oldIrql);

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    size = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_size);
    notifyOff = READ_REGISTER_USHORT((volatile USHORT *)&Dev->CommonCfg->queue_notify_off);

    VirtioPciCommonCfgUnlock(Dev, oldIrql);

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

    /*
     * Ensure all prior ring writes are visible before writing the notify doorbell.
     * See docs/virtio/virtqueue-split-ring-win7.md for the publish/notify ordering.
     */
    KeMemoryBarrier();
    WRITE_REGISTER_USHORT((volatile USHORT *)notifyAddr, QueueIndex);
    KeMemoryBarrier();
}

UCHAR
VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL || Dev->IsrStatus == NULL) {
        return 0;
    }

    return READ_REGISTER_UCHAR((volatile UCHAR *)Dev->IsrStatus);
}

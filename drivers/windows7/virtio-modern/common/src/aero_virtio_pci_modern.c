/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "aero_virtio_pci_modern.h"

#if !AERO_VIRTIO_PCI_MODERN_KERNEL_MODE
#include <string.h>
#endif

/* -------------------------------------------------------------------------- */
/* Internal helpers                                                           */
/* -------------------------------------------------------------------------- */

#define AERO_VIRTIO_RESET_TIMEOUT_US 1000000u
#define AERO_VIRTIO_RESET_POLL_DELAY_US 1000u
#define AERO_VIRTIO_CONFIG_MAX_READ_RETRIES 10u

/*
 * MMIO accessors:
 *   - In kernel mode we use READ_REGISTER_* / WRITE_REGISTER_*.
 *   - In host unit tests we can build the library with
 *     AERO_VIRTIO_PCI_MODERN_USE_TEST_MMIO to route reads/writes to an emulated
 *     device model.
 */

#if defined(AERO_VIRTIO_PCI_MODERN_USE_TEST_MMIO)

extern UCHAR AeroVirtioPciModernTestRead8(const volatile void *Addr);
extern USHORT AeroVirtioPciModernTestRead16(const volatile void *Addr);
extern ULONG AeroVirtioPciModernTestRead32(const volatile void *Addr);
extern void AeroVirtioPciModernTestWrite8(volatile void *Addr, UCHAR Value);
extern void AeroVirtioPciModernTestWrite16(volatile void *Addr, USHORT Value);
extern void AeroVirtioPciModernTestWrite32(volatile void *Addr, ULONG Value);
extern void AeroVirtioPciModernTestBarrier(void);
extern void AeroVirtioPciModernTestStallExecutionProcessor(ULONG Microseconds);

#define AV_READ8(addr) AeroVirtioPciModernTestRead8((addr))
#define AV_READ16(addr) AeroVirtioPciModernTestRead16((addr))
#define AV_READ32(addr) AeroVirtioPciModernTestRead32((addr))
#define AV_WRITE8(addr, v) AeroVirtioPciModernTestWrite8((addr), (v))
#define AV_WRITE16(addr, v) AeroVirtioPciModernTestWrite16((addr), (v))
#define AV_WRITE32(addr, v) AeroVirtioPciModernTestWrite32((addr), (v))
#define AV_BARRIER() AeroVirtioPciModernTestBarrier()
#define AV_STALL(us) AeroVirtioPciModernTestStallExecutionProcessor((us))

#elif AERO_VIRTIO_PCI_MODERN_KERNEL_MODE

#define AV_READ8(addr) READ_REGISTER_UCHAR((volatile UCHAR *)(addr))
#define AV_READ16(addr) READ_REGISTER_USHORT((volatile USHORT *)(addr))
#define AV_READ32(addr) READ_REGISTER_ULONG((volatile ULONG *)(addr))
#define AV_WRITE8(addr, v) WRITE_REGISTER_UCHAR((volatile UCHAR *)(addr), (v))
#define AV_WRITE16(addr, v) WRITE_REGISTER_USHORT((volatile USHORT *)(addr), (v))
#define AV_WRITE32(addr, v) WRITE_REGISTER_ULONG((volatile ULONG *)(addr), (v))
#define AV_BARRIER() KeMemoryBarrier()
#define AV_STALL(us) KeStallExecutionProcessor((us))

#else

#if defined(__GNUC__) || defined(__clang__)
#define AV_BARRIER() __sync_synchronize()
#else
#define AV_BARRIER() ((void)0)
#endif

#define AV_READ8(addr) (*(volatile const UCHAR *)(addr))
#define AV_READ16(addr) (*(volatile const USHORT *)(addr))
#define AV_READ32(addr) (*(volatile const ULONG *)(addr))
#define AV_WRITE8(addr, v) (*(volatile UCHAR *)(addr) = (v))
#define AV_WRITE16(addr, v) (*(volatile USHORT *)(addr) = (v))
#define AV_WRITE32(addr, v) (*(volatile ULONG *)(addr) = (v))
#define AV_STALL(us) ((void)(us))

#endif

#if AERO_VIRTIO_PCI_MODERN_KERNEL_MODE
#define AV_MEMCPY(dst, src, len) RtlCopyMemory((dst), (src), (len))
#define AV_MEMZERO(dst, len) RtlZeroMemory((dst), (len))
#else
#define AV_MEMCPY(dst, src, len) memcpy((dst), (src), (len))
#define AV_MEMZERO(dst, len) memset((dst), 0, (len))
#endif

static __inline UCHAR av_read_cfg8(volatile const UCHAR *base, ULONG offset)
{
    return AV_READ8((volatile const void *)(base + offset));
}

static __inline USHORT av_read_cfg16(volatile const UCHAR *base, ULONG offset)
{
    return AV_READ16((volatile const void *)(base + offset));
}

static __inline ULONG av_read_cfg32(volatile const UCHAR *base, ULONG offset)
{
    return AV_READ32((volatile const void *)(base + offset));
}

static void av_copy_from_device(volatile const UCHAR *base, ULONG offset, UCHAR *out_bytes, ULONG length)
{
    ULONG i = 0;

    while (i < length && ((offset + i) & 3u) != 0) {
        out_bytes[i] = av_read_cfg8(base, offset + i);
        i++;
    }

    while (length - i >= sizeof(ULONG)) {
        ULONG v32 = av_read_cfg32(base, offset + i);
        AV_MEMCPY(out_bytes + i, &v32, sizeof(v32));
        i += sizeof(ULONG);
    }

    while (length - i >= sizeof(USHORT)) {
        USHORT v16 = av_read_cfg16(base, offset + i);
        AV_MEMCPY(out_bytes + i, &v16, sizeof(v16));
        i += sizeof(USHORT);
    }

    while (i < length) {
        out_bytes[i] = av_read_cfg8(base, offset + i);
        i++;
    }
}

static __inline UCHAR av_read_device_status(const AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    return AV_READ8((volatile const void *)&device->CommonCfg->device_status);
}

static __inline void av_write_device_status(const AERO_VIRTIO_PCI_MODERN_DEVICE *device, UCHAR status)
{
    AV_WRITE8((volatile void *)&device->CommonCfg->device_status, status);
}

static __inline void av_select_queue_locked(const AERO_VIRTIO_PCI_MODERN_DEVICE *device, USHORT queue_index)
{
    AV_WRITE16((volatile void *)&device->CommonCfg->queue_select, queue_index);
}

/* -------------------------------------------------------------------------- */
/* Public API                                                                 */
/* -------------------------------------------------------------------------- */

NTSTATUS AeroVirtioPciModernInitFromBar0(AERO_VIRTIO_PCI_MODERN_DEVICE *device, volatile void *bar0_va, ULONG bar0_len)
{
    volatile UCHAR *base;

    if (device == NULL || bar0_va == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (bar0_len < AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE) {
        return STATUS_INVALID_PARAMETER;
    }

    AV_MEMZERO(device, sizeof(*device));

    base = (volatile UCHAR *)bar0_va;

    device->CommonCfg = (volatile virtio_pci_common_cfg *)(base + AERO_VIRTIO_PCI_MODERN_COMMON_CFG_OFFSET);
    device->NotifyBase = base + AERO_VIRTIO_PCI_MODERN_NOTIFY_OFFSET;
    device->IsrStatus = base + AERO_VIRTIO_PCI_MODERN_ISR_OFFSET;
    device->DeviceCfg = base + AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_OFFSET;
    device->NotifyOffMultiplier = AERO_VIRTIO_PCI_MODERN_NOTIFY_OFF_MULTIPLIER;

#if AERO_VIRTIO_PCI_MODERN_KERNEL_MODE
    KeInitializeSpinLock(&device->CommonCfgLock);
#else
    device->CommonCfgLock = 0;
#endif

    return STATUS_SUCCESS;
}

KIRQL AeroVirtioCommonCfgLock(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
#if AERO_VIRTIO_PCI_MODERN_KERNEL_MODE
    if (device == NULL) {
        return 0;
    }

    return KeAcquireSpinLockRaiseToDpc(&device->CommonCfgLock);
#else
    (void)device;
    return 0;
#endif
}

void AeroVirtioCommonCfgUnlock(AERO_VIRTIO_PCI_MODERN_DEVICE *device, KIRQL old_irql)
{
#if AERO_VIRTIO_PCI_MODERN_KERNEL_MODE
    if (device == NULL) {
        return;
    }

    KeReleaseSpinLock(&device->CommonCfgLock, old_irql);
#else
    (void)device;
    (void)old_irql;
#endif
}

void AeroVirtioResetDevice(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    ULONG waited_us;

    if (device == NULL || device->CommonCfg == NULL) {
        return;
    }

    AV_BARRIER();
    av_write_device_status(device, 0);
    AV_BARRIER();

    for (waited_us = 0; waited_us < AERO_VIRTIO_RESET_TIMEOUT_US; waited_us += AERO_VIRTIO_RESET_POLL_DELAY_US) {
        if (av_read_device_status(device) == 0) {
            AV_BARRIER();
            return;
        }

        AV_STALL(AERO_VIRTIO_RESET_POLL_DELAY_US);
    }
}

void AeroVirtioAddStatus(AERO_VIRTIO_PCI_MODERN_DEVICE *device, UCHAR status_bits)
{
    UCHAR status;

    if (device == NULL || device->CommonCfg == NULL) {
        return;
    }

    AV_BARRIER();
    status = av_read_device_status(device);
    status |= status_bits;
    av_write_device_status(device, status);
    AV_BARRIER();
}

UCHAR AeroVirtioGetStatus(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    if (device == NULL || device->CommonCfg == NULL) {
        return 0;
    }

    AV_BARRIER();
    return av_read_device_status(device);
}

void AeroVirtioSetStatus(AERO_VIRTIO_PCI_MODERN_DEVICE *device, UCHAR status)
{
    if (device == NULL || device->CommonCfg == NULL) {
        return;
    }

    AV_BARRIER();
    av_write_device_status(device, status);
    AV_BARRIER();
}

void AeroVirtioFailDevice(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    AeroVirtioAddStatus(device, VIRTIO_STATUS_FAILED);
}

ULONGLONG AeroVirtioReadDeviceFeatures(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    ULONG lo;
    ULONG hi;
    KIRQL irql;

    if (device == NULL || device->CommonCfg == NULL) {
        return 0;
    }

    lo = 0;
    hi = 0;

    irql = AeroVirtioCommonCfgLock(device);

    AV_WRITE32((volatile void *)&device->CommonCfg->device_feature_select, 0);
    AV_BARRIER();
    lo = AV_READ32((volatile const void *)&device->CommonCfg->device_feature);
    AV_BARRIER();

    AV_WRITE32((volatile void *)&device->CommonCfg->device_feature_select, 1);
    AV_BARRIER();
    hi = AV_READ32((volatile const void *)&device->CommonCfg->device_feature);
    AV_BARRIER();

    AeroVirtioCommonCfgUnlock(device, irql);

    return ((ULONGLONG)hi << 32) | (ULONGLONG)lo;
}

void AeroVirtioWriteDriverFeatures(AERO_VIRTIO_PCI_MODERN_DEVICE *device, ULONGLONG features)
{
    ULONG lo;
    ULONG hi;
    KIRQL irql;

    if (device == NULL || device->CommonCfg == NULL) {
        return;
    }

    lo = (ULONG)(features & 0xFFFFFFFFull);
    hi = (ULONG)(features >> 32);

    irql = AeroVirtioCommonCfgLock(device);

    AV_WRITE32((volatile void *)&device->CommonCfg->driver_feature_select, 0);
    AV_BARRIER();
    AV_WRITE32((volatile void *)&device->CommonCfg->driver_feature, lo);
    AV_BARRIER();

    AV_WRITE32((volatile void *)&device->CommonCfg->driver_feature_select, 1);
    AV_BARRIER();
    AV_WRITE32((volatile void *)&device->CommonCfg->driver_feature, hi);
    AV_BARRIER();

    AeroVirtioCommonCfgUnlock(device, irql);
}

NTSTATUS AeroVirtioNegotiateFeatures(AERO_VIRTIO_PCI_MODERN_DEVICE *device,
                                     ULONGLONG required,
                                     ULONGLONG wanted,
                                     ULONGLONG *negotiated_out)
{
    ULONGLONG device_features;
    ULONGLONG negotiated;
    UCHAR status;

    if (negotiated_out == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *negotiated_out = 0;

    if (device == NULL || device->CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    required |= VIRTIO_F_VERSION_1;

    AeroVirtioResetDevice(device);

    AeroVirtioAddStatus(device, VIRTIO_STATUS_ACKNOWLEDGE);
    AeroVirtioAddStatus(device, VIRTIO_STATUS_DRIVER);

    device_features = AeroVirtioReadDeviceFeatures(device);

    if ((device_features & required) != required) {
        AeroVirtioFailDevice(device);
        return STATUS_NOT_SUPPORTED;
    }

    negotiated = (device_features & wanted) | required;

    AeroVirtioWriteDriverFeatures(device, negotiated);
    AV_BARRIER();

    AeroVirtioAddStatus(device, VIRTIO_STATUS_FEATURES_OK);

    status = AeroVirtioGetStatus(device);
    if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
        AeroVirtioFailDevice(device);
        return STATUS_NOT_SUPPORTED;
    }

    *negotiated_out = negotiated;
    return STATUS_SUCCESS;
}

USHORT AeroVirtioGetNumQueues(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    if (device == NULL || device->CommonCfg == NULL) {
        return 0;
    }

    return AV_READ16((volatile const void *)&device->CommonCfg->num_queues);
}

NTSTATUS AeroVirtioQueryQueue(AERO_VIRTIO_PCI_MODERN_DEVICE *device,
                              USHORT queue_index,
                              USHORT *queue_size_out,
                              USHORT *queue_notify_off_out)
{
    KIRQL irql;
    USHORT size;
    USHORT notify_off;

    if (queue_size_out == NULL || queue_notify_off_out == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *queue_size_out = 0;
    *queue_notify_off_out = 0;

    if (device == NULL || device->CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    size = 0;
    notify_off = 0;

    irql = AeroVirtioCommonCfgLock(device);

    av_select_queue_locked(device, queue_index);
    AV_BARRIER();

    size = AV_READ16((volatile const void *)&device->CommonCfg->queue_size);
    AV_BARRIER();
    notify_off = AV_READ16((volatile const void *)&device->CommonCfg->queue_notify_off);
    AV_BARRIER();

    AeroVirtioCommonCfgUnlock(device, irql);

    if (size == 0) {
        return STATUS_NOT_FOUND;
    }

    *queue_size_out = size;
    *queue_notify_off_out = notify_off;
    return STATUS_SUCCESS;
}

NTSTATUS AeroVirtioSetupQueue(AERO_VIRTIO_PCI_MODERN_DEVICE *device, USHORT queue_index, ULONGLONG desc_pa, ULONGLONG avail_pa, ULONGLONG used_pa)
{
    KIRQL irql;
    USHORT size;
    USHORT enabled;

    if (device == NULL || device->CommonCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    size = 0;
    enabled = 0;

    irql = AeroVirtioCommonCfgLock(device);

    av_select_queue_locked(device, queue_index);
    AV_BARRIER();

    size = AV_READ16((volatile const void *)&device->CommonCfg->queue_size);
    if (size == 0) {
        AeroVirtioCommonCfgUnlock(device, irql);
        return STATUS_NOT_FOUND;
    }

    AV_WRITE32((volatile void *)&device->CommonCfg->queue_desc_lo, (ULONG)(desc_pa & 0xFFFFFFFFull));
    AV_WRITE32((volatile void *)&device->CommonCfg->queue_desc_hi, (ULONG)(desc_pa >> 32));

    AV_WRITE32((volatile void *)&device->CommonCfg->queue_avail_lo, (ULONG)(avail_pa & 0xFFFFFFFFull));
    AV_WRITE32((volatile void *)&device->CommonCfg->queue_avail_hi, (ULONG)(avail_pa >> 32));

    AV_WRITE32((volatile void *)&device->CommonCfg->queue_used_lo, (ULONG)(used_pa & 0xFFFFFFFFull));
    AV_WRITE32((volatile void *)&device->CommonCfg->queue_used_hi, (ULONG)(used_pa >> 32));

    /*
     * The device must observe ring addresses before queue_enable is set.
     */
    AV_BARRIER();

    AV_WRITE16((volatile void *)&device->CommonCfg->queue_enable, 1);
    AV_BARRIER();

    enabled = AV_READ16((volatile const void *)&device->CommonCfg->queue_enable);

    AeroVirtioCommonCfgUnlock(device, irql);

    if (enabled != 1) {
        return STATUS_IO_DEVICE_ERROR;
    }

    return STATUS_SUCCESS;
}

void AeroVirtioNotifyQueue(AERO_VIRTIO_PCI_MODERN_DEVICE *device, USHORT queue_index, USHORT queue_notify_off)
{
    ULONGLONG offset;
    volatile UCHAR *addr;

    if (device == NULL || device->NotifyBase == NULL || device->NotifyOffMultiplier == 0) {
        return;
    }

    offset = (ULONGLONG)queue_notify_off * (ULONGLONG)device->NotifyOffMultiplier;

    /*
     * Contract v1 fixes the notify capability window size. Bounds-check so a
     * bad queue_notify_off doesn't scribble arbitrary MMIO.
     */
    if (offset + sizeof(USHORT) > AERO_VIRTIO_PCI_MODERN_NOTIFY_SIZE) {
        return;
    }

    addr = device->NotifyBase + (ULONG)offset;

    /*
     * Ensure all prior ring writes (descriptor/ring index updates) are visible
     * before ringing the doorbell. See docs/virtio/virtqueue-split-ring-win7.md
     * (§5.1/§5.2) for the publish/notify ordering requirement.
     */
    AV_BARRIER();
    AV_WRITE16((volatile void *)addr, queue_index);
    AV_BARRIER();
}

UCHAR AeroVirtioReadIsr(AERO_VIRTIO_PCI_MODERN_DEVICE *device)
{
    UCHAR v;

    if (device == NULL || device->IsrStatus == NULL) {
        return 0;
    }

    v = AV_READ8((volatile const void *)&device->IsrStatus[0]);
    AV_BARRIER();
    return v;
}

NTSTATUS AeroVirtioReadDeviceConfig(AERO_VIRTIO_PCI_MODERN_DEVICE *device, ULONG offset, void *buffer, ULONG length)
{
    ULONG attempt;
    UCHAR gen0;
    UCHAR gen1;
    UCHAR *out_bytes;
    ULONGLONG end;

    if (length == 0) {
        return STATUS_SUCCESS;
    }

    if (device == NULL || device->CommonCfg == NULL || device->DeviceCfg == NULL || buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    end = (ULONGLONG)offset + (ULONGLONG)length;
    if (end < offset || end > AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_SIZE) {
        return STATUS_INVALID_PARAMETER;
    }

    out_bytes = (UCHAR *)buffer;

    for (attempt = 0; attempt < AERO_VIRTIO_CONFIG_MAX_READ_RETRIES; attempt++) {
        gen0 = AV_READ8((volatile const void *)&device->CommonCfg->config_generation);
        AV_BARRIER();

        av_copy_from_device(device->DeviceCfg, offset, out_bytes, length);

        AV_BARRIER();
        gen1 = AV_READ8((volatile const void *)&device->CommonCfg->config_generation);
        AV_BARRIER();

        if (gen0 == gen1) {
            return STATUS_SUCCESS;
        }
    }

    return STATUS_IO_TIMEOUT;
}

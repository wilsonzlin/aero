/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtio_pci_legacy.h"

static uint8_t vplt_read8(virtio_pci_legacy_device_t *dev, uint32_t offset)
{
    return dev->os->read_io8(dev->os_ctx, dev->io_base, offset);
}

static uint16_t vplt_read16(virtio_pci_legacy_device_t *dev, uint32_t offset)
{
    return dev->os->read_io16(dev->os_ctx, dev->io_base, offset);
}

static uint32_t vplt_read32(virtio_pci_legacy_device_t *dev, uint32_t offset)
{
    return dev->os->read_io32(dev->os_ctx, dev->io_base, offset);
}

static void vplt_write8(virtio_pci_legacy_device_t *dev, uint32_t offset, uint8_t value)
{
    dev->os->write_io8(dev->os_ctx, dev->io_base, offset, value);
}

static void vplt_write16(virtio_pci_legacy_device_t *dev, uint32_t offset, uint16_t value)
{
    dev->os->write_io16(dev->os_ctx, dev->io_base, offset, value);
}

static void vplt_write32(virtio_pci_legacy_device_t *dev, uint32_t offset, uint32_t value)
{
    dev->os->write_io32(dev->os_ctx, dev->io_base, offset, value);
}

void virtio_pci_legacy_init(virtio_pci_legacy_device_t *dev,
                            const virtio_os_ops_t *os,
                            void *os_ctx,
                            uintptr_t io_base,
                            virtio_bool_t msix_enabled)
{
    if (dev == NULL) {
        return;
    }

    dev->os = os;
    dev->os_ctx = os_ctx;
    dev->io_base = io_base;
    dev->msix_enabled = msix_enabled;
    dev->device_config_offset = (msix_enabled != VIRTIO_FALSE) ? VIRTIO_PCI_DEVICE_CFG_OFF_MSIX : VIRTIO_PCI_DEVICE_CFG_OFF_NO_MSIX;
}

uint32_t virtio_pci_legacy_get_vring_align(void)
{
    return VIRTIO_PCI_VRING_ALIGN;
}

void virtio_pci_legacy_reset(virtio_pci_legacy_device_t *dev)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io8 == NULL) {
        return;
    }

    /* Writing 0 to STATUS resets the device. */
    vplt_write8(dev, VIRTIO_PCI_STATUS, 0);
}

uint8_t virtio_pci_legacy_get_status(virtio_pci_legacy_device_t *dev)
{
    if (dev == NULL || dev->os == NULL || dev->os->read_io8 == NULL) {
        return 0;
    }
    return vplt_read8(dev, VIRTIO_PCI_STATUS);
}

void virtio_pci_legacy_set_status(virtio_pci_legacy_device_t *dev, uint8_t status)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io8 == NULL) {
        return;
    }
    vplt_write8(dev, VIRTIO_PCI_STATUS, status);
}

void virtio_pci_legacy_add_status(virtio_pci_legacy_device_t *dev, uint8_t status_bits)
{
    uint8_t status;

    if (dev == NULL || dev->os == NULL || dev->os->read_io8 == NULL || dev->os->write_io8 == NULL) {
        return;
    }

    status = vplt_read8(dev, VIRTIO_PCI_STATUS);
    status |= status_bits;
    vplt_write8(dev, VIRTIO_PCI_STATUS, status);
}

uint64_t virtio_pci_legacy_read_device_features(virtio_pci_legacy_device_t *dev)
{
    uint32_t features;

    if (dev == NULL || dev->os == NULL || dev->os->read_io32 == NULL) {
        return 0;
    }

    features = vplt_read32(dev, VIRTIO_PCI_HOST_FEATURES);
    return (uint64_t)features;
}

void virtio_pci_legacy_write_driver_features(virtio_pci_legacy_device_t *dev, uint64_t features)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io32 == NULL) {
        return;
    }

    if ((features >> 32) != 0 && dev->os->log != NULL) {
        dev->os->log(dev->os_ctx, "virtio_pci_legacy: upper 32 feature bits ignored (legacy transport)");
    }

    vplt_write32(dev, VIRTIO_PCI_GUEST_FEATURES, (uint32_t)features);
}

uint8_t virtio_pci_legacy_read_isr_status(virtio_pci_legacy_device_t *dev)
{
    if (dev == NULL || dev->os == NULL || dev->os->read_io8 == NULL) {
        return 0;
    }

    /* Reading ISR acknowledges it. */
    return vplt_read8(dev, VIRTIO_PCI_ISR);
}

void virtio_pci_legacy_select_queue(virtio_pci_legacy_device_t *dev, uint16_t queue_index)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io16 == NULL) {
        return;
    }

    vplt_write16(dev, VIRTIO_PCI_QUEUE_SEL, queue_index);
}

uint16_t virtio_pci_legacy_get_queue_size(virtio_pci_legacy_device_t *dev, uint16_t queue_index)
{
    if (dev == NULL || dev->os == NULL || dev->os->read_io16 == NULL || dev->os->write_io16 == NULL) {
        return 0;
    }

    virtio_pci_legacy_select_queue(dev, queue_index);
    return vplt_read16(dev, VIRTIO_PCI_QUEUE_NUM);
}

int virtio_pci_legacy_set_queue_pfn(virtio_pci_legacy_device_t *dev, uint16_t queue_index, uint64_t queue_paddr)
{
    uint32_t pfn;

    if (dev == NULL || dev->os == NULL || dev->os->write_io32 == NULL || dev->os->write_io16 == NULL) {
        return VIRTIO_ERR_INVAL;
    }

    /* PFN is physical address >> 12; requires 4K alignment. */
    if ((queue_paddr & 0xfffu) != 0) {
        return VIRTIO_ERR_RANGE;
    }
    if ((queue_paddr >> 12) > 0xffffffffu) {
        /* Queue PFN register is 32-bit. */
        return VIRTIO_ERR_RANGE;
    }

    pfn = (uint32_t)(queue_paddr >> 12);

    virtio_pci_legacy_select_queue(dev, queue_index);
    vplt_write32(dev, VIRTIO_PCI_QUEUE_PFN, pfn);
    return VIRTIO_OK;
}

void virtio_pci_legacy_notify_queue(virtio_pci_legacy_device_t *dev, uint16_t queue_index)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io16 == NULL) {
        return;
    }
    vplt_write16(dev, VIRTIO_PCI_QUEUE_NOTIFY, queue_index);
}

uint8_t virtio_pci_legacy_read_config8(virtio_pci_legacy_device_t *dev, uint32_t offset)
{
    if (dev == NULL || dev->os == NULL || dev->os->read_io8 == NULL) {
        return 0;
    }
    return vplt_read8(dev, dev->device_config_offset + offset);
}

uint16_t virtio_pci_legacy_read_config16(virtio_pci_legacy_device_t *dev, uint32_t offset)
{
    if (dev == NULL || dev->os == NULL || dev->os->read_io16 == NULL) {
        return 0;
    }
    return vplt_read16(dev, dev->device_config_offset + offset);
}

uint32_t virtio_pci_legacy_read_config32(virtio_pci_legacy_device_t *dev, uint32_t offset)
{
    if (dev == NULL || dev->os == NULL || dev->os->read_io32 == NULL) {
        return 0;
    }
    return vplt_read32(dev, dev->device_config_offset + offset);
}

void virtio_pci_legacy_write_config8(virtio_pci_legacy_device_t *dev, uint32_t offset, uint8_t value)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io8 == NULL) {
        return;
    }
    vplt_write8(dev, dev->device_config_offset + offset, value);
}

void virtio_pci_legacy_write_config16(virtio_pci_legacy_device_t *dev, uint32_t offset, uint16_t value)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io16 == NULL) {
        return;
    }
    vplt_write16(dev, dev->device_config_offset + offset, value);
}

void virtio_pci_legacy_write_config32(virtio_pci_legacy_device_t *dev, uint32_t offset, uint32_t value)
{
    if (dev == NULL || dev->os == NULL || dev->os->write_io32 == NULL) {
        return;
    }
    vplt_write32(dev, dev->device_config_offset + offset, value);
}

/* -------------------------------------------------------------------------- */
/* Windows kernel convenience wrapper implementation                            */
/* -------------------------------------------------------------------------- */

#if defined(_KERNEL_MODE)

static __forceinline UCHAR VirtioPciRead8(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG Offset)
{
    return READ_PORT_UCHAR(Device->IoBase + Offset);
}

static __forceinline USHORT VirtioPciRead16(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG Offset)
{
    return READ_PORT_USHORT((PUSHORT)(Device->IoBase + Offset));
}

static __forceinline ULONG VirtioPciRead32(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG Offset)
{
    return READ_PORT_ULONG((PULONG)(Device->IoBase + Offset));
}

static __forceinline VOID VirtioPciWrite8(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG Offset, _In_ UCHAR Value)
{
    WRITE_PORT_UCHAR(Device->IoBase + Offset, Value);
}

static __forceinline VOID VirtioPciWrite16(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG Offset, _In_ USHORT Value)
{
    WRITE_PORT_USHORT((PUSHORT)(Device->IoBase + Offset), Value);
}

static __forceinline VOID VirtioPciWrite32(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG Offset, _In_ ULONG Value)
{
    WRITE_PORT_ULONG((PULONG)(Device->IoBase + Offset), Value);
}

VOID VirtioPciInitialize(_Out_ VIRTIO_PCI_DEVICE *Device, _In_ PUCHAR IoBase, _In_ ULONG IoLength, _In_ BOOLEAN MsixEnabled)
{
    RtlZeroMemory(Device, sizeof(*Device));

    Device->IoBase = IoBase;
    Device->IoLength = IoLength;
    Device->MsixEnabled = MsixEnabled ? TRUE : FALSE;
    Device->DeviceConfigOffset = MsixEnabled ? VIRTIO_PCI_DEVICE_CFG_OFF_MSIX : VIRTIO_PCI_DEVICE_CFG_OFF_NO_MSIX;
}

VOID VirtioPciReset(_Inout_ VIRTIO_PCI_DEVICE *Device)
{
    UNREFERENCED_PARAMETER(Device);
    VirtioPciWrite8(Device, VIRTIO_PCI_STATUS, 0);
    KeMemoryBarrier();
}

UCHAR VirtioPciGetStatus(_In_ const VIRTIO_PCI_DEVICE *Device) { return VirtioPciRead8(Device, VIRTIO_PCI_STATUS); }

VOID VirtioPciSetStatus(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ UCHAR Status)
{
    VirtioPciWrite8(Device, VIRTIO_PCI_STATUS, Status);
    KeMemoryBarrier();
}

VOID VirtioPciAddStatus(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ UCHAR StatusBits)
{
    UCHAR Status;

    Status = VirtioPciGetStatus(Device);
    Status |= StatusBits;
    VirtioPciSetStatus(Device, Status);
}

ULONG VirtioPciReadHostFeatures(_Inout_ VIRTIO_PCI_DEVICE *Device)
{
    Device->HostFeatures = VirtioPciRead32(Device, VIRTIO_PCI_HOST_FEATURES);
    return Device->HostFeatures;
}

VOID VirtioPciWriteGuestFeatures(_Inout_ VIRTIO_PCI_DEVICE *Device, _In_ ULONG GuestFeatures)
{
    Device->GuestFeatures = GuestFeatures;
    VirtioPciWrite32(Device, VIRTIO_PCI_GUEST_FEATURES, GuestFeatures);
    KeMemoryBarrier();
}

UCHAR VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE *Device) { return VirtioPciRead8(Device, VIRTIO_PCI_ISR); }

VOID VirtioPciSelectQueue(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ USHORT QueueIndex)
{
    VirtioPciWrite16(Device, VIRTIO_PCI_QUEUE_SEL, QueueIndex);
    KeMemoryBarrier();
}

USHORT VirtioPciReadQueueSize(_In_ const VIRTIO_PCI_DEVICE *Device) { return VirtioPciRead16(Device, VIRTIO_PCI_QUEUE_NUM); }

VOID VirtioPciWriteQueuePfn(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG QueuePfn)
{
    VirtioPciWrite32(Device, VIRTIO_PCI_QUEUE_PFN, QueuePfn);
    KeMemoryBarrier();
}

VOID VirtioPciNotifyQueue(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ USHORT QueueIndex)
{
    VirtioPciWrite16(Device, VIRTIO_PCI_QUEUE_NOTIFY, QueueIndex);
    KeMemoryBarrier();
}

NTSTATUS VirtioPciReadDeviceConfig(_In_ const VIRTIO_PCI_DEVICE *Device,
                                   _In_ ULONG Offset,
                                   _Out_writes_bytes_(Length) VOID *Buffer,
                                   _In_ ULONG Length)
{
    PUCHAR Out;
    ULONG I;

    Out = (PUCHAR)Buffer;

    if (Offset + Length < Offset) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Device->DeviceConfigOffset + Offset + Length > Device->IoLength) {
        /* The caller likely passed a truncated resource length. */
        return STATUS_BUFFER_TOO_SMALL;
    }

    for (I = 0; I < Length; I++) {
        Out[I] = VirtioPciRead8(Device, Device->DeviceConfigOffset + Offset + I);
    }

    return STATUS_SUCCESS;
}

#endif /* _KERNEL_MODE */


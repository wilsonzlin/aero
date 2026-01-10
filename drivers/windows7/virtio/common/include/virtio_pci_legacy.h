/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * virtio-pci legacy / transitional transport (I/O port register set).
 *
 * This header provides:
 *   1) An OS-agnostic transport API (`virtio_pci_legacy_*`) built on the
 *      virtio OS shim (`virtio_os_ops_t`).
 *   2) Optional Windows kernel convenience wrappers (`VirtioPci*`) used by
 *      early in-tree drivers. These are only available when building in kernel
 *      mode and are excluded from the host-side unit tests.
 *
 * Register layout:
 *   - This is the classic virtio 0.9 "legacy" PCI I/O interface.
 *   - Split virtqueues use a fixed 4 KiB alignment for the ring layout.
 *   - If MSI-X is enabled, two extra vector registers are present and the
 *     device-specific config space starts at a different offset.
 */

#ifndef AERO_VIRTIO_PCI_LEGACY_H_
#define AERO_VIRTIO_PCI_LEGACY_H_

#include "virtio_bits.h"
#include "virtio_os.h"

/* Legacy virtio-pci register offsets (byte offsets from BAR base). */
#define VIRTIO_PCI_HOST_FEATURES 0x00u /* u32 */
#define VIRTIO_PCI_GUEST_FEATURES 0x04u /* u32 */
#define VIRTIO_PCI_QUEUE_PFN 0x08u /* u32 (physical >> 12) */
#define VIRTIO_PCI_QUEUE_NUM 0x0Cu /* u16 */
#define VIRTIO_PCI_QUEUE_SEL 0x0Eu /* u16 */
#define VIRTIO_PCI_QUEUE_NOTIFY 0x10u /* u16 */
#define VIRTIO_PCI_STATUS 0x12u /* u8 */
#define VIRTIO_PCI_ISR 0x13u /* u8 (read clears/acks) */

/* MSI-X only (optional). */
#define VIRTIO_PCI_CONFIG_VECTOR 0x14u /* u16 */
#define VIRTIO_PCI_QUEUE_VECTOR 0x16u /* u16 */

/* Device-specific config offset depends on whether MSI-X is enabled. */
#define VIRTIO_PCI_DEVICE_CFG_OFF_NO_MSIX 0x14u
#define VIRTIO_PCI_DEVICE_CFG_OFF_MSIX 0x18u

/* ISR status bits (read-to-ack). */
#define VIRTIO_PCI_ISR_QUEUE 0x01u
#define VIRTIO_PCI_ISR_CONFIG 0x02u

/* Legacy split vring alignment requirement (virtio-pci legacy spec). */
#define VIRTIO_PCI_VRING_ALIGN 4096u

typedef struct virtio_pci_legacy_device {
    const virtio_os_ops_t *os;
    void *os_ctx;
    uintptr_t io_base;

    virtio_bool_t msix_enabled;
    uint32_t device_config_offset;
} virtio_pci_legacy_device_t;

void virtio_pci_legacy_init(virtio_pci_legacy_device_t *dev,
                            const virtio_os_ops_t *os,
                            void *os_ctx,
                            uintptr_t io_base,
                            virtio_bool_t msix_enabled);

void virtio_pci_legacy_reset(virtio_pci_legacy_device_t *dev);

uint8_t virtio_pci_legacy_get_status(virtio_pci_legacy_device_t *dev);
void virtio_pci_legacy_set_status(virtio_pci_legacy_device_t *dev, uint8_t status);
void virtio_pci_legacy_add_status(virtio_pci_legacy_device_t *dev, uint8_t status_bits);

uint64_t virtio_pci_legacy_read_device_features(virtio_pci_legacy_device_t *dev);
void virtio_pci_legacy_write_driver_features(virtio_pci_legacy_device_t *dev, uint64_t features);

/* Reading the ISR acknowledges the interrupt. */
uint8_t virtio_pci_legacy_read_isr_status(virtio_pci_legacy_device_t *dev);

void virtio_pci_legacy_select_queue(virtio_pci_legacy_device_t *dev, uint16_t queue_index);
uint16_t virtio_pci_legacy_get_queue_size(virtio_pci_legacy_device_t *dev, uint16_t queue_index);

/* Fixed legacy alignment (4 KiB). */
uint32_t virtio_pci_legacy_get_vring_align(void);

/*
 * Set the queue base physical address.
 *
 * The legacy interface uses a 32-bit Queue PFN register which contains the
 * physical page frame number (queue_paddr >> 12).
 */
int virtio_pci_legacy_set_queue_pfn(virtio_pci_legacy_device_t *dev, uint16_t queue_index, uint64_t queue_paddr);

void virtio_pci_legacy_notify_queue(virtio_pci_legacy_device_t *dev, uint16_t queue_index);

uint8_t virtio_pci_legacy_read_config8(virtio_pci_legacy_device_t *dev, uint32_t offset);
uint16_t virtio_pci_legacy_read_config16(virtio_pci_legacy_device_t *dev, uint32_t offset);
uint32_t virtio_pci_legacy_read_config32(virtio_pci_legacy_device_t *dev, uint32_t offset);

void virtio_pci_legacy_write_config8(virtio_pci_legacy_device_t *dev, uint32_t offset, uint8_t value);
void virtio_pci_legacy_write_config16(virtio_pci_legacy_device_t *dev, uint32_t offset, uint16_t value);
void virtio_pci_legacy_write_config32(virtio_pci_legacy_device_t *dev, uint32_t offset, uint32_t value);

/* -------------------------------------------------------------------------- */
/* Windows kernel convenience wrappers                                        */
/* -------------------------------------------------------------------------- */

#if defined(_KERNEL_MODE)

#include <ntddk.h>

typedef struct _VIRTIO_PCI_DEVICE {
    PUCHAR IoBase;
    ULONG IoLength;
    BOOLEAN MsixEnabled;

    ULONG HostFeatures;
    ULONG GuestFeatures;

    ULONG DeviceConfigOffset;
} VIRTIO_PCI_DEVICE;

VOID VirtioPciInitialize(_Out_ VIRTIO_PCI_DEVICE *Device,
                         _In_ PUCHAR IoBase,
                         _In_ ULONG IoLength,
                         _In_ BOOLEAN MsixEnabled);

VOID VirtioPciReset(_Inout_ VIRTIO_PCI_DEVICE *Device);

_Ret_range_(0, 0xFF) UCHAR VirtioPciGetStatus(_In_ const VIRTIO_PCI_DEVICE *Device);
VOID VirtioPciSetStatus(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ UCHAR Status);
VOID VirtioPciAddStatus(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ UCHAR StatusBits);

_Ret_range_(0, 0xFFFFFFFF) ULONG VirtioPciReadHostFeatures(_Inout_ VIRTIO_PCI_DEVICE *Device);
VOID VirtioPciWriteGuestFeatures(_Inout_ VIRTIO_PCI_DEVICE *Device, _In_ ULONG GuestFeatures);

_Ret_range_(0, 0xFF) UCHAR VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE *Device);

VOID VirtioPciSelectQueue(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ USHORT QueueIndex);
_Ret_range_(0, 0xFFFF) USHORT VirtioPciReadQueueSize(_In_ const VIRTIO_PCI_DEVICE *Device);
VOID VirtioPciWriteQueuePfn(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ ULONG QueuePfn);
VOID VirtioPciNotifyQueue(_In_ const VIRTIO_PCI_DEVICE *Device, _In_ USHORT QueueIndex);

_Must_inspect_result_ NTSTATUS VirtioPciReadDeviceConfig(_In_ const VIRTIO_PCI_DEVICE *Device,
                                                         _In_ ULONG Offset,
                                                         _Out_writes_bytes_(Length) VOID *Buffer,
                                                         _In_ ULONG Length);

#endif /* _KERNEL_MODE */

#endif /* AERO_VIRTIO_PCI_LEGACY_H_ */


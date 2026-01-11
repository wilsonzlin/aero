/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#ifndef AERO_VIRTIO_FAKE_PCI_DEVICE_MODERN_H_
#define AERO_VIRTIO_FAKE_PCI_DEVICE_MODERN_H_

#include <stddef.h>
#include <stdint.h>

#include "virtio_bits.h"
#include "virtqueue_split.h"

#include "test_os.h"

/* Contract v1 BAR0 MMIO layout (see docs/windows7-virtio-driver-contract.md). */
enum {
    FAKE_VIRTIO_PCI_MODERN_BAR0_SIZE = 0x4000u,

    FAKE_VIRTIO_PCI_MODERN_COMMON_OFF = 0x0000u,
    FAKE_VIRTIO_PCI_MODERN_COMMON_LEN = 0x0100u,

    FAKE_VIRTIO_PCI_MODERN_NOTIFY_OFF = 0x1000u,
    FAKE_VIRTIO_PCI_MODERN_NOTIFY_LEN = 0x0100u,

    FAKE_VIRTIO_PCI_MODERN_ISR_OFF = 0x2000u,
    FAKE_VIRTIO_PCI_MODERN_ISR_LEN = 0x0020u,

    FAKE_VIRTIO_PCI_MODERN_DEVICE_OFF = 0x3000u,
    FAKE_VIRTIO_PCI_MODERN_DEVICE_LEN = 0x0100u,
};

typedef struct fake_pci_modern_queue_state {
    uint16_t queue_size;
    uint16_t queue_notify_off; /* units of notify_off_multiplier */
    uint16_t queue_enable;

    uint64_t queue_desc;
    uint64_t queue_avail;
    uint64_t queue_used;

    vring_desc_t *desc;
    vring_avail_t *avail;
    vring_used_t *used;

    uint16_t last_avail_idx;
} fake_pci_modern_queue_state_t;

typedef struct fake_pci_device_modern {
    test_os_ctx_t *os_ctx;

    /* PCI config space (256 bytes). */
    uint8_t pci_cfg[256];

    /* Device/driver state. */
    uint64_t host_features;
    uint64_t guest_features;
    uint8_t device_status;
    uint8_t isr_status;

    uint32_t device_feature_select;
    uint32_t driver_feature_select;
    uint16_t queue_select;

    uint32_t notify_off_multiplier;

    /* For tests: record which notify address was used last (BAR-relative). */
    uint32_t last_notify_offset;

    fake_pci_modern_queue_state_t queues[1];
} fake_pci_device_modern_t;

void fake_pci_device_modern_init(fake_pci_device_modern_t *dev,
                                 test_os_ctx_t *os_ctx,
                                 uint16_t queue_size,
                                 uint16_t queue_notify_off,
                                 uint32_t notify_off_multiplier);

/* PCI config space accessors (byte offsets). */
uint8_t fake_pci_modern_cfg_read8(fake_pci_device_modern_t *dev, uint32_t offset);
uint16_t fake_pci_modern_cfg_read16(fake_pci_device_modern_t *dev, uint32_t offset);
uint32_t fake_pci_modern_cfg_read32(fake_pci_device_modern_t *dev, uint32_t offset);
void fake_pci_modern_cfg_write8(fake_pci_device_modern_t *dev, uint32_t offset, uint8_t value);
void fake_pci_modern_cfg_write16(fake_pci_device_modern_t *dev, uint32_t offset, uint16_t value);
void fake_pci_modern_cfg_write32(fake_pci_device_modern_t *dev, uint32_t offset, uint32_t value);

/* BAR0 MMIO accessors (byte offsets from BAR0 base). */
uint8_t fake_pci_modern_mmio_read8(fake_pci_device_modern_t *dev, uint32_t offset);
uint16_t fake_pci_modern_mmio_read16(fake_pci_device_modern_t *dev, uint32_t offset);
uint32_t fake_pci_modern_mmio_read32(fake_pci_device_modern_t *dev, uint32_t offset);
void fake_pci_modern_mmio_write8(fake_pci_device_modern_t *dev, uint32_t offset, uint8_t value);
void fake_pci_modern_mmio_write16(fake_pci_device_modern_t *dev, uint32_t offset, uint16_t value);
void fake_pci_modern_mmio_write32(fake_pci_device_modern_t *dev, uint32_t offset, uint32_t value);

void fake_pci_modern_process_queue(fake_pci_device_modern_t *dev, uint16_t queue_index);

#endif /* AERO_VIRTIO_FAKE_PCI_DEVICE_MODERN_H_ */


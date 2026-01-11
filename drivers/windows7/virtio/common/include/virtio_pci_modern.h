/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * virtio-pci modern (Virtio 1.0+) transport.
 *
 * This implementation is intentionally OS-agnostic and depends only on the
 * `virtio_os_ops_t` shim for register access and basic services.
 *
 * The API mirrors `virtio_pci_legacy.*` but uses PCI vendor capabilities and
 * MMIO register blocks (virtio 1.0+ "modern" virtio-pci).
 */

#ifndef AERO_VIRTIO_PCI_MODERN_H_
#define AERO_VIRTIO_PCI_MODERN_H_

#include <stdint.h>

#include "virtio_bits.h"
#include "virtio_os.h"

/* PCI config space offsets used for capability discovery. */
#define VIRTIO_PCI_CFG_STATUS 0x06u /* u16 */
#define VIRTIO_PCI_CFG_CAP_PTR 0x34u /* u8 */
#define VIRTIO_PCI_STATUS_CAP_LIST 0x10u /* bit 4 in STATUS register */

/* PCI capability IDs. */
#define VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC 0x09u

/* Virtio vendor capability types (`virtio_pci_cap.cfg_type`). */
#define VIRTIO_PCI_CAP_COMMON_CFG 1u
#define VIRTIO_PCI_CAP_NOTIFY_CFG 2u
#define VIRTIO_PCI_CAP_ISR_CFG 3u
#define VIRTIO_PCI_CAP_DEVICE_CFG 4u

/* Offsets within the virtio_pci_common_cfg MMIO region (contract v1). */
#define VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE_SELECT 0x00u
#define VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE 0x04u
#define VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE_SELECT 0x08u
#define VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE 0x0Cu
#define VIRTIO_PCI_COMMON_CFG_MSIX_CONFIG 0x10u
#define VIRTIO_PCI_COMMON_CFG_NUM_QUEUES 0x12u
#define VIRTIO_PCI_COMMON_CFG_DEVICE_STATUS 0x14u
#define VIRTIO_PCI_COMMON_CFG_CONFIG_GENERATION 0x15u
#define VIRTIO_PCI_COMMON_CFG_QUEUE_SELECT 0x16u
#define VIRTIO_PCI_COMMON_CFG_QUEUE_SIZE 0x18u
#define VIRTIO_PCI_COMMON_CFG_QUEUE_MSIX_VECTOR 0x1Au
#define VIRTIO_PCI_COMMON_CFG_QUEUE_ENABLE 0x1Cu
#define VIRTIO_PCI_COMMON_CFG_QUEUE_NOTIFY_OFF 0x1Eu
#define VIRTIO_PCI_COMMON_CFG_QUEUE_DESC 0x20u
#define VIRTIO_PCI_COMMON_CFG_QUEUE_AVAIL 0x28u
#define VIRTIO_PCI_COMMON_CFG_QUEUE_USED 0x30u

typedef struct virtio_pci_cap_region {
    uint8_t bar;
    uint32_t offset;
    uint32_t length;
} virtio_pci_cap_region_t;

typedef struct virtio_pci_modern_device {
    const virtio_os_ops_t *os;
    void *os_ctx;

    /* Base handles passed to the OS shim. */
    uintptr_t pci_cfg_base;
    uintptr_t bar0_base;

    /* Discovered MMIO regions (from virtio vendor caps). */
    virtio_pci_cap_region_t common_cfg;
    virtio_pci_cap_region_t notify_cfg;
    virtio_pci_cap_region_t isr_cfg;
    virtio_pci_cap_region_t device_cfg;

    uint32_t notify_off_multiplier;

    /* Optional lock for selector-based common_cfg accesses. */
    void *common_cfg_lock;
} virtio_pci_modern_device_t;

/*
 * Initialize a modern virtio-pci transport instance.
 *
 * The caller provides:
 *   - `pci_cfg_base`: an opaque handle that `os->read_io*` can use to read PCI
 *     config space (0..255 offsets).
 *   - `bar0_base`: an opaque handle that `os->read_io*` can use to access BAR0
 *     MMIO space (byte offsets from BAR0).
 *
 * Note: For host tests these bases are backed by a fake device; for real
 * drivers they can be backed by PCI bus interface reads and MmMapIoSpace
 * mappings.
 */
int virtio_pci_modern_init(virtio_pci_modern_device_t *dev,
                           const virtio_os_ops_t *os,
                           void *os_ctx,
                           uintptr_t pci_cfg_base,
                           uintptr_t bar0_base);

void virtio_pci_modern_uninit(virtio_pci_modern_device_t *dev);

void virtio_pci_modern_reset(virtio_pci_modern_device_t *dev);

uint8_t virtio_pci_modern_get_status(virtio_pci_modern_device_t *dev);
void virtio_pci_modern_set_status(virtio_pci_modern_device_t *dev, uint8_t status);
void virtio_pci_modern_add_status(virtio_pci_modern_device_t *dev, uint8_t status_bits);

uint64_t virtio_pci_modern_read_device_features(virtio_pci_modern_device_t *dev);
void virtio_pci_modern_write_driver_features(virtio_pci_modern_device_t *dev, uint64_t features);

/*
 * Negotiate features for a modern virtio-pci device.
 *
 * Always requires `VIRTIO_F_VERSION_1`. Returns VIRTIO_OK on success.
 */
int virtio_pci_modern_negotiate_features(virtio_pci_modern_device_t *dev,
                                         uint64_t required,
                                         uint64_t wanted,
                                         uint64_t *out_negotiated);

/* Reading the ISR acknowledges the interrupt. */
uint8_t virtio_pci_modern_read_isr_status(virtio_pci_modern_device_t *dev);

uint16_t virtio_pci_modern_get_num_queues(virtio_pci_modern_device_t *dev);
uint16_t virtio_pci_modern_get_queue_size(virtio_pci_modern_device_t *dev, uint16_t queue_index);

/*
 * Program a split virtqueue via `common_cfg` (desc/avail/used physical addresses)
 * and enable it.
 */
int virtio_pci_modern_setup_queue(virtio_pci_modern_device_t *dev,
                                  uint16_t queue_index,
                                  uint64_t desc_paddr,
                                  uint64_t avail_paddr,
                                  uint64_t used_paddr);

void virtio_pci_modern_notify_queue(virtio_pci_modern_device_t *dev, uint16_t queue_index);

#endif /* AERO_VIRTIO_PCI_MODERN_H_ */


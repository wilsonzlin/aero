/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtio_pci_modern.h"

#include <string.h>

enum {
    VIRTIO_PCI_MODERN_CFG_SPACE_LEN = 256u,
    VIRTIO_PCI_MODERN_CFG_MIN_CAP_OFF = 0x40u,
    VIRTIO_PCI_MODERN_CAP_MIN_LEN = 16u,
    VIRTIO_PCI_MODERN_NOTIFY_CAP_MIN_LEN = 20u,
    VIRTIO_PCI_MODERN_MAX_CAP_ITERS = 64u,
};

static uint8_t vpcm_cfg_read8(virtio_pci_modern_device_t *dev, uint32_t offset)
{
    return dev->os->read_io8(dev->os_ctx, dev->pci_cfg_base, offset);
}

static uint16_t vpcm_cfg_read16(virtio_pci_modern_device_t *dev, uint32_t offset)
{
    return dev->os->read_io16(dev->os_ctx, dev->pci_cfg_base, offset);
}

static uint32_t vpcm_cfg_read32(virtio_pci_modern_device_t *dev, uint32_t offset)
{
    return dev->os->read_io32(dev->os_ctx, dev->pci_cfg_base, offset);
}

static uint8_t vpcm_mmio_read8(virtio_pci_modern_device_t *dev, uint32_t offset)
{
    return dev->os->read_io8(dev->os_ctx, dev->bar0_base, offset);
}

static uint16_t vpcm_mmio_read16(virtio_pci_modern_device_t *dev, uint32_t offset)
{
    return dev->os->read_io16(dev->os_ctx, dev->bar0_base, offset);
}

static uint32_t vpcm_mmio_read32(virtio_pci_modern_device_t *dev, uint32_t offset)
{
    return dev->os->read_io32(dev->os_ctx, dev->bar0_base, offset);
}

static void vpcm_mmio_write8(virtio_pci_modern_device_t *dev, uint32_t offset, uint8_t value)
{
    dev->os->write_io8(dev->os_ctx, dev->bar0_base, offset, value);
}

static void vpcm_mmio_write16(virtio_pci_modern_device_t *dev, uint32_t offset, uint16_t value)
{
    dev->os->write_io16(dev->os_ctx, dev->bar0_base, offset, value);
}

static void vpcm_mmio_write32(virtio_pci_modern_device_t *dev, uint32_t offset, uint32_t value)
{
    dev->os->write_io32(dev->os_ctx, dev->bar0_base, offset, value);
}

static void vpcm_mmio_write64(virtio_pci_modern_device_t *dev, uint32_t offset, uint64_t value)
{
    vpcm_mmio_write32(dev, offset, (uint32_t)value);
    vpcm_mmio_write32(dev, offset + 4u, (uint32_t)(value >> 32));
}

static void vpcm_lock_common_cfg(virtio_pci_modern_device_t *dev, virtio_spinlock_state_t *state)
{
    if (dev->common_cfg_lock == NULL || dev->os->spinlock_acquire == NULL) {
        return;
    }
    dev->os->spinlock_acquire(dev->os_ctx, dev->common_cfg_lock, state);
}

static void vpcm_unlock_common_cfg(virtio_pci_modern_device_t *dev, virtio_spinlock_state_t state)
{
    if (dev->common_cfg_lock == NULL || dev->os->spinlock_release == NULL) {
        return;
    }
    dev->os->spinlock_release(dev->os_ctx, dev->common_cfg_lock, state);
}

static int vpcm_parse_caps(virtio_pci_modern_device_t *dev)
{
    uint16_t status;
    uint8_t cap_ptr;
    uint8_t visited[VIRTIO_PCI_MODERN_CFG_SPACE_LEN];
    uint32_t iter;

    memset(&dev->common_cfg, 0, sizeof(dev->common_cfg));
    memset(&dev->notify_cfg, 0, sizeof(dev->notify_cfg));
    memset(&dev->isr_cfg, 0, sizeof(dev->isr_cfg));
    memset(&dev->device_cfg, 0, sizeof(dev->device_cfg));
    dev->notify_off_multiplier = 0;

    status = vpcm_cfg_read16(dev, VIRTIO_PCI_CFG_STATUS);
    if ((status & VIRTIO_PCI_STATUS_CAP_LIST) == 0) {
        return VIRTIO_ERR_IO;
    }

    cap_ptr = (uint8_t)(vpcm_cfg_read8(dev, VIRTIO_PCI_CFG_CAP_PTR) & 0xFCu);
    if (cap_ptr == 0) {
        return VIRTIO_ERR_IO;
    }

    memset(visited, 0, sizeof(visited));

    for (iter = 0; cap_ptr != 0 && iter < VIRTIO_PCI_MODERN_MAX_CAP_ITERS; iter++) {
        uint8_t cap_id;
        uint8_t cap_next;
        uint8_t cap_len;

        if ((cap_ptr & 0x03u) != 0) {
            return VIRTIO_ERR_IO;
        }
        if (cap_ptr < VIRTIO_PCI_MODERN_CFG_MIN_CAP_OFF || cap_ptr >= VIRTIO_PCI_MODERN_CFG_SPACE_LEN) {
            return VIRTIO_ERR_IO;
        }
        if (visited[cap_ptr] != 0) {
            /* Cycle in capability list. */
            return VIRTIO_ERR_IO;
        }
        visited[cap_ptr] = 1;

        cap_id = vpcm_cfg_read8(dev, cap_ptr + 0u);
        cap_next = (uint8_t)(vpcm_cfg_read8(dev, cap_ptr + 1u) & 0xFCu);
        cap_len = vpcm_cfg_read8(dev, cap_ptr + 2u);

        if (cap_id == VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC) {
            uint8_t cfg_type;
            uint8_t bar;
            uint32_t offset;
            uint32_t length;

            if (cap_len < VIRTIO_PCI_MODERN_CAP_MIN_LEN) {
                return VIRTIO_ERR_IO;
            }

            cfg_type = vpcm_cfg_read8(dev, cap_ptr + 3u);
            bar = vpcm_cfg_read8(dev, cap_ptr + 4u);
            offset = vpcm_cfg_read32(dev, cap_ptr + 8u);
            length = vpcm_cfg_read32(dev, cap_ptr + 12u);

            switch (cfg_type) {
            case VIRTIO_PCI_CAP_COMMON_CFG:
                dev->common_cfg.bar = bar;
                dev->common_cfg.offset = offset;
                dev->common_cfg.length = length;
                break;
            case VIRTIO_PCI_CAP_NOTIFY_CFG:
                if (cap_len < VIRTIO_PCI_MODERN_NOTIFY_CAP_MIN_LEN) {
                    return VIRTIO_ERR_IO;
                }
                dev->notify_cfg.bar = bar;
                dev->notify_cfg.offset = offset;
                dev->notify_cfg.length = length;
                dev->notify_off_multiplier = vpcm_cfg_read32(dev, cap_ptr + 16u);
                break;
            case VIRTIO_PCI_CAP_ISR_CFG:
                dev->isr_cfg.bar = bar;
                dev->isr_cfg.offset = offset;
                dev->isr_cfg.length = length;
                break;
            case VIRTIO_PCI_CAP_DEVICE_CFG:
                dev->device_cfg.bar = bar;
                dev->device_cfg.offset = offset;
                dev->device_cfg.length = length;
                break;
            default:
                /* Ignore. */
                break;
            }
        }

        cap_ptr = cap_next;
    }

    if (dev->common_cfg.length == 0 || dev->notify_cfg.length == 0 || dev->isr_cfg.length == 0 || dev->device_cfg.length == 0 ||
        dev->notify_off_multiplier == 0) {
        return VIRTIO_ERR_IO;
    }

    /* Contract v1 only requires BAR0, but tolerate other BAR values as long as they match. */
    if (dev->common_cfg.bar != dev->notify_cfg.bar || dev->common_cfg.bar != dev->isr_cfg.bar || dev->common_cfg.bar != dev->device_cfg.bar) {
        return VIRTIO_ERR_IO;
    }
    if (dev->common_cfg.bar != 0) {
        return VIRTIO_ERR_IO;
    }

    return VIRTIO_OK;
}

int virtio_pci_modern_init(virtio_pci_modern_device_t *dev,
                           const virtio_os_ops_t *os,
                           void *os_ctx,
                           uintptr_t pci_cfg_base,
                           uintptr_t bar0_base)
{
    if (dev == NULL || os == NULL) {
        return VIRTIO_ERR_INVAL;
    }
    if (os->read_io8 == NULL || os->read_io16 == NULL || os->read_io32 == NULL || os->write_io8 == NULL || os->write_io16 == NULL ||
        os->write_io32 == NULL) {
        return VIRTIO_ERR_INVAL;
    }

    memset(dev, 0, sizeof(*dev));
    dev->os = os;
    dev->os_ctx = os_ctx;
    dev->pci_cfg_base = pci_cfg_base;
    dev->bar0_base = bar0_base;

    if (vpcm_parse_caps(dev) != VIRTIO_OK) {
        return VIRTIO_ERR_IO;
    }

    if (dev->os->spinlock_create != NULL) {
        dev->common_cfg_lock = dev->os->spinlock_create(dev->os_ctx);
    }

    return VIRTIO_OK;
}

void virtio_pci_modern_uninit(virtio_pci_modern_device_t *dev)
{
    if (dev == NULL) {
        return;
    }
    if (dev->common_cfg_lock != NULL && dev->os != NULL && dev->os->spinlock_destroy != NULL) {
        dev->os->spinlock_destroy(dev->os_ctx, dev->common_cfg_lock);
    }
    dev->common_cfg_lock = NULL;
}

void virtio_pci_modern_reset(virtio_pci_modern_device_t *dev)
{
    if (dev == NULL || dev->os == NULL) {
        return;
    }
    vpcm_mmio_write8(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_STATUS, 0);
    if (dev->os->mb != NULL) {
        dev->os->mb(dev->os_ctx);
    }
}

uint8_t virtio_pci_modern_get_status(virtio_pci_modern_device_t *dev)
{
    if (dev == NULL || dev->os == NULL) {
        return 0;
    }
    return vpcm_mmio_read8(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_STATUS);
}

void virtio_pci_modern_set_status(virtio_pci_modern_device_t *dev, uint8_t status)
{
    if (dev == NULL || dev->os == NULL) {
        return;
    }
    vpcm_mmio_write8(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_STATUS, status);
    if (dev->os->mb != NULL) {
        dev->os->mb(dev->os_ctx);
    }
}

void virtio_pci_modern_add_status(virtio_pci_modern_device_t *dev, uint8_t status_bits)
{
    uint8_t status;

    if (dev == NULL || dev->os == NULL) {
        return;
    }

    status = virtio_pci_modern_get_status(dev);
    status |= status_bits;
    virtio_pci_modern_set_status(dev, status);
}

uint64_t virtio_pci_modern_read_device_features(virtio_pci_modern_device_t *dev)
{
    virtio_spinlock_state_t st = 0;
    uint32_t lo;
    uint32_t hi;

    if (dev == NULL || dev->os == NULL) {
        return 0;
    }

    vpcm_lock_common_cfg(dev, &st);

    vpcm_mmio_write32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE_SELECT, 0);
    lo = vpcm_mmio_read32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE);
    vpcm_mmio_write32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE_SELECT, 1);
    hi = vpcm_mmio_read32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE);

    vpcm_unlock_common_cfg(dev, st);

    return ((uint64_t)hi << 32) | (uint64_t)lo;
}

void virtio_pci_modern_write_driver_features(virtio_pci_modern_device_t *dev, uint64_t features)
{
    virtio_spinlock_state_t st = 0;

    if (dev == NULL || dev->os == NULL) {
        return;
    }

    vpcm_lock_common_cfg(dev, &st);

    vpcm_mmio_write32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE_SELECT, 0);
    vpcm_mmio_write32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE, (uint32_t)features);
    vpcm_mmio_write32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE_SELECT, 1);
    vpcm_mmio_write32(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE, (uint32_t)(features >> 32));

    vpcm_unlock_common_cfg(dev, st);

    if (dev->os->mb != NULL) {
        dev->os->mb(dev->os_ctx);
    }
}

int virtio_pci_modern_negotiate_features(virtio_pci_modern_device_t *dev,
                                         uint64_t required,
                                         uint64_t wanted,
                                         uint64_t *out_negotiated)
{
    uint64_t device_features;
    uint64_t negotiated;
    uint8_t status;

    if (dev == NULL) {
        return VIRTIO_ERR_INVAL;
    }

    virtio_pci_modern_reset(dev);
    virtio_pci_modern_add_status(dev, VIRTIO_STATUS_ACKNOWLEDGE);
    virtio_pci_modern_add_status(dev, VIRTIO_STATUS_DRIVER);

    device_features = virtio_pci_modern_read_device_features(dev);
    if ((device_features & VIRTIO_F_VERSION_1) == 0) {
        return VIRTIO_ERR_IO;
    }

    negotiated = (device_features & wanted) | required | VIRTIO_F_VERSION_1;
    virtio_pci_modern_write_driver_features(dev, negotiated);

    virtio_pci_modern_add_status(dev, VIRTIO_STATUS_FEATURES_OK);
    status = virtio_pci_modern_get_status(dev);
    if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
        return VIRTIO_ERR_IO;
    }

    if (out_negotiated != NULL) {
        *out_negotiated = negotiated;
    }

    return VIRTIO_OK;
}

uint8_t virtio_pci_modern_read_isr_status(virtio_pci_modern_device_t *dev)
{
    if (dev == NULL || dev->os == NULL) {
        return 0;
    }
    return vpcm_mmio_read8(dev, dev->isr_cfg.offset);
}

uint16_t virtio_pci_modern_get_num_queues(virtio_pci_modern_device_t *dev)
{
    if (dev == NULL || dev->os == NULL) {
        return 0;
    }
    return vpcm_mmio_read16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_NUM_QUEUES);
}

uint16_t virtio_pci_modern_get_queue_size(virtio_pci_modern_device_t *dev, uint16_t queue_index)
{
    virtio_spinlock_state_t st = 0;
    uint16_t qsz;

    if (dev == NULL || dev->os == NULL) {
        return 0;
    }

    vpcm_lock_common_cfg(dev, &st);
    vpcm_mmio_write16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_SELECT, queue_index);
    qsz = vpcm_mmio_read16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_SIZE);
    vpcm_unlock_common_cfg(dev, st);

    return qsz;
}

int virtio_pci_modern_setup_queue(virtio_pci_modern_device_t *dev,
                                  uint16_t queue_index,
                                  uint64_t desc_paddr,
                                  uint64_t avail_paddr,
                                  uint64_t used_paddr)
{
    virtio_spinlock_state_t st = 0;

    if (dev == NULL || dev->os == NULL) {
        return VIRTIO_ERR_INVAL;
    }

    vpcm_lock_common_cfg(dev, &st);

    vpcm_mmio_write16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_SELECT, queue_index);

    vpcm_mmio_write64(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_DESC, desc_paddr);
    vpcm_mmio_write64(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_AVAIL, avail_paddr);
    vpcm_mmio_write64(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_USED, used_paddr);
    vpcm_mmio_write16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_ENABLE, 1);

    vpcm_unlock_common_cfg(dev, st);

    if (dev->os->mb != NULL) {
        dev->os->mb(dev->os_ctx);
    }

    return VIRTIO_OK;
}

void virtio_pci_modern_notify_queue(virtio_pci_modern_device_t *dev, uint16_t queue_index)
{
    virtio_spinlock_state_t st = 0;
    uint16_t notify_off;
    uint32_t notify_addr_off;

    if (dev == NULL || dev->os == NULL) {
        return;
    }

    vpcm_lock_common_cfg(dev, &st);
    vpcm_mmio_write16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_SELECT, queue_index);
    notify_off = vpcm_mmio_read16(dev, dev->common_cfg.offset + VIRTIO_PCI_COMMON_CFG_QUEUE_NOTIFY_OFF);
    vpcm_unlock_common_cfg(dev, st);

    notify_addr_off = dev->notify_cfg.offset + ((uint32_t)notify_off * dev->notify_off_multiplier);
    vpcm_mmio_write16(dev, notify_addr_off, queue_index);

    if (dev->os->mb != NULL) {
        dev->os->mb(dev->os_ctx);
    }
}


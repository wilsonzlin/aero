/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "fake_pci_device_modern.h"

#include <assert.h>
#include <string.h>

#include "virtio_pci_modern.h"

static uint16_t fake_le16_read(const uint8_t *p)
{
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t fake_le32_read(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void fake_le16_write(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

static void fake_le32_write(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static void fake_write_virtio_cap(fake_pci_device_modern_t *dev,
                                  uint8_t cap_off,
                                  uint8_t cap_next,
                                  uint8_t cfg_type,
                                  uint32_t offset,
                                  uint32_t length,
                                  uint8_t cap_len,
                                  uint32_t notify_off_multiplier)
{
    uint8_t *c;

    assert(dev != NULL);
    assert(cap_len >= 16u);
    c = &dev->pci_cfg[cap_off];

    c[0] = VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC;
    c[1] = cap_next;
    c[2] = cap_len;
    c[3] = cfg_type;
    c[4] = 0; /* BAR0 */
    c[5] = 0; /* id */
    c[6] = 0;
    c[7] = 0;
    fake_le32_write(&c[8], offset);
    fake_le32_write(&c[12], length);

    if (cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG) {
        assert(cap_len >= 20u);
        fake_le32_write(&c[16], notify_off_multiplier);
    }
}

static void fake_modern_reset(fake_pci_device_modern_t *dev)
{
    size_t i;
    assert(dev != NULL);

    dev->guest_features = 0;
    dev->device_status = 0;
    dev->isr_status = 0;
    dev->device_feature_select = 0;
    dev->driver_feature_select = 0;
    dev->queue_select = 0;
    dev->last_notify_offset = 0;

    for (i = 0; i < VIRTIO_ARRAY_SIZE(dev->queues); i++) {
        fake_pci_modern_queue_state_t *qs;
        qs = &dev->queues[i];
        qs->queue_enable = 0;
        qs->queue_desc = 0;
        qs->queue_avail = 0;
        qs->queue_used = 0;
        qs->desc = NULL;
        qs->avail = NULL;
        qs->used = NULL;
        qs->last_avail_idx = 0;
    }
}

static fake_pci_modern_queue_state_t *fake_modern_sel_queue(fake_pci_device_modern_t *dev)
{
    if (dev == NULL) {
        return NULL;
    }
    if (dev->queue_select >= (uint16_t)VIRTIO_ARRAY_SIZE(dev->queues)) {
        return NULL;
    }
    return &dev->queues[dev->queue_select];
}

static void fake_modern_update_ring_ptrs(fake_pci_device_modern_t *dev, uint16_t q)
{
    fake_pci_modern_queue_state_t *qs;

    qs = &dev->queues[q];
    qs->desc = NULL;
    qs->avail = NULL;
    qs->used = NULL;
    qs->last_avail_idx = 0;

    if (qs->queue_enable == 0) {
        return;
    }
    if (qs->queue_desc == 0 || qs->queue_avail == 0 || qs->queue_used == 0) {
        return;
    }

    qs->desc = (vring_desc_t *)test_os_phys_to_virt(dev->os_ctx, qs->queue_desc);
    qs->avail = (vring_avail_t *)test_os_phys_to_virt(dev->os_ctx, qs->queue_avail);
    qs->used = (vring_used_t *)test_os_phys_to_virt(dev->os_ctx, qs->queue_used);
}

void fake_pci_device_modern_init(fake_pci_device_modern_t *dev,
                                 test_os_ctx_t *os_ctx,
                                 uint16_t queue_size,
                                 uint16_t queue_notify_off,
                                 uint32_t notify_off_multiplier)
{
    assert(dev != NULL);
    assert(os_ctx != NULL);
    assert(queue_size != 0);
    assert(notify_off_multiplier != 0);

    memset(dev, 0, sizeof(*dev));
    dev->os_ctx = os_ctx;
    dev->notify_off_multiplier = notify_off_multiplier;

    dev->host_features = VIRTIO_F_VERSION_1 | (uint64_t)VIRTIO_RING_F_INDIRECT_DESC;

    dev->queues[0].queue_size = queue_size;
    dev->queues[0].queue_notify_off = queue_notify_off;

    /* Minimal PCI config header with capability list. */
    memset(dev->pci_cfg, 0, sizeof(dev->pci_cfg));

    /* Vendor ID 0x1AF4, Device ID 0x1041 (virtio-net modern ID space). */
    fake_le16_write(&dev->pci_cfg[0x00], 0x1AF4u);
    fake_le16_write(&dev->pci_cfg[0x02], 0x1041u);

    /* Status: capabilities list present. */
    fake_le16_write(&dev->pci_cfg[VIRTIO_PCI_CFG_STATUS], VIRTIO_PCI_STATUS_CAP_LIST);

    /* Capability pointer at 0x34. */
    dev->pci_cfg[VIRTIO_PCI_CFG_CAP_PTR] = 0x40u;

    /* Capability list. */
    fake_write_virtio_cap(dev,
                          0x40u,
                          0x50u,
                          VIRTIO_PCI_CAP_COMMON_CFG,
                          FAKE_VIRTIO_PCI_MODERN_COMMON_OFF,
                          FAKE_VIRTIO_PCI_MODERN_COMMON_LEN,
                          16u,
                          0);
    fake_write_virtio_cap(dev,
                          0x50u,
                          0x64u,
                          VIRTIO_PCI_CAP_NOTIFY_CFG,
                          FAKE_VIRTIO_PCI_MODERN_NOTIFY_OFF,
                          FAKE_VIRTIO_PCI_MODERN_NOTIFY_LEN,
                          20u,
                          notify_off_multiplier);
    fake_write_virtio_cap(dev,
                          0x64u,
                          0x74u,
                          VIRTIO_PCI_CAP_ISR_CFG,
                          FAKE_VIRTIO_PCI_MODERN_ISR_OFF,
                          FAKE_VIRTIO_PCI_MODERN_ISR_LEN,
                          16u,
                          0);
    fake_write_virtio_cap(dev,
                          0x74u,
                          0x00u,
                          VIRTIO_PCI_CAP_DEVICE_CFG,
                          FAKE_VIRTIO_PCI_MODERN_DEVICE_OFF,
                          FAKE_VIRTIO_PCI_MODERN_DEVICE_LEN,
                          16u,
                          0);

    fake_modern_reset(dev);
}

uint8_t fake_pci_modern_cfg_read8(fake_pci_device_modern_t *dev, uint32_t offset)
{
    if (dev == NULL || offset >= sizeof(dev->pci_cfg)) {
        return 0;
    }
    return dev->pci_cfg[offset];
}

uint16_t fake_pci_modern_cfg_read16(fake_pci_device_modern_t *dev, uint32_t offset)
{
    if (dev == NULL || offset + 1u >= sizeof(dev->pci_cfg)) {
        return 0;
    }
    return fake_le16_read(&dev->pci_cfg[offset]);
}

uint32_t fake_pci_modern_cfg_read32(fake_pci_device_modern_t *dev, uint32_t offset)
{
    if (dev == NULL || offset + 3u >= sizeof(dev->pci_cfg)) {
        return 0;
    }
    return fake_le32_read(&dev->pci_cfg[offset]);
}

void fake_pci_modern_cfg_write8(fake_pci_device_modern_t *dev, uint32_t offset, uint8_t value)
{
    if (dev == NULL || offset >= sizeof(dev->pci_cfg)) {
        return;
    }
    dev->pci_cfg[offset] = value;
}

void fake_pci_modern_cfg_write16(fake_pci_device_modern_t *dev, uint32_t offset, uint16_t value)
{
    if (dev == NULL || offset + 1u >= sizeof(dev->pci_cfg)) {
        return;
    }
    fake_le16_write(&dev->pci_cfg[offset], value);
}

void fake_pci_modern_cfg_write32(fake_pci_device_modern_t *dev, uint32_t offset, uint32_t value)
{
    if (dev == NULL || offset + 3u >= sizeof(dev->pci_cfg)) {
        return;
    }
    fake_le32_write(&dev->pci_cfg[offset], value);
}

uint8_t fake_pci_modern_mmio_read8(fake_pci_device_modern_t *dev, uint32_t offset)
{
    if (dev == NULL) {
        return 0;
    }

    if (offset == FAKE_VIRTIO_PCI_MODERN_ISR_OFF) {
        uint8_t isr;
        isr = dev->isr_status;
        dev->isr_status = 0; /* read-to-ack */
        return isr;
    }

    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DEVICE_STATUS)) {
        return dev->device_status;
    }
    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_CONFIG_GENERATION)) {
        return 0;
    }

    return 0;
}

uint16_t fake_pci_modern_mmio_read16(fake_pci_device_modern_t *dev, uint32_t offset)
{
    fake_pci_modern_queue_state_t *qs;

    if (dev == NULL) {
        return 0;
    }

    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_MSIX_CONFIG)) {
        return 0xFFFFu;
    }
    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_NUM_QUEUES)) {
        return (uint16_t)VIRTIO_ARRAY_SIZE(dev->queues);
    }
    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_SELECT)) {
        return dev->queue_select;
    }

    qs = fake_modern_sel_queue(dev);
    if (qs == NULL) {
        return 0;
    }

    switch (offset) {
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_SIZE:
        return qs->queue_size;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_MSIX_VECTOR:
        return 0xFFFFu;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_ENABLE:
        return qs->queue_enable;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_NOTIFY_OFF:
        return qs->queue_notify_off;
    default:
        return 0;
    }
}

uint32_t fake_pci_modern_mmio_read32(fake_pci_device_modern_t *dev, uint32_t offset)
{
    fake_pci_modern_queue_state_t *qs;

    if (dev == NULL) {
        return 0;
    }

    switch (offset) {
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE_SELECT:
        return dev->device_feature_select;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE:
        if (dev->device_feature_select == 0) {
            return (uint32_t)dev->host_features;
        }
        if (dev->device_feature_select == 1) {
            return (uint32_t)(dev->host_features >> 32);
        }
        return 0;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE_SELECT:
        return dev->driver_feature_select;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE:
        if (dev->driver_feature_select == 0) {
            return (uint32_t)dev->guest_features;
        }
        if (dev->driver_feature_select == 1) {
            return (uint32_t)(dev->guest_features >> 32);
        }
        return 0;
    default:
        break;
    }

    qs = fake_modern_sel_queue(dev);
    if (qs == NULL) {
        return 0;
    }

    switch (offset) {
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_DESC:
        return (uint32_t)qs->queue_desc;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_DESC + 4u:
        return (uint32_t)(qs->queue_desc >> 32);
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_AVAIL:
        return (uint32_t)qs->queue_avail;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_AVAIL + 4u:
        return (uint32_t)(qs->queue_avail >> 32);
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_USED:
        return (uint32_t)qs->queue_used;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_USED + 4u:
        return (uint32_t)(qs->queue_used >> 32);
    default:
        return 0;
    }
}

void fake_pci_modern_mmio_write8(fake_pci_device_modern_t *dev, uint32_t offset, uint8_t value)
{
    if (dev == NULL) {
        return;
    }

    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DEVICE_STATUS)) {
        if (value == 0) {
            fake_modern_reset(dev);
            return;
        }

        dev->device_status = value;
        if ((value & VIRTIO_STATUS_FEATURES_OK) != 0 && (dev->guest_features & VIRTIO_F_VERSION_1) == 0) {
            /* Device rejects FEATURES_OK if VERSION_1 was not accepted. */
            dev->device_status = (uint8_t)(value & ~VIRTIO_STATUS_FEATURES_OK);
        }
        return;
    }
}

void fake_pci_modern_mmio_write16(fake_pci_device_modern_t *dev, uint32_t offset, uint16_t value)
{
    fake_pci_modern_queue_state_t *qs;

    if (dev == NULL) {
        return;
    }

    /* Notify region: write to queue-specific notify address. */
    if (offset >= FAKE_VIRTIO_PCI_MODERN_NOTIFY_OFF &&
        offset < (FAKE_VIRTIO_PCI_MODERN_NOTIFY_OFF + FAKE_VIRTIO_PCI_MODERN_NOTIFY_LEN)) {
        uint32_t rel;
        rel = offset - FAKE_VIRTIO_PCI_MODERN_NOTIFY_OFF;
        dev->last_notify_offset = offset;

        /* Only one queue in this fake device. */
        qs = &dev->queues[0];
        if (rel == ((uint32_t)qs->queue_notify_off * dev->notify_off_multiplier) && qs->queue_enable != 0) {
            (void)value; /* queue index value ignored (offset selects queue). */
            fake_pci_modern_process_queue(dev, 0);
        }
        return;
    }

    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_SELECT)) {
        dev->queue_select = value;
        return;
    }

    qs = fake_modern_sel_queue(dev);
    if (qs == NULL) {
        return;
    }

    if (offset == (FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_ENABLE)) {
        qs->queue_enable = value ? 1u : 0u;
        fake_modern_update_ring_ptrs(dev, dev->queue_select);
        return;
    }
}

void fake_pci_modern_mmio_write32(fake_pci_device_modern_t *dev, uint32_t offset, uint32_t value)
{
    fake_pci_modern_queue_state_t *qs;

    if (dev == NULL) {
        return;
    }

    switch (offset) {
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DEVICE_FEATURE_SELECT:
        dev->device_feature_select = value;
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE_SELECT:
        dev->driver_feature_select = value;
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_DRIVER_FEATURE:
        if (dev->driver_feature_select == 0) {
            dev->guest_features = (dev->guest_features & 0xFFFFFFFF00000000ull) | (uint64_t)value;
        } else if (dev->driver_feature_select == 1) {
            dev->guest_features = (dev->guest_features & 0x00000000FFFFFFFFull) | ((uint64_t)value << 32);
        }
        return;
    default:
        break;
    }

    qs = fake_modern_sel_queue(dev);
    if (qs == NULL) {
        return;
    }

    switch (offset) {
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_DESC:
        qs->queue_desc = (qs->queue_desc & 0xFFFFFFFF00000000ull) | (uint64_t)value;
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_DESC + 4u:
        qs->queue_desc = (qs->queue_desc & 0x00000000FFFFFFFFull) | ((uint64_t)value << 32);
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_AVAIL:
        qs->queue_avail = (qs->queue_avail & 0xFFFFFFFF00000000ull) | (uint64_t)value;
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_AVAIL + 4u:
        qs->queue_avail = (qs->queue_avail & 0x00000000FFFFFFFFull) | ((uint64_t)value << 32);
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_USED:
        qs->queue_used = (qs->queue_used & 0xFFFFFFFF00000000ull) | (uint64_t)value;
        return;
    case FAKE_VIRTIO_PCI_MODERN_COMMON_OFF + VIRTIO_PCI_COMMON_CFG_QUEUE_USED + 4u:
        qs->queue_used = (qs->queue_used & 0x00000000FFFFFFFFull) | ((uint64_t)value << 32);
        return;
    default:
        return;
    }
}

static uint32_t fake_sum_desc_len(fake_pci_device_modern_t *dev, fake_pci_modern_queue_state_t *qs, uint16_t head)
{
    uint32_t sum;
    uint16_t idx;
    uint16_t limit;

    sum = 0;
    if (head >= qs->queue_size) {
        return 0;
    }

    if ((qs->desc[head].flags & VRING_DESC_F_INDIRECT) != 0) {
        uint16_t i;
        uint16_t n;
        vring_desc_t *table;

        n = (uint16_t)(qs->desc[head].len / sizeof(vring_desc_t));
        if (n == 0) {
            return 0;
        }

        table = (vring_desc_t *)test_os_phys_to_virt(dev->os_ctx, qs->desc[head].addr);
        if (table == NULL) {
            return 0;
        }

        for (i = 0; i < n; i++) {
            sum += table[i].len;
            if ((table[i].flags & VRING_DESC_F_NEXT) == 0) {
                break;
            }
        }
        return sum;
    }

    idx = head;
    limit = qs->queue_size;
    while (limit-- != 0) {
        vring_desc_t *d;
        d = &qs->desc[idx];
        sum += d->len;
        if ((d->flags & VRING_DESC_F_NEXT) == 0) {
            break;
        }
        idx = d->next;
        if (idx >= qs->queue_size) {
            break;
        }
    }

    return sum;
}

void fake_pci_modern_process_queue(fake_pci_device_modern_t *dev, uint16_t queue_index)
{
    fake_pci_modern_queue_state_t *qs;
    uint16_t avail_idx;

    if (dev == NULL || queue_index >= VIRTIO_ARRAY_SIZE(dev->queues)) {
        return;
    }

    qs = &dev->queues[queue_index];
    if (qs->avail == NULL || qs->used == NULL || qs->desc == NULL) {
        return;
    }

    avail_idx = qs->avail->idx;
    while (qs->last_avail_idx != avail_idx) {
        uint16_t slot;
        uint16_t head;
        uint16_t used_slot;
        uint32_t len;

        slot = (uint16_t)(qs->last_avail_idx % qs->queue_size);
        head = qs->avail->ring[slot];

        len = fake_sum_desc_len(dev, qs, head);

        used_slot = (uint16_t)(qs->used->idx % qs->queue_size);
        qs->used->ring[used_slot].id = head;
        qs->used->ring[used_slot].len = len;
        qs->used->idx++;

        qs->last_avail_idx++;
    }

    /* Signal INTx via ISR bit 0. */
    dev->isr_status |= 0x1u;
}


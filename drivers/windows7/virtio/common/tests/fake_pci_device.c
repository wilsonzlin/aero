/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "fake_pci_device.h"

#include <assert.h>
#include <string.h>

static size_t fake_avail_size(uint16_t queue_size, virtio_bool_t event_idx)
{
    size_t size;
    size = sizeof(uint16_t) * 2u;
    size += sizeof(uint16_t) * (size_t)queue_size;
    if (event_idx != VIRTIO_FALSE) {
        size += sizeof(uint16_t);
    }
    return size;
}

static void fake_update_ring_ptrs(fake_pci_device_t *dev, uint16_t q)
{
    fake_pci_queue_state_t *qs;
    virtio_bool_t event_idx;
    size_t desc_size;
    size_t avail_off;
    size_t used_off;
    uint8_t *base;
    void *ring;

    qs = &dev->queues[q];
    if (qs->queue_pfn == 0) {
        qs->ring_vaddr = NULL;
        qs->desc = NULL;
        qs->avail = NULL;
        qs->used = NULL;
        qs->used_event = NULL;
        qs->last_avail_idx = 0;
        return;
    }

    ring = test_os_phys_to_virt(dev->os_ctx, ((uint64_t)qs->queue_pfn) << 12);
    qs->ring_vaddr = ring;
    if (ring == NULL) {
        return;
    }

    base = (uint8_t *)ring;
    desc_size = sizeof(vring_desc_t) * (size_t)qs->queue_size;
    avail_off = desc_size;
    event_idx = (dev->guest_features & VIRTIO_RING_F_EVENT_IDX) ? VIRTIO_TRUE : VIRTIO_FALSE;
    used_off = virtio_align_up_size(avail_off + fake_avail_size(qs->queue_size, event_idx), (size_t)dev->queue_align);

    qs->desc = (vring_desc_t *)(void *)(base);
    qs->avail = (vring_avail_t *)(void *)(base + avail_off);
    qs->used = (vring_used_t *)(void *)(base + used_off);

    if (event_idx != VIRTIO_FALSE) {
        qs->used_event = &qs->avail->ring[qs->queue_size];
        *qs->used_event = (uint16_t)(qs->last_avail_idx + (dev->notify_batch ? (dev->notify_batch - 1u) : 0u));
    } else {
        qs->used_event = NULL;
    }
}

void fake_pci_device_init(fake_pci_device_t *dev,
                          test_os_ctx_t *os_ctx,
                          uint16_t queue_size,
                          uint32_t queue_align,
                          virtio_bool_t event_idx,
                          uint16_t notify_batch)
{
    assert(dev != NULL);

    memset(dev, 0, sizeof(*dev));
    dev->os_ctx = os_ctx;
    dev->queue_align = queue_align;
    dev->queue_sel = 0;
    dev->event_idx = event_idx;
    dev->notify_batch = notify_batch == 0 ? 1u : notify_batch;

    dev->host_features = VIRTIO_RING_F_INDIRECT_DESC;
    if (event_idx != VIRTIO_FALSE) {
        dev->host_features |= VIRTIO_RING_F_EVENT_IDX;
    }

    dev->queues[0].queue_size = queue_size;
}

uint8_t fake_pci_read8(fake_pci_device_t *dev, uint32_t offset)
{
    if (dev == NULL) {
        return 0;
    }

    switch (offset) {
    case VIRTIO_PCI_STATUS:
        return dev->status;
    case VIRTIO_PCI_ISR: {
        uint8_t isr;
        isr = dev->isr;
        dev->isr = 0; /* read-to-ack */
        return isr;
    }
    default:
        return 0;
    }
}

uint16_t fake_pci_read16(fake_pci_device_t *dev, uint32_t offset)
{
    if (dev == NULL) {
        return 0;
    }

    switch (offset) {
    case VIRTIO_PCI_QUEUE_NUM:
        return dev->queues[dev->queue_sel].queue_size;
    case VIRTIO_PCI_QUEUE_SEL:
        return dev->queue_sel;
    default:
        return 0;
    }
}

uint32_t fake_pci_read32(fake_pci_device_t *dev, uint32_t offset)
{
    if (dev == NULL) {
        return 0;
    }

    switch (offset) {
    case VIRTIO_PCI_HOST_FEATURES:
        return dev->host_features;
    case VIRTIO_PCI_GUEST_FEATURES:
        return dev->guest_features;
    case VIRTIO_PCI_QUEUE_PFN:
        return dev->queues[dev->queue_sel].queue_pfn;
    default:
        return 0;
    }
}

void fake_pci_write8(fake_pci_device_t *dev, uint32_t offset, uint8_t value)
{
    if (dev == NULL) {
        return;
    }

    switch (offset) {
    case VIRTIO_PCI_STATUS:
        dev->status = value;
        if (value == 0) {
            /* reset */
            dev->guest_features = 0;
            dev->isr = 0;
            dev->queue_sel = 0;
            dev->queues[0].queue_pfn = 0;
            fake_update_ring_ptrs(dev, 0);
        }
        break;
    default:
        break;
    }
}

void fake_pci_write16(fake_pci_device_t *dev, uint32_t offset, uint16_t value)
{
    if (dev == NULL) {
        return;
    }

    switch (offset) {
    case VIRTIO_PCI_QUEUE_SEL:
        dev->queue_sel = value;
        break;
    case VIRTIO_PCI_QUEUE_NOTIFY:
        fake_pci_process_queue(dev, value);
        break;
    default:
        break;
    }
}

void fake_pci_write32(fake_pci_device_t *dev, uint32_t offset, uint32_t value)
{
    if (dev == NULL) {
        return;
    }

    switch (offset) {
    case VIRTIO_PCI_GUEST_FEATURES:
        dev->guest_features = value;
        break;
    case VIRTIO_PCI_QUEUE_PFN:
        dev->queues[dev->queue_sel].queue_pfn = value;
        fake_update_ring_ptrs(dev, dev->queue_sel);
        break;
    default:
        break;
    }
}

static uint32_t fake_sum_desc_len(fake_pci_device_t *dev, fake_pci_queue_state_t *qs, uint16_t head)
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

void fake_pci_process_queue(fake_pci_device_t *dev, uint16_t queue_index)
{
    fake_pci_queue_state_t *qs;
    uint16_t avail_idx;

    if (dev == NULL || queue_index >= VIRTIO_ARRAY_SIZE(dev->queues)) {
        return;
    }

    qs = &dev->queues[queue_index];
    if (qs->avail == NULL || qs->used == NULL) {
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

    if ((dev->guest_features & VIRTIO_RING_F_EVENT_IDX) != 0 && qs->used_event != NULL) {
        *qs->used_event = (uint16_t)(qs->last_avail_idx + (dev->notify_batch - 1u));
    }

    /* Signal an interrupt (queue update). */
    dev->isr |= 0x1u;
}

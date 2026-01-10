/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#ifndef AERO_VIRTIO_FAKE_PCI_DEVICE_H_
#define AERO_VIRTIO_FAKE_PCI_DEVICE_H_

#include <stdint.h>

#include "virtio_pci_legacy.h"
#include "virtqueue_split.h"

#include "test_os.h"

typedef struct fake_pci_queue_state {
    uint16_t queue_size;
    uint32_t queue_pfn;

    void *ring_vaddr;
    vring_desc_t *desc;
    vring_avail_t *avail;
    vring_used_t *used;
    uint16_t *used_event;

    uint16_t last_avail_idx;
} fake_pci_queue_state_t;

typedef struct fake_pci_device {
    test_os_ctx_t *os_ctx;

    uint32_t host_features;
    uint32_t guest_features;
    uint8_t status;
    uint8_t isr;
    uint32_t queue_align;

    uint16_t queue_sel;

    virtio_bool_t event_idx;
    uint16_t notify_batch; /* for VIRTIO_RING_F_EVENT_IDX: request notify every N entries */

    fake_pci_queue_state_t queues[1];
} fake_pci_device_t;

void fake_pci_device_init(fake_pci_device_t *dev,
                          test_os_ctx_t *os_ctx,
                          uint16_t queue_size,
                          uint32_t queue_align,
                          virtio_bool_t event_idx,
                          uint16_t notify_batch);

/* PIO handlers used by the unit-test OS shim. */
uint8_t fake_pci_read8(fake_pci_device_t *dev, uint32_t offset);
uint16_t fake_pci_read16(fake_pci_device_t *dev, uint32_t offset);
uint32_t fake_pci_read32(fake_pci_device_t *dev, uint32_t offset);
void fake_pci_write8(fake_pci_device_t *dev, uint32_t offset, uint8_t value);
void fake_pci_write16(fake_pci_device_t *dev, uint32_t offset, uint16_t value);
void fake_pci_write32(fake_pci_device_t *dev, uint32_t offset, uint32_t value);

/* Process a queue (similar to a device consuming avail and producing used). */
void fake_pci_process_queue(fake_pci_device_t *dev, uint16_t queue_index);

#endif /* AERO_VIRTIO_FAKE_PCI_DEVICE_H_ */


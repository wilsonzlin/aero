#include "virtio_input.h"

#include <string.h>

static void virtio_input_report_ring_init(struct virtio_input_report_ring *ring) {
  memset(ring, 0, sizeof(*ring));
}

static void virtio_input_report_ring_push(struct virtio_input_device *dev, const uint8_t *data, size_t len) {
  struct virtio_input_report_ring *ring = &dev->report_ring;
  if (len > VIRTIO_INPUT_REPORT_MAX_SIZE) {
    return;
  }

  /*
   * Input reports are stateful; dropping intermediate reports is typically
   * preferable to blocking when the consumer is slow. We deterministically
   * drop the oldest report when the ring is full.
   */
  if (ring->count == VIRTIO_INPUT_REPORT_RING_CAPACITY) {
    ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
    ring->count--;
  }

  struct virtio_input_report *slot = &ring->reports[ring->head];
  slot->len = (uint8_t)len;
  memcpy(slot->data, data, len);

  ring->head = (ring->head + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
  ring->count++;

  if (dev->report_ready) {
    dev->report_ready(dev->report_ready_context);
  }
}

static bool virtio_input_report_ring_pop(struct virtio_input_report_ring *ring, struct virtio_input_report *out) {
  if (ring->count == 0) {
    return false;
  }

  const struct virtio_input_report *slot = &ring->reports[ring->tail];
  *out = *slot;

  ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
  ring->count--;
  return true;
}

static void virtio_input_emit_report(void *context, const uint8_t *report, size_t report_len) {
  struct virtio_input_device *dev = (struct virtio_input_device *)context;
  virtio_input_report_ring_push(dev, report, report_len);
}

void virtio_input_device_init(struct virtio_input_device *dev, virtio_input_report_ready_fn report_ready,
                              void *report_ready_context) {
  memset(dev, 0, sizeof(*dev));
  virtio_input_report_ring_init(&dev->report_ring);
  dev->report_ready = report_ready;
  dev->report_ready_context = report_ready_context;
  hid_translate_init(&dev->translate, virtio_input_emit_report, dev);
}

void virtio_input_device_reset_state(struct virtio_input_device *dev, bool emit_reports) {
  hid_translate_reset(&dev->translate, emit_reports);
}

void virtio_input_process_event_le(struct virtio_input_device *dev, const struct virtio_input_event_le *ev_le) {
  hid_translate_handle_event_le(&dev->translate, ev_le);
}

bool virtio_input_try_pop_report(struct virtio_input_device *dev, struct virtio_input_report *out_report) {
  return virtio_input_report_ring_pop(&dev->report_ring, out_report);
}


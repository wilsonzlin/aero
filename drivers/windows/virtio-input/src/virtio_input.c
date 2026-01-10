#include "virtio_input.h"

#include <string.h>

#ifdef _WIN32
static __forceinline PDEVICE_CONTEXT virtio_input_get_device_context(_In_ struct virtio_input_device *dev) {
  if (dev == NULL) {
    return NULL;
  }
  return CONTAINING_RECORD(dev, DEVICE_CONTEXT, InputDevice);
}

static __forceinline void virtio_input_diag_update_ring_depth(_Inout_ PDEVICE_CONTEXT ctx, _In_ uint32_t depth) {
  VioInputCounterSet(&ctx->Counters.ReportRingDepth, (LONG)depth);
  VioInputCounterMaxUpdate(&ctx->Counters.ReportRingMaxDepth, (LONG)depth);
}
#endif

static void virtio_input_report_ring_init(struct virtio_input_report_ring *ring) {
  memset(ring, 0, sizeof(*ring));
}

static void virtio_input_report_ring_push(struct virtio_input_device *dev, const uint8_t *data, size_t len) {
  struct virtio_input_report_ring *ring = &dev->report_ring;
#ifdef _WIN32
  PDEVICE_CONTEXT ctx = virtio_input_get_device_context(dev);
#endif
  if (len > VIRTIO_INPUT_REPORT_MAX_SIZE) {
#ifdef _WIN32
    if (ctx != NULL) {
      VioInputCounterInc(&ctx->Counters.ReportRingOverruns);
      VioInputCounterInc(&ctx->Counters.VirtioEventOverruns);
      VIOINPUT_LOG(
          VIOINPUT_LOG_ERROR | VIOINPUT_LOG_QUEUE,
          "report overrun: len=%Iu max=%u\n",
          len,
          (unsigned)VIRTIO_INPUT_REPORT_MAX_SIZE);
    }
#endif
    return;
  }

  /*
   * Input reports are stateful; dropping intermediate reports is typically
   * preferable to blocking when the consumer is slow. We deterministically
   * drop the oldest report when the ring is full.
   */
  if (ring->count == VIRTIO_INPUT_REPORT_RING_CAPACITY) {
#ifdef _WIN32
    if (ctx != NULL) {
      VioInputCounterInc(&ctx->Counters.ReportRingDrops);
      VioInputCounterInc(&ctx->Counters.VirtioEventDrops);
    }
#endif
    ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
    ring->count--;
  }

  struct virtio_input_report *slot = &ring->reports[ring->head];
  slot->len = (uint8_t)len;
  memcpy(slot->data, data, len);

  ring->head = (ring->head + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
  ring->count++;

#ifdef _WIN32
  if (ctx != NULL) {
    virtio_input_diag_update_ring_depth(ctx, ring->count);
  }
#endif

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
#ifdef _WIN32
  PDEVICE_CONTEXT ctx = virtio_input_get_device_context(dev);
  if (ctx != NULL) {
    VioInputCounterInc(&ctx->Counters.VirtioEvents);
    if (VioInputLogEnabled(VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ)) {
      VIOINPUT_LOG(
          VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
          "virtio event: type=%u code=%u value=%u events=%ld\n",
          (unsigned)ev_le->type,
          (unsigned)ev_le->code,
          (unsigned)ev_le->value,
          ctx->Counters.VirtioEvents);
    }
  }
#endif
  hid_translate_handle_event_le(&dev->translate, ev_le);
}

bool virtio_input_try_pop_report(struct virtio_input_device *dev, struct virtio_input_report *out_report) {
  bool ok = virtio_input_report_ring_pop(&dev->report_ring, out_report);
#ifdef _WIN32
  if (ok) {
    PDEVICE_CONTEXT ctx = virtio_input_get_device_context(dev);
    if (ctx != NULL) {
      virtio_input_diag_update_ring_depth(ctx, dev->report_ring.count);
    }
  }
#endif
  return ok;
}

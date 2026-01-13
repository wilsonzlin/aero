#include "../src/virtio_input.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

struct report_ready_counter {
  uint32_t calls;
};

static void report_ready_cb(void *context) {
  struct report_ready_counter *c = (struct report_ready_counter *)context;
  c->calls++;
}

static void make_report(uint8_t out[VIRTIO_INPUT_REPORT_MAX_SIZE], uint32_t seq) {
  out[0] = 0xA5; /* constant marker */
  out[1] = (uint8_t)(seq & 0xFFu);
  out[2] = (uint8_t)((seq >> 8) & 0xFFu);
  out[3] = (uint8_t)((seq >> 16) & 0xFFu);
  out[4] = (uint8_t)((seq >> 24) & 0xFFu);

  for (size_t i = 5; i < (size_t)VIRTIO_INPUT_REPORT_MAX_SIZE; i++) {
    out[i] = (uint8_t)(seq + (uint32_t)i);
  }
}

static void expect_report_seq(const struct virtio_input_report *r, uint32_t seq) {
  uint8_t expected[VIRTIO_INPUT_REPORT_MAX_SIZE];
  make_report(expected, seq);
  assert(r->len == (uint8_t)VIRTIO_INPUT_REPORT_MAX_SIZE);
  assert(memcmp(r->data, expected, (size_t)VIRTIO_INPUT_REPORT_MAX_SIZE) == 0);
}

static void test_report_ring_drop_oldest(void) {
  struct virtio_input_device dev;
  struct report_ready_counter ready = {0};

  virtio_input_device_init(&dev, report_ready_cb, &ready, NULL, NULL, NULL);

  /*
   * Push more than capacity to force drops and wrap-around. The ring should
   * retain the newest VIRTIO_INPUT_REPORT_RING_CAPACITY reports and pop them
   * oldest-to-newest within that retained window.
   */
  const uint32_t total_reports = (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY * 3u + 7u;
  for (uint32_t seq = 0; seq < total_reports; seq++) {
    uint8_t report[VIRTIO_INPUT_REPORT_MAX_SIZE];
    make_report(report, seq);
    dev.translate.emit_report(dev.translate.emit_report_context, report, sizeof(report));
  }

  assert(ready.calls == total_reports);
  assert(dev.report_ring.count == (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);

  const uint32_t first_retained = total_reports - (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY;

  uint32_t popped = 0;
  struct virtio_input_report out;
  while (virtio_input_try_pop_report(&dev, &out)) {
    expect_report_seq(&out, first_retained + popped);
    popped++;
  }

  assert(popped == (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);
  assert(dev.report_ring.count == 0);
}

int main(void) {
  test_report_ring_drop_oldest();
  printf("report_ring_test: ok\n");
  return 0;
}

/*
 * tests/run.sh builds each *_test.c as a standalone translation unit. Include the
 * portable virtio-input glue and its dependencies directly so this test can be
 * built without any special-casing in the runner.
 */
#include "../src/hid_translate.c"
#include "../src/virtio_input.c"

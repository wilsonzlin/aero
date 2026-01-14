// TEST_DEPS: virtio_input.c hid_translate.c

#include "../src/virtio_input.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

struct report_ready_counter {
  uint32_t calls;
};

static void report_ready_cb(void *context) {
  struct report_ready_counter *c = (struct report_ready_counter *)context;
  c->calls++;
}

static uint16_t to_le16(uint16_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return (uint16_t)((v >> 8) | (v << 8));
#else
  return v;
#endif
}

static uint32_t to_le32(uint32_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return ((v & 0x000000FFu) << 24) | ((v & 0x0000FF00u) << 8) | ((v & 0x00FF0000u) >> 8) | ((v & 0xFF000000u) >> 24);
#else
  return v;
#endif
}

static void send_abs(struct virtio_input_device *dev, uint16_t code, int32_t value) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_ABS);
  ev.code = to_le16(code);
  ev.value = to_le32((uint32_t)value);
  virtio_input_process_event_le(dev, &ev);
}

static void send_key(struct virtio_input_device *dev, uint16_t code, uint32_t value) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_KEY);
  ev.code = to_le16(code);
  ev.value = to_le32(value);
  virtio_input_process_event_le(dev, &ev);
}

static void send_syn(struct virtio_input_device *dev) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_SYN);
  ev.code = to_le16(VIRTIO_INPUT_SYN_REPORT);
  ev.value = to_le32(0);
  virtio_input_process_event_le(dev, &ev);
}

static void expect_tablet_report(const struct virtio_input_report *r, uint8_t buttons, uint16_t x, uint16_t y) {
  assert(r->len == HID_TRANSLATE_TABLET_REPORT_SIZE);
  assert(r->data[0] == HID_TRANSLATE_REPORT_ID_TABLET);
  assert(r->data[1] == buttons);
  assert(r->data[2] == (uint8_t)(x & 0xFFu));
  assert(r->data[3] == (uint8_t)((x >> 8) & 0xFFu));
  assert(r->data[4] == (uint8_t)(y & 0xFFu));
  assert(r->data[5] == (uint8_t)((y >> 8) & 0xFFu));
}

static void test_tablet_events_push_reports_to_ring(void) {
  struct virtio_input_device dev;
  struct report_ready_counter ready = {0};

  virtio_input_device_init(&dev, report_ready_cb, &ready, NULL, NULL, NULL);
  virtio_input_device_set_enabled_reports(&dev, HID_TRANSLATE_REPORT_MASK_TABLET);

  send_abs(&dev, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&dev, VIRTIO_INPUT_ABS_Y, 20);
  send_syn(&dev);

  assert(ready.calls == 1);
  struct virtio_input_report out;
  assert(virtio_input_try_pop_report(&dev, &out));
  expect_tablet_report(&out, 0x00, 10, 20);

  /* Same coordinates again should not emit (no change). */
  send_abs(&dev, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&dev, VIRTIO_INPUT_ABS_Y, 20);
  send_syn(&dev);
  assert(ready.calls == 1);
  assert(!virtio_input_try_pop_report(&dev, &out));

  /* Empty SYN should also not emit. */
  send_syn(&dev);
  assert(ready.calls == 1);
  assert(!virtio_input_try_pop_report(&dev, &out));
}

static void test_tablet_button_events_push_reports_to_ring(void) {
  struct virtio_input_device dev;
  struct report_ready_counter ready = {0};

  virtio_input_device_init(&dev, report_ready_cb, &ready, NULL, NULL, NULL);
  virtio_input_device_set_enabled_reports(&dev, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Establish a position, flush and discard report. */
  send_abs(&dev, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&dev, VIRTIO_INPUT_ABS_Y, 20);
  send_syn(&dev);
  struct virtio_input_report out;
  assert(virtio_input_try_pop_report(&dev, &out));
  expect_tablet_report(&out, 0x00, 10, 20);

  /* Touch down maps to Button 1 for tablet reports. */
  send_key(&dev, VIRTIO_INPUT_BTN_TOUCH, 1);
  send_syn(&dev);
  assert(virtio_input_try_pop_report(&dev, &out));
  expect_tablet_report(&out, 0x01, 10, 20);

  /* Touch up. */
  send_key(&dev, VIRTIO_INPUT_BTN_TOUCH, 0);
  send_syn(&dev);
  assert(virtio_input_try_pop_report(&dev, &out));
  expect_tablet_report(&out, 0x00, 10, 20);

  assert(ready.calls == 3);
  assert(!virtio_input_try_pop_report(&dev, &out));
}

int main(void) {
  test_tablet_events_push_reports_to_ring();
  test_tablet_button_events_push_reports_to_ring();
  printf("virtio_input_integration_test: ok\n");
  return 0;
}


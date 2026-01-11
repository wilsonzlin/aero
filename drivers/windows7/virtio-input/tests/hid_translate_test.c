#include "../src/hid_translate.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

enum { MAX_CAPTURED_REPORTS = 64 };

struct captured_reports {
  size_t count;
  size_t lens[MAX_CAPTURED_REPORTS];
  uint8_t bytes[MAX_CAPTURED_REPORTS][HID_TRANSLATE_KEYBOARD_REPORT_SIZE];
};

static void capture_emit(void *context, const uint8_t *report, size_t report_len) {
  struct captured_reports *cap = (struct captured_reports *)context;
  assert(cap->count < MAX_CAPTURED_REPORTS);
  assert(report_len <= HID_TRANSLATE_KEYBOARD_REPORT_SIZE);
  cap->lens[cap->count] = report_len;
  memcpy(cap->bytes[cap->count], report, report_len);
  cap->count++;
}

static void cap_clear(struct captured_reports *cap) {
  memset(cap, 0, sizeof(*cap));
}

static void send_key(struct hid_translate *t, uint16_t code, uint32_t value) {
  struct virtio_input_event ev;
  ev.type = VIRTIO_INPUT_EV_KEY;
  ev.code = code;
  ev.value = value;
  hid_translate_handle_event(t, &ev);
}

static void send_rel(struct hid_translate *t, uint16_t code, int32_t delta) {
  struct virtio_input_event ev;
  ev.type = VIRTIO_INPUT_EV_REL;
  ev.code = code;
  ev.value = (uint32_t)delta;
  hid_translate_handle_event(t, &ev);
}

static void send_syn(struct hid_translate *t) {
  struct virtio_input_event ev;
  ev.type = VIRTIO_INPUT_EV_SYN;
  ev.code = VIRTIO_INPUT_SYN_REPORT;
  ev.value = 0;
  hid_translate_handle_event(t, &ev);
}

static void expect_report(const struct captured_reports *cap, size_t idx, const uint8_t *expected, size_t len) {
  assert(idx < cap->count);
  assert(cap->lens[idx] == len);
  assert(memcmp(cap->bytes[idx], expected, len) == 0);
}

static void test_mapping(void) {
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_A) == 0x04);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_Z) == 0x1D);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_1) == 0x1E);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_0) == 0x27);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_ENTER) == 0x28);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_ESC) == 0x29);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_TAB) == 0x2B);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_SPACE) == 0x2C);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_CAPSLOCK) == 0x39);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_UP) == 0x52);

  /* Modifiers are handled as a bitmask, not returned as usages. */
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTSHIFT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTCTRL) == 0);
}

static void test_keyboard_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Press A, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x04, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Press LeftShift, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTSHIFT, 1);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x02, 0, 0x04, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Release A, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 0);
  send_syn(&t);

  uint8_t expect3[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x02, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Release LeftShift, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTSHIFT, 0);
  send_syn(&t);

  uint8_t expect4[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Repeat shouldn't create another report (state doesn't change). */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_syn(&t);
  assert(cap.count == 1);
  send_key(&t, VIRTIO_INPUT_KEY_A, 2);
  send_syn(&t);
  assert(cap.count == 1);
}

static void test_keyboard_overflow_queue(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Press 7 keys, flush once. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_key(&t, VIRTIO_INPUT_KEY_B, 1);
  send_key(&t, VIRTIO_INPUT_KEY_C, 1);
  send_key(&t, VIRTIO_INPUT_KEY_D, 1);
  send_key(&t, VIRTIO_INPUT_KEY_E, 1);
  send_key(&t, VIRTIO_INPUT_KEY_F, 1);
  send_key(&t, VIRTIO_INPUT_KEY_G, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Release B; queued G becomes visible in the 6-key array. */
  send_key(&t, VIRTIO_INPUT_KEY_B, 0);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x04, 0x06, 0x07, 0x08, 0x09, 0x0A};
  expect_report(&cap, 1, expect2, sizeof(expect2));
}

static void test_mouse_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Left button down. */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Move and wheel. */
  send_rel(&t, VIRTIO_INPUT_REL_X, 5);
  send_rel(&t, VIRTIO_INPUT_REL_Y, -3);
  send_rel(&t, VIRTIO_INPUT_REL_WHEEL, 1);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x05, 0xFD, 0x01};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Large delta is split into multiple reports. */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  send_rel(&t, VIRTIO_INPUT_REL_X, 200);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect3[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x7F, 0x00, 0x00};
  uint8_t expect4[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x49, 0x00, 0x00};
  expect_report(&cap, 0, expect3, sizeof(expect3));
  expect_report(&cap, 1, expect4, sizeof(expect4));
}

static void test_reset_emits_release_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_syn(&t);
  assert(cap.count == 2); /* keyboard + mouse */

  cap_clear(&cap);
  hid_translate_reset(&t, true);
  assert(cap.count == 2);

  uint8_t expect_kb[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  uint8_t expect_mouse[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0, 0, 0, 0};
  expect_report(&cap, 0, expect_kb, sizeof(expect_kb));
  expect_report(&cap, 1, expect_mouse, sizeof(expect_mouse));
}

static void test_keyboard_only_enable(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD);

  /* Mouse input is ignored. */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_rel(&t, VIRTIO_INPUT_REL_X, 5);
  send_syn(&t);
  assert(cap.count == 0);

  /* Keyboard input still emits. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_syn(&t);
  assert(cap.count == 1);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x04, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Reset emits only the enabled report types. */
  cap_clear(&cap);
  hid_translate_reset(&t, true);
  assert(cap.count == 1);
  uint8_t expect_release[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect_release, sizeof(expect_release));
}

static void test_mouse_only_enable(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Keyboard input is ignored. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_syn(&t);
  assert(cap.count == 0);

  /* Mouse input emits. */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_rel(&t, VIRTIO_INPUT_REL_X, 5);
  send_rel(&t, VIRTIO_INPUT_REL_Y, -3);
  send_syn(&t);
  assert(cap.count == 1);

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x05, 0xFD, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Reset emits only the enabled report types. */
  cap_clear(&cap);
  hid_translate_reset(&t, true);
  assert(cap.count == 1);
  uint8_t expect_release[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0, 0, 0, 0};
  expect_report(&cap, 0, expect_release, sizeof(expect_release));
}

int main(void) {
  test_mapping();
  test_keyboard_reports();
  test_keyboard_overflow_queue();
  test_mouse_reports();
  test_reset_emits_release_reports();
  test_keyboard_only_enable();
  test_mouse_only_enable();
  printf("hid_translate_test: ok\n");
  return 0;
}

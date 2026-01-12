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

static void test_linux_keycode_abi_values(void) {
  /*
   * The translator works on raw Linux input-event-codes.h values coming over the
   * virtio wire. If these enums drift from the Linux input ABI, the mapping
   * layer may still compile but keys will not work end-to-end.
   */
  assert(VIRTIO_INPUT_KEY_ESC == 1);
  assert(VIRTIO_INPUT_KEY_ENTER == 28);
  assert(VIRTIO_INPUT_KEY_A == 30);
  assert(VIRTIO_INPUT_KEY_Z == 44);
  assert(VIRTIO_INPUT_KEY_0 == 11);
  assert(VIRTIO_INPUT_KEY_9 == 10);
  assert(VIRTIO_INPUT_KEY_LEFTCTRL == 29);
  assert(VIRTIO_INPUT_KEY_RIGHTCTRL == 97);
  assert(VIRTIO_INPUT_KEY_LEFTSHIFT == 42);
  assert(VIRTIO_INPUT_KEY_RIGHTSHIFT == 54);
  assert(VIRTIO_INPUT_KEY_LEFTALT == 56);
  assert(VIRTIO_INPUT_KEY_RIGHTALT == 100);
  assert(VIRTIO_INPUT_KEY_CAPSLOCK == 58);
  assert(VIRTIO_INPUT_KEY_F1 == 59);
  assert(VIRTIO_INPUT_KEY_F2 == 60);
  assert(VIRTIO_INPUT_KEY_F3 == 61);
  assert(VIRTIO_INPUT_KEY_F4 == 62);
  assert(VIRTIO_INPUT_KEY_F5 == 63);
  assert(VIRTIO_INPUT_KEY_F6 == 64);
  assert(VIRTIO_INPUT_KEY_F7 == 65);
  assert(VIRTIO_INPUT_KEY_F8 == 66);
  assert(VIRTIO_INPUT_KEY_F9 == 67);
  assert(VIRTIO_INPUT_KEY_F10 == 68);
  assert(VIRTIO_INPUT_KEY_F11 == 87);
  assert(VIRTIO_INPUT_KEY_F12 == 88);
  assert(VIRTIO_INPUT_KEY_HOME == 102);
  assert(VIRTIO_INPUT_KEY_UP == 103);
  assert(VIRTIO_INPUT_KEY_PAGEUP == 104);
  assert(VIRTIO_INPUT_KEY_LEFT == 105);
  assert(VIRTIO_INPUT_KEY_RIGHT == 106);
  assert(VIRTIO_INPUT_KEY_END == 107);
  assert(VIRTIO_INPUT_KEY_DOWN == 108);
  assert(VIRTIO_INPUT_KEY_PAGEDOWN == 109);
  assert(VIRTIO_INPUT_KEY_INSERT == 110);
  assert(VIRTIO_INPUT_KEY_DELETE == 111);
  assert(VIRTIO_INPUT_KEY_LEFTMETA == 125);
  assert(VIRTIO_INPUT_KEY_RIGHTMETA == 126);
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
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F1) == 0x3A);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F2) == 0x3B);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F3) == 0x3C);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F4) == 0x3D);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F5) == 0x3E);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F6) == 0x3F);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F7) == 0x40);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F8) == 0x41);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F9) == 0x42);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F10) == 0x43);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F11) == 0x44);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_F12) == 0x45);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_INSERT) == 0x49);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_HOME) == 0x4A);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_PAGEUP) == 0x4B);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_DELETE) == 0x4C);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_END) == 0x4D);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_PAGEDOWN) == 0x4E);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHT) == 0x4F);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFT) == 0x50);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_DOWN) == 0x51);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_UP) == 0x52);

  /* Modifiers are handled as a bitmask, not returned as usages. */
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTCTRL) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTSHIFT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTALT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTMETA) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTCTRL) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTSHIFT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTALT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTMETA) == 0);
}

static void test_keyboard_modifier_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Press LeftCtrl, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTCTRL, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x01, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Press RightAlt, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTALT, 1);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x41, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Press LeftMeta, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTMETA, 1);
  send_syn(&t);

  uint8_t expect3[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x49, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Release RightAlt, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTALT, 0);
  send_syn(&t);

  uint8_t expect4[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x09, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Release LeftCtrl + LeftMeta, flush once. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTCTRL, 0);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTMETA, 0);
  send_syn(&t);

  uint8_t expect5[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 4, expect5, sizeof(expect5));
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

static void test_keyboard_function_key_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Press+release F1, flushing after each. */
  send_key(&t, VIRTIO_INPUT_KEY_F1, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x3A, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  send_key(&t, VIRTIO_INPUT_KEY_F1, 0);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Press+release F12, flushing after each. */
  send_key(&t, VIRTIO_INPUT_KEY_F12, 1);
  send_syn(&t);

  uint8_t expect3[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x45, 0, 0, 0, 0, 0};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  send_key(&t, VIRTIO_INPUT_KEY_F12, 0);
  send_syn(&t);

  uint8_t expect4[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 3, expect4, sizeof(expect4));
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
  test_linux_keycode_abi_values();
  test_mapping();
  test_keyboard_modifier_reports();
  test_keyboard_reports();
  test_keyboard_function_key_reports();
  test_keyboard_overflow_queue();
  test_mouse_reports();
  test_reset_emits_release_reports();
  test_keyboard_only_enable();
  test_mouse_only_enable();
  printf("hid_translate_test: ok\n");
  return 0;
}

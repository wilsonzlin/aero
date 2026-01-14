#include "../src/hid_translate.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

enum { MAX_CAPTURED_REPORTS = 64 };

struct captured_reports {
  size_t count;
  size_t lens[MAX_CAPTURED_REPORTS];
  uint8_t bytes[MAX_CAPTURED_REPORTS][HID_TRANSLATE_MAX_REPORT_SIZE];
};

static void capture_emit(void *context, const uint8_t *report, size_t report_len) {
  struct captured_reports *cap = (struct captured_reports *)context;
  assert(cap->count < MAX_CAPTURED_REPORTS);
  assert(report_len <= HID_TRANSLATE_MAX_REPORT_SIZE);
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

static void send_abs(struct hid_translate *t, uint16_t code, int32_t value) {
  struct virtio_input_event ev;
  ev.type = VIRTIO_INPUT_EV_ABS;
  ev.code = code;
  ev.value = (uint32_t)value;
  hid_translate_handle_event(t, &ev);
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

static void send_key_le(struct hid_translate *t, uint16_t code, uint32_t value) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_KEY);
  ev.code = to_le16(code);
  ev.value = to_le32(value);
  hid_translate_handle_event_le(t, &ev);
}

static void send_rel_le(struct hid_translate *t, uint16_t code, int32_t delta) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_REL);
  ev.code = to_le16(code);
  ev.value = to_le32((uint32_t)delta);
  hid_translate_handle_event_le(t, &ev);
}

static void send_abs_le(struct hid_translate *t, uint16_t code, int32_t value) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_ABS);
  ev.code = to_le16(code);
  ev.value = to_le32((uint32_t)value);
  hid_translate_handle_event_le(t, &ev);
}

static void send_syn_le(struct hid_translate *t) {
  struct virtio_input_event_le ev;
  ev.type = to_le16(VIRTIO_INPUT_EV_SYN);
  ev.code = to_le16(VIRTIO_INPUT_SYN_REPORT);
  ev.value = to_le32(0);
  hid_translate_handle_event_le(t, &ev);
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
  assert(VIRTIO_INPUT_KEY_BACKSPACE == 14);
  assert(VIRTIO_INPUT_KEY_TAB == 15);
  assert(VIRTIO_INPUT_KEY_SPACE == 57);
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
  assert(VIRTIO_INPUT_KEY_KPASTERISK == 55);
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
  assert(VIRTIO_INPUT_KEY_NUMLOCK == 69);
  assert(VIRTIO_INPUT_KEY_SCROLLLOCK == 70);
  assert(VIRTIO_INPUT_KEY_KP1 == 79);
  assert(VIRTIO_INPUT_KEY_KP0 == 82);
  assert(VIRTIO_INPUT_KEY_KPDOT == 83);
  assert(VIRTIO_INPUT_KEY_102ND == 86);
  assert(VIRTIO_INPUT_KEY_F11 == 87);
  assert(VIRTIO_INPUT_KEY_F12 == 88);
  assert(VIRTIO_INPUT_KEY_RO == 89);
  assert(VIRTIO_INPUT_KEY_KPENTER == 96);
  assert(VIRTIO_INPUT_KEY_KPSLASH == 98);
  assert(VIRTIO_INPUT_KEY_SYSRQ == 99);
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
  assert(VIRTIO_INPUT_KEY_KPEQUAL == 117);
  assert(VIRTIO_INPUT_KEY_PAUSE == 119);
  assert(VIRTIO_INPUT_KEY_KPCOMMA == 121);
  assert(VIRTIO_INPUT_KEY_YEN == 124);
  assert(VIRTIO_INPUT_KEY_LEFTMETA == 125);
  assert(VIRTIO_INPUT_KEY_RIGHTMETA == 126);
  assert(VIRTIO_INPUT_KEY_MENU == 139);

  /* Consumer/media keys. */
  assert(VIRTIO_INPUT_KEY_MUTE == 113);
  assert(VIRTIO_INPUT_KEY_VOLUMEDOWN == 114);
  assert(VIRTIO_INPUT_KEY_VOLUMEUP == 115);
  assert(VIRTIO_INPUT_KEY_NEXTSONG == 163);
  assert(VIRTIO_INPUT_KEY_PLAYPAUSE == 164);
  assert(VIRTIO_INPUT_KEY_PREVIOUSSONG == 165);
  assert(VIRTIO_INPUT_KEY_STOPCD == 166);

  /* Mouse buttons (Linux input-event-codes.h ABI). */
  assert(VIRTIO_INPUT_BTN_LEFT == 272);
  assert(VIRTIO_INPUT_BTN_RIGHT == 273);
  assert(VIRTIO_INPUT_BTN_MIDDLE == 274);
  assert(VIRTIO_INPUT_BTN_SIDE == 275);
  assert(VIRTIO_INPUT_BTN_EXTRA == 276);
  assert(VIRTIO_INPUT_BTN_FORWARD == 277);
  assert(VIRTIO_INPUT_BTN_BACK == 278);
  assert(VIRTIO_INPUT_BTN_TASK == 279);

  /* Relative axes (Linux input userspace ABI). */
  assert(VIRTIO_INPUT_REL_X == 0);
  assert(VIRTIO_INPUT_REL_Y == 1);
  assert(VIRTIO_INPUT_REL_HWHEEL == 6);
  assert(VIRTIO_INPUT_REL_WHEEL == 8);

  /* Tablet-related event and ABS codes (Linux input userspace ABI). */
  assert(VIRTIO_INPUT_EV_ABS == 0x03);
  assert(VIRTIO_INPUT_ABS_X == 0);
  assert(VIRTIO_INPUT_ABS_Y == 1);
}

static void test_linux_rel_code_abi_values(void) {
  assert(VIRTIO_INPUT_REL_X == 0);
  assert(VIRTIO_INPUT_REL_Y == 1);
  assert(VIRTIO_INPUT_REL_HWHEEL == 6);
  assert(VIRTIO_INPUT_REL_WHEEL == 8);
}

static void test_mapping(void) {
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_A) == 0x04);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_Z) == 0x1D);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_1) == 0x1E);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_0) == 0x27);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_ENTER) == 0x28);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_ESC) == 0x29);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_BACKSPACE) == 0x2A);
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
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_SYSRQ) == 0x46);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_SCROLLLOCK) == 0x47);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_PAUSE) == 0x48);
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
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_NUMLOCK) == 0x53);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPSLASH) == 0x54);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPASTERISK) == 0x55);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPMINUS) == 0x56);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPPLUS) == 0x57);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPENTER) == 0x58);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP1) == 0x59);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP2) == 0x5A);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP3) == 0x5B);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP4) == 0x5C);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP5) == 0x5D);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP6) == 0x5E);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP7) == 0x5F);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP8) == 0x60);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP9) == 0x61);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KP0) == 0x62);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPDOT) == 0x63);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_102ND) == 0x64);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_MENU) == 0x65);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPEQUAL) == 0x67);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_KPCOMMA) == 0x85);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RO) == 0x87);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_YEN) == 0x89);

  /* Modifiers are handled as a bitmask, not returned as usages. */
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTCTRL) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTSHIFT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTALT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_LEFTMETA) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTCTRL) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTSHIFT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTALT) == 0);
  assert(hid_translate_linux_key_to_hid_usage(VIRTIO_INPUT_KEY_RIGHTMETA) == 0);

  /* Unsupported keys should not map to any usage. */
  assert(hid_translate_linux_key_to_hid_usage(0) == 0);
}

static void test_keyboard_modifier_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

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

static void test_keyboard_all_modifier_bits_report(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Press all 8 modifiers, flush once. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTCTRL, 1);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTSHIFT, 1);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTALT, 1);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTMETA, 1);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTCTRL, 1);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTSHIFT, 1);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTALT, 1);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTMETA, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0xFF, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Release all 8 modifiers, flush once. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTCTRL, 0);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTSHIFT, 0);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTALT, 0);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTMETA, 0);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTCTRL, 0);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTSHIFT, 0);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTALT, 0);
  send_key(&t, VIRTIO_INPUT_KEY_RIGHTMETA, 0);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));
}

static void test_keyboard_ctrl_alt_delete_report(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Press Ctrl+Alt+Delete, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTCTRL, 1);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTALT, 1);
  send_key(&t, VIRTIO_INPUT_KEY_DELETE, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x05, 0, 0x4C, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Release Delete, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_DELETE, 0);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x05, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Release Ctrl+Alt, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_LEFTCTRL, 0);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTALT, 0);
  send_syn(&t);

  uint8_t expect3[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 2, expect3, sizeof(expect3));
}

static void test_keyboard_unsupported_key_ignored(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Linux KEY_RESERVED=0 is not mapped; should produce no report. */
  send_key(&t, 0, 1);
  send_syn(&t);
  assert(cap.count == 0);
}

static void test_keyboard_lock_keys_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* CapsLock */
  send_key(&t, VIRTIO_INPUT_KEY_CAPSLOCK, 1);
  send_syn(&t);
  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0x39, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  send_key(&t, VIRTIO_INPUT_KEY_CAPSLOCK, 0);
  send_syn(&t);
  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* NumLock */
  send_key(&t, VIRTIO_INPUT_KEY_NUMLOCK, 1);
  send_syn(&t);
  uint8_t expect3[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0x53, 0, 0, 0, 0, 0};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  send_key(&t, VIRTIO_INPUT_KEY_NUMLOCK, 0);
  send_syn(&t);
  uint8_t expect4[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* ScrollLock */
  send_key(&t, VIRTIO_INPUT_KEY_SCROLLLOCK, 1);
  send_syn(&t);
  uint8_t expect5[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0x47, 0, 0, 0, 0, 0};
  expect_report(&cap, 4, expect5, sizeof(expect5));

  send_key(&t, VIRTIO_INPUT_KEY_SCROLLLOCK, 0);
  send_syn(&t);
  uint8_t expect6[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0x00, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 5, expect6, sizeof(expect6));
}

static void test_keyboard_repeat_does_not_emit(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  /* Repeat for a normal key in the 6-key array (F1). */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_key(&t, VIRTIO_INPUT_KEY_F1, 1);
  send_syn(&t);
  assert(cap.count == 1);
  send_key(&t, VIRTIO_INPUT_KEY_F1, 2);
  send_syn(&t);
  assert(cap.count == 1);

  /* Repeat for a modifier key (LeftShift). */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTSHIFT, 1);
  send_syn(&t);
  assert(cap.count == 1);
  send_key(&t, VIRTIO_INPUT_KEY_LEFTSHIFT, 2);
  send_syn(&t);
  assert(cap.count == 1);
}

static void test_keyboard_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

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
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_syn(&t);
  assert(cap.count == 1);
  send_key(&t, VIRTIO_INPUT_KEY_A, 2);
  send_syn(&t);
  assert(cap.count == 1);
}

static void test_keyboard_function_key_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

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

static void test_keyboard_function_key_reports_le(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Press+release F12, delivered in little-endian wire format. */
  send_key_le(&t, VIRTIO_INPUT_KEY_F12, 1);
  send_syn_le(&t);

  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x45, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  send_key_le(&t, VIRTIO_INPUT_KEY_F12, 0);
  send_syn_le(&t);

  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));
}

static void test_keyboard_keypad_and_misc_key_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* PrintScreen (Linux KEY_SYSRQ). */
  send_key(&t, VIRTIO_INPUT_KEY_SYSRQ, 1);
  send_syn(&t);
  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x46, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  send_key(&t, VIRTIO_INPUT_KEY_SYSRQ, 0);
  send_syn(&t);
  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Keypad 1. */
  send_key(&t, VIRTIO_INPUT_KEY_KP1, 1);
  send_syn(&t);
  uint8_t expect3[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x59, 0, 0, 0, 0, 0};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  send_key(&t, VIRTIO_INPUT_KEY_KP1, 0);
  send_syn(&t);
  uint8_t expect4[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Menu / Application key. */
  send_key(&t, VIRTIO_INPUT_KEY_MENU, 1);
  send_syn(&t);
  uint8_t expect5[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x65, 0, 0, 0, 0, 0};
  expect_report(&cap, 4, expect5, sizeof(expect5));

  send_key(&t, VIRTIO_INPUT_KEY_MENU, 0);
  send_syn(&t);
  uint8_t expect6[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 5, expect6, sizeof(expect6));

  /* Keypad '=' (non-boot usage range). */
  send_key(&t, VIRTIO_INPUT_KEY_KPEQUAL, 1);
  send_syn(&t);
  uint8_t expect7[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x67, 0, 0, 0, 0, 0};
  expect_report(&cap, 6, expect7, sizeof(expect7));

  send_key(&t, VIRTIO_INPUT_KEY_KPEQUAL, 0);
  send_syn(&t);
  uint8_t expect8[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 7, expect8, sizeof(expect8));

  /* Keypad ',' (non-boot usage range). */
  send_key(&t, VIRTIO_INPUT_KEY_KPCOMMA, 1);
  send_syn(&t);
  uint8_t expect9[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x85, 0, 0, 0, 0, 0};
  expect_report(&cap, 8, expect9, sizeof(expect9));

  send_key(&t, VIRTIO_INPUT_KEY_KPCOMMA, 0);
  send_syn(&t);
  uint8_t expect10[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 9, expect10, sizeof(expect10));

  /* IntlRo (non-boot usage range). */
  send_key(&t, VIRTIO_INPUT_KEY_RO, 1);
  send_syn(&t);
  uint8_t expect11[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x87, 0, 0, 0, 0, 0};
  expect_report(&cap, 10, expect11, sizeof(expect11));

  send_key(&t, VIRTIO_INPUT_KEY_RO, 0);
  send_syn(&t);
  uint8_t expect12[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 11, expect12, sizeof(expect12));

  /* IntlYen (non-boot usage range). */
  send_key(&t, VIRTIO_INPUT_KEY_YEN, 1);
  send_syn(&t);
  uint8_t expect13[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x89, 0, 0, 0, 0, 0};
  expect_report(&cap, 12, expect13, sizeof(expect13));

  send_key(&t, VIRTIO_INPUT_KEY_YEN, 0);
  send_syn(&t);
  uint8_t expect14[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  expect_report(&cap, 13, expect14, sizeof(expect14));
}

static void test_mouse_reports_le(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Left button down. */
  send_key_le(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_syn_le(&t);

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Move and wheel. */
  send_rel_le(&t, VIRTIO_INPUT_REL_X, 5);
  send_rel_le(&t, VIRTIO_INPUT_REL_Y, -3);
  send_rel_le(&t, VIRTIO_INPUT_REL_WHEEL, 1);
  send_rel_le(&t, VIRTIO_INPUT_REL_HWHEEL, -2);
  send_syn_le(&t);

  uint8_t expect2[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x05, 0xFD, 0x01, 0xFE};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Side/back button down. */
  send_key_le(&t, VIRTIO_INPUT_BTN_SIDE, 1);
  send_syn_le(&t);
  uint8_t expect3[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x09, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Extra/forward button down. */
  send_key_le(&t, VIRTIO_INPUT_BTN_EXTRA, 1);
  send_syn_le(&t);
  uint8_t expect4[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x19, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Additional buttons (6..8). */
  send_key_le(&t, VIRTIO_INPUT_BTN_FORWARD, 1);
  send_syn_le(&t);
  uint8_t expect_forward_down[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x39, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 4, expect_forward_down, sizeof(expect_forward_down));

  send_key_le(&t, VIRTIO_INPUT_BTN_BACK, 1);
  send_syn_le(&t);
  uint8_t expect_back_down[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x79, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 5, expect_back_down, sizeof(expect_back_down));

  send_key_le(&t, VIRTIO_INPUT_BTN_TASK, 1);
  send_syn_le(&t);
  uint8_t expect_task_down[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0xF9, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 6, expect_task_down, sizeof(expect_task_down));

  /* Release in reverse order. */
  send_key_le(&t, VIRTIO_INPUT_BTN_FORWARD, 0);
  send_syn_le(&t);
  uint8_t expect_forward_up[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0xD9, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 7, expect_forward_up, sizeof(expect_forward_up));

  send_key_le(&t, VIRTIO_INPUT_BTN_BACK, 0);
  send_syn_le(&t);
  uint8_t expect_back_up[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x99, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 8, expect_back_up, sizeof(expect_back_up));

  send_key_le(&t, VIRTIO_INPUT_BTN_TASK, 0);
  send_syn_le(&t);
  uint8_t expect_task_up[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x19, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 9, expect_task_up, sizeof(expect_task_up));
}

static void test_mouse_buttons_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Right button down. */
  send_key(&t, VIRTIO_INPUT_BTN_RIGHT, 1);
  send_syn(&t);
  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x02, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Right button up. */
  send_key(&t, VIRTIO_INPUT_BTN_RIGHT, 0);
  send_syn(&t);
  uint8_t expect2[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Middle button down. */
  send_key(&t, VIRTIO_INPUT_BTN_MIDDLE, 1);
  send_syn(&t);
  uint8_t expect3[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x04, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Middle button up. */
  send_key(&t, VIRTIO_INPUT_BTN_MIDDLE, 0);
  send_syn(&t);
  uint8_t expect4[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Left+right+middle down (all at once before SYN). */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_key(&t, VIRTIO_INPUT_BTN_RIGHT, 1);
  send_key(&t, VIRTIO_INPUT_BTN_MIDDLE, 1);
  send_syn(&t);
  uint8_t expect5[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x07, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 4, expect5, sizeof(expect5));

  /* Release buttons and ensure bitmask tracks state. */
  send_key(&t, VIRTIO_INPUT_BTN_RIGHT, 0);
  send_syn(&t);
  uint8_t expect6[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x05, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 5, expect6, sizeof(expect6));

  send_key(&t, VIRTIO_INPUT_BTN_MIDDLE, 0);
  send_syn(&t);
  uint8_t expect7[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 6, expect7, sizeof(expect7));

  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 0);
  send_syn(&t);
  uint8_t expect8[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 7, expect8, sizeof(expect8));
}

static void test_mouse_buttons_reports_le(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Right button down (LE wire format). */
  send_key_le(&t, VIRTIO_INPUT_BTN_RIGHT, 1);
  send_syn_le(&t);
  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x02, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Right button up. */
  send_key_le(&t, VIRTIO_INPUT_BTN_RIGHT, 0);
  send_syn_le(&t);
  uint8_t expect2[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Middle button down. */
  send_key_le(&t, VIRTIO_INPUT_BTN_MIDDLE, 1);
  send_syn_le(&t);
  uint8_t expect3[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x04, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Middle button up. */
  send_key_le(&t, VIRTIO_INPUT_BTN_MIDDLE, 0);
  send_syn_le(&t);
  uint8_t expect4[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Left+right+middle down (all at once before SYN). */
  send_key_le(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_key_le(&t, VIRTIO_INPUT_BTN_RIGHT, 1);
  send_key_le(&t, VIRTIO_INPUT_BTN_MIDDLE, 1);
  send_syn_le(&t);
  uint8_t expect5[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x07, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 4, expect5, sizeof(expect5));

  /* Release buttons and ensure bitmask tracks state. */
  send_key_le(&t, VIRTIO_INPUT_BTN_RIGHT, 0);
  send_syn_le(&t);
  uint8_t expect6[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x05, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 5, expect6, sizeof(expect6));

  send_key_le(&t, VIRTIO_INPUT_BTN_MIDDLE, 0);
  send_syn_le(&t);
  uint8_t expect7[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 6, expect7, sizeof(expect7));

  send_key_le(&t, VIRTIO_INPUT_BTN_LEFT, 0);
  send_syn_le(&t);
  uint8_t expect8[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 7, expect8, sizeof(expect8));
}

static void test_mouse_wheel_and_hwheel_one_syn(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  send_rel(&t, VIRTIO_INPUT_REL_WHEEL, 2);
  send_rel(&t, VIRTIO_INPUT_REL_HWHEEL, -1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x02, 0xFF};
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_keyboard_overflow_queue(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

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

static void test_keyboard_overflow_queue_does_not_emit_on_queued_press(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Press 6 keys, flush. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_key(&t, VIRTIO_INPUT_KEY_B, 1);
  send_key(&t, VIRTIO_INPUT_KEY_C, 1);
  send_key(&t, VIRTIO_INPUT_KEY_D, 1);
  send_key(&t, VIRTIO_INPUT_KEY_E, 1);
  send_key(&t, VIRTIO_INPUT_KEY_F, 1);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Press a 7th key; it is queued (not visible) so no new report should emit. */
  send_key(&t, VIRTIO_INPUT_KEY_G, 1);
  send_syn(&t);
  assert(cap.count == 1);

  /* Release B; queued G becomes visible and now a report should emit. */
  send_key(&t, VIRTIO_INPUT_KEY_B, 0);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect2[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0x04, 0x06, 0x07, 0x08, 0x09, 0x0A};
  expect_report(&cap, 1, expect2, sizeof(expect2));
}

static void test_mouse_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Left button down. */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Move and wheel. */
  send_rel(&t, VIRTIO_INPUT_REL_X, 5);
  send_rel(&t, VIRTIO_INPUT_REL_Y, -3);
  send_rel(&t, VIRTIO_INPUT_REL_WHEEL, 1);
  send_rel(&t, VIRTIO_INPUT_REL_HWHEEL, -2);
  send_syn(&t);

  uint8_t expect2[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x05, 0xFD, 0x01, 0xFE};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Side/back button down. */
  send_key(&t, VIRTIO_INPUT_BTN_SIDE, 1);
  send_syn(&t);
  uint8_t expect3[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x09, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Extra/forward button down. */
  send_key(&t, VIRTIO_INPUT_BTN_EXTRA, 1);
  send_syn(&t);
  uint8_t expect4[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x19, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Additional buttons (6..8). */
  send_key(&t, VIRTIO_INPUT_BTN_FORWARD, 1);
  send_syn(&t);
  uint8_t expect_forward_down[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x39, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 4, expect_forward_down, sizeof(expect_forward_down));

  send_key(&t, VIRTIO_INPUT_BTN_BACK, 1);
  send_syn(&t);
  uint8_t expect_back_down[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x79, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 5, expect_back_down, sizeof(expect_back_down));

  send_key(&t, VIRTIO_INPUT_BTN_TASK, 1);
  send_syn(&t);
  uint8_t expect_task_down[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0xF9, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 6, expect_task_down, sizeof(expect_task_down));

  /* Release in reverse order. */
  send_key(&t, VIRTIO_INPUT_BTN_FORWARD, 0);
  send_syn(&t);
  uint8_t expect_forward_up[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0xD9, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 7, expect_forward_up, sizeof(expect_forward_up));

  send_key(&t, VIRTIO_INPUT_BTN_BACK, 0);
  send_syn(&t);
  uint8_t expect_back_up[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x99, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 8, expect_back_up, sizeof(expect_back_up));

  send_key(&t, VIRTIO_INPUT_BTN_TASK, 0);
  send_syn(&t);
  uint8_t expect_task_up[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x19, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 9, expect_task_up, sizeof(expect_task_up));

  /* Large delta is split into multiple reports. */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_rel(&t, VIRTIO_INPUT_REL_X, 200);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect5[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x7F, 0x00, 0x00, 0x00};
  uint8_t expect6[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x49, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect5, sizeof(expect5));
  expect_report(&cap, 1, expect6, sizeof(expect6));

  /* Large negative delta is split into multiple reports. */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_rel(&t, VIRTIO_INPUT_REL_X, -200);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect7[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x81, 0x00, 0x00, 0x00};
  uint8_t expect8[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0xB7, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect7, sizeof(expect7));
  expect_report(&cap, 1, expect8, sizeof(expect8));

  /* Negative wheel delta is encoded as two's complement. */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_rel(&t, VIRTIO_INPUT_REL_WHEEL, -1);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect9[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0xFF, 0x00};
  expect_report(&cap, 0, expect9, sizeof(expect9));
  /* Large horizontal wheel delta is split into multiple reports. */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_rel(&t, VIRTIO_INPUT_REL_HWHEEL, -200);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect10[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x81};
  uint8_t expect11[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0xB7};
  expect_report(&cap, 0, expect10, sizeof(expect10));
  expect_report(&cap, 1, expect11, sizeof(expect11));
}

static void test_mouse_hwheel_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Horizontal wheel delta alone. */
  send_rel(&t, VIRTIO_INPUT_REL_HWHEEL, 5);
  send_syn(&t);
  assert(cap.count == 1);

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x05};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Coalesces with X/Y/Wheel on a single SYN_REPORT. */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_rel(&t, VIRTIO_INPUT_REL_X, 1);
  send_rel(&t, VIRTIO_INPUT_REL_Y, 2);
  send_rel(&t, VIRTIO_INPUT_REL_WHEEL, 3);
  send_rel(&t, VIRTIO_INPUT_REL_HWHEEL, 4);
  send_syn(&t);
  assert(cap.count == 1);

  uint8_t expect2[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x01, 0x02, 0x03, 0x04};
  expect_report(&cap, 0, expect2, sizeof(expect2));

  /* Large delta is split into multiple reports (same policy as REL_X/Y/WHEEL). */
  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_MOUSE);
  send_rel(&t, VIRTIO_INPUT_REL_HWHEEL, 200);
  send_syn(&t);
  assert(cap.count == 2);

  uint8_t expect3[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x7F};
  uint8_t expect4[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x00, 0x00, 0x00, 0x00, 0x49};
  expect_report(&cap, 0, expect3, sizeof(expect3));
  expect_report(&cap, 1, expect4, sizeof(expect4));
}

static void test_consumer_control_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Volume Up */
  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEUP, 1);
  send_syn(&t);
  uint8_t expect1[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x04};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEUP, 0);
  send_syn(&t);
  uint8_t expect2[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x00};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Mute + Volume Down together. */
  send_key(&t, VIRTIO_INPUT_KEY_MUTE, 1);
  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEDOWN, 1);
  send_syn(&t);
  uint8_t expect3[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x03};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Release Mute only. */
  send_key(&t, VIRTIO_INPUT_KEY_MUTE, 0);
  send_syn(&t);
  uint8_t expect4[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x02};
  expect_report(&cap, 3, expect4, sizeof(expect4));

  /* Release Volume Down. */
  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEDOWN, 0);
  send_syn(&t);
  uint8_t expect5[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x00};
  expect_report(&cap, 4, expect5, sizeof(expect5));

  /* Transport controls (Play/Pause, Next, Previous, Stop). */
  send_key(&t, VIRTIO_INPUT_KEY_PLAYPAUSE, 1);
  send_key(&t, VIRTIO_INPUT_KEY_NEXTSONG, 1);
  send_key(&t, VIRTIO_INPUT_KEY_PREVIOUSSONG, 1);
  send_key(&t, VIRTIO_INPUT_KEY_STOPCD, 1);
  send_syn(&t);
  uint8_t expect6[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x78};
  expect_report(&cap, 5, expect6, sizeof(expect6));

  /* Release all transport controls. */
  send_key(&t, VIRTIO_INPUT_KEY_PLAYPAUSE, 0);
  send_key(&t, VIRTIO_INPUT_KEY_NEXTSONG, 0);
  send_key(&t, VIRTIO_INPUT_KEY_PREVIOUSSONG, 0);
  send_key(&t, VIRTIO_INPUT_KEY_STOPCD, 0);
  send_syn(&t);
  uint8_t expect7[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0x00};
  expect_report(&cap, 6, expect7, sizeof(expect7));
}

static void test_consumer_control_disabled_does_not_emit(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Disable consumer-control output. */
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE);

  /* Consumer key events should be ignored (no consumer report emission). */
  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEUP, 1);
  send_syn(&t);
  assert(cap.count == 0);
}

static void test_reset_emits_release_reports(void) {
  struct captured_reports cap = {0};
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_CONSUMER | HID_TRANSLATE_REPORT_MASK_MOUSE);

  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_syn(&t);
  assert(cap.count == 2); /* keyboard + mouse */

  cap_clear(&cap);
  hid_translate_reset(&t, true);
  assert(cap.count == 3);

  uint8_t expect_kb[HID_TRANSLATE_KEYBOARD_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0};
  uint8_t expect_cc[HID_TRANSLATE_CONSUMER_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_CONSUMER, 0};
  uint8_t expect_mouse[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect_kb, sizeof(expect_kb));
  expect_report(&cap, 1, expect_cc, sizeof(expect_cc));
  expect_report(&cap, 2, expect_mouse, sizeof(expect_mouse));
}

static void test_reset_without_emit_reports_does_not_emit(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_ALL);

  /* Seed dirty state across all report types. */
  send_key(&t, VIRTIO_INPUT_KEY_A, 1);
  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEUP, 1);
  send_rel(&t, VIRTIO_INPUT_REL_X, 5);
  send_abs(&t, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 20);
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  assert(cap.count == 0);

  hid_translate_reset(&t, false);
  assert(cap.count == 0);

  /*
   * After reset, release events should be ignored (state already cleared), and
   * a SYN_REPORT should not emit anything.
   */
  send_key(&t, VIRTIO_INPUT_KEY_A, 0);
  send_key(&t, VIRTIO_INPUT_KEY_VOLUMEUP, 0);
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 0);
  send_syn(&t);
  assert(cap.count == 0);
}

static void test_keyboard_only_enable(void) {
  struct captured_reports cap = {0};
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
  struct captured_reports cap = {0};
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

  uint8_t expect1[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0x01, 0x05, 0xFD, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Reset emits only the enabled report types. */
  cap_clear(&cap);
  hid_translate_reset(&t, true);
  assert(cap.count == 1);
  uint8_t expect_release[HID_TRANSLATE_MOUSE_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_MOUSE, 0, 0, 0, 0, 0};
  expect_report(&cap, 0, expect_release, sizeof(expect_release));
}

static void test_tablet_abs_ignored_when_tablet_report_disabled(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);

  /* Explicitly disable tablet output. */
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE | HID_TRANSLATE_REPORT_MASK_CONSUMER);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 20);
  send_syn(&t);

  assert(cap.count == 0);
}

static void test_tablet_basic_abs_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /*
   * X/Y updates should not emit until SYN_REPORT.
   */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 100);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 200);
  assert(cap.count == 0);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0x64,
      0x00,
      0xC8,
      0x00,
  };
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_basic_abs_reports_le(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Delivered in little-endian wire format. */
  send_abs_le(&t, VIRTIO_INPUT_ABS_X, 0x1234);
  send_abs_le(&t, VIRTIO_INPUT_ABS_Y, 0x5678);
  send_syn_le(&t);

  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0x34,
      0x12,
      0x78,
      0x56,
  };
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_clamp_min_max(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Below-min values should clamp to 0. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, -123);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, -1);
  send_syn(&t);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0x00,
      0x00,
      0x00,
      0x00,
  };
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Exact max should map to max. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 0x7FFF);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 0x7FFF);
  send_syn(&t);
  uint8_t expect2[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0xFF,
      0x7F,
      0xFF,
      0x7F,
  };
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Above-max values should clamp to max. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 0x7FFF + 1);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 0x7FFF + 100);
  send_syn(&t);
  expect_report(&cap, 2, expect2, sizeof(expect2));
}

static void test_tablet_button_press_release(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Establish a position first. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 20);
  send_syn(&t);

  uint8_t expect_pos[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0x0A,
      0x00,
      0x14,
      0x00,
  };
  expect_report(&cap, 0, expect_pos, sizeof(expect_pos));

  /* Left button down. */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 1);
  send_syn(&t);

  uint8_t expect_down[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x01,
      0x0A,
      0x00,
      0x14,
      0x00,
  };
  expect_report(&cap, 1, expect_down, sizeof(expect_down));

  /* Left button up. */
  send_key(&t, VIRTIO_INPUT_BTN_LEFT, 0);
  send_syn(&t);

  uint8_t expect_up[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0x0A,
      0x00,
      0x14,
      0x00,
  };
  expect_report(&cap, 2, expect_up, sizeof(expect_up));
}

static void test_tablet_multiple_abs_updates_before_syn_is_deterministic(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 1);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 2);
  send_abs(&t, VIRTIO_INPUT_ABS_X, 3);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 4);
  send_abs(&t, VIRTIO_INPUT_ABS_X, 5);
  assert(cap.count == 0);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {
      HID_TRANSLATE_REPORT_ID_TABLET,
      0x00,
      0x05,
      0x00,
      0x04,
      0x00,
  };
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_scaling_reports(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);
  hid_translate_set_tablet_abs_range(&t, 0, 1000, 0, 500);

  /* Touch down at the middle of the range, flush. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 500);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 250);
  send_key(&t, VIRTIO_INPUT_BTN_TOUCH, 1);
  send_syn(&t);

  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x01, 0x00, 0x40, 0x00, 0x40};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Clamp beyond max, flush. */
  send_abs_le(&t, VIRTIO_INPUT_ABS_X, 2000);
  send_abs_le(&t, VIRTIO_INPUT_ABS_Y, -100);
  send_key_le(&t, VIRTIO_INPUT_BTN_TOUCH, 0);
  send_syn_le(&t);

  uint8_t expect2[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0xFF, 0x7F, 0x00, 0x00};
  expect_report(&cap, 1, expect2, sizeof(expect2));
}

static void test_tablet_scaling_rounds_to_nearest(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /*
   * Use a tiny device range where rounding behavior is visible.
   *
   * Expected mapping for v=1 in range [0, 2] with out_max=32767:
   *   scaled = (1 * 32767 + (2/2)) / 2 = 16384 (0x4000)
   */
  hid_translate_set_tablet_abs_range(&t, 0, 2, 0, 2);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 1);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 1);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x40, 0x00, 0x40};
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_scaling_with_negative_device_min(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /*
   * Cover scaling when the device range has a negative minimum (offset). Use a
   * non-symmetric range so "0" does not land at the midpoint.
   *
   * For v=0 with range [-50, 150] and out_max=32767:
   *   scaled = ((0 - (-50)) * 32767 + (200/2)) / 200
   *          = (50*32767 + 100) / 200
   *          = 8192 (0x2000)
   */
  hid_translate_set_tablet_abs_range(&t, -50, 150, -50, 150);

  /* Value inside range. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 0);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 0);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x20, 0x00, 0x20};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Exact min maps to 0. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, -50);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, -50);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect2[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Exact max maps to max. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 150);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 150);
  send_syn(&t);

  assert(cap.count == 3);
  uint8_t expect3[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0xFF, 0x7F, 0xFF, 0x7F};
  expect_report(&cap, 2, expect3, sizeof(expect3));

  /* Values outside range should clamp before scaling. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, -100);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 200);
  send_syn(&t);

  assert(cap.count == 4);
  uint8_t expect4[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x00, 0xFF, 0x7F};
  expect_report(&cap, 3, expect4, sizeof(expect4));
}

static void test_tablet_scaling_min_equals_max_maps_to_zero(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Degenerate range: should not divide by zero; map to 0 deterministically. */
  hid_translate_set_tablet_abs_range(&t, 5, 5, 7, 7);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 123);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, -456);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_abs_no_change_does_not_emit(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 1000);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 2000);
  send_syn(&t);
  assert(cap.count == 1);

  /* Sending the same coordinates again should not emit a duplicate report. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 1000);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 2000);
  send_syn(&t);
  assert(cap.count == 1);

  /* No events at all should also not emit. */
  send_syn(&t);
  assert(cap.count == 1);
}

static void test_tablet_abs_range_swaps_min_max(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Intentionally pass inverted min/max; API should normalize it. */
  hid_translate_set_tablet_abs_range(&t, 1000, 0, 500, 0);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 500);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 250);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x40, 0x00, 0x40};
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_partial_axis_updates_use_last_value(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Establish an initial position. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 100);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 200);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x64, 0x00, 0xC8, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));

  /* Update X only; Y should retain the previous value. */
  send_abs(&t, VIRTIO_INPUT_ABS_X, 300);
  send_syn(&t);

  assert(cap.count == 2);
  uint8_t expect2[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x2C, 0x01, 0xC8, 0x00};
  expect_report(&cap, 1, expect2, sizeof(expect2));

  /* Update Y only; X should retain the previous value. */
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 400);
  send_syn(&t);

  assert(cap.count == 3);
  uint8_t expect3[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x2C, 0x01, 0x90, 0x01};
  expect_report(&cap, 2, expect3, sizeof(expect3));
}

static void test_tablet_reset_without_xy_does_not_emit(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /* Without ever observing an X/Y pair, reset should not emit a spurious report. */
  hid_translate_reset(&t, true);
  assert(cap.count == 0);
}

static void test_tablet_reset_emits_release_without_xy_when_button_pressed(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  /*
   * Press a tablet button without setting a position; calling reset should still
   * emit a release report so the HID stacks don't latch the button state.
   */
  send_key(&t, VIRTIO_INPUT_BTN_TOUCH, 1);
  assert(cap.count == 0);

  hid_translate_reset(&t, true);
  assert(cap.count == 1);

  uint8_t expect1[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x00, 0x00, 0x00, 0x00};
  expect_report(&cap, 0, expect1, sizeof(expect1));
}

static void test_tablet_reset_emits_release_with_xy_when_button_pressed(void) {
  struct captured_reports cap;
  struct hid_translate t;

  cap_clear(&cap);
  hid_translate_init(&t, capture_emit, &cap);
  hid_translate_set_enabled_reports(&t, HID_TRANSLATE_REPORT_MASK_TABLET);

  send_abs(&t, VIRTIO_INPUT_ABS_X, 10);
  send_abs(&t, VIRTIO_INPUT_ABS_Y, 20);
  send_key(&t, VIRTIO_INPUT_BTN_TOUCH, 1);
  send_syn(&t);

  assert(cap.count == 1);
  uint8_t expect_down[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x01, 0x0A, 0x00, 0x14, 0x00};
  expect_report(&cap, 0, expect_down, sizeof(expect_down));

  hid_translate_reset(&t, true);

  assert(cap.count == 2);
  uint8_t expect_up[HID_TRANSLATE_TABLET_REPORT_SIZE] = {HID_TRANSLATE_REPORT_ID_TABLET, 0x00, 0x0A, 0x00, 0x14, 0x00};
  expect_report(&cap, 1, expect_up, sizeof(expect_up));
}

int main(void) {
  test_linux_keycode_abi_values();
  test_linux_rel_code_abi_values();
  test_mapping();
  test_keyboard_modifier_reports();
  test_keyboard_all_modifier_bits_report();
  test_keyboard_ctrl_alt_delete_report();
  test_keyboard_unsupported_key_ignored();
  test_keyboard_lock_keys_reports();
  test_keyboard_repeat_does_not_emit();
  test_keyboard_reports();
  test_keyboard_function_key_reports();
  test_keyboard_function_key_reports_le();
  test_keyboard_keypad_and_misc_key_reports();
  test_keyboard_overflow_queue();
  test_keyboard_overflow_queue_does_not_emit_on_queued_press();
  test_mouse_reports();
  test_mouse_reports_le();
  test_mouse_hwheel_reports();
  test_mouse_wheel_and_hwheel_one_syn();
  test_mouse_buttons_reports();
  test_mouse_buttons_reports_le();
  test_consumer_control_reports();
  test_consumer_control_disabled_does_not_emit();
  test_reset_emits_release_reports();
  test_reset_without_emit_reports_does_not_emit();
  test_keyboard_only_enable();
  test_mouse_only_enable();
  test_tablet_abs_ignored_when_tablet_report_disabled();
  test_tablet_basic_abs_reports();
  test_tablet_basic_abs_reports_le();
  test_tablet_clamp_min_max();
  test_tablet_button_press_release();
  test_tablet_multiple_abs_updates_before_syn_is_deterministic();
  test_tablet_scaling_reports();
  test_tablet_scaling_rounds_to_nearest();
  test_tablet_scaling_with_negative_device_min();
  test_tablet_scaling_min_equals_max_maps_to_zero();
  test_tablet_abs_no_change_does_not_emit();
  test_tablet_abs_range_swaps_min_max();
  test_tablet_partial_axis_updates_use_last_value();
  test_tablet_reset_without_xy_does_not_emit();
  test_tablet_reset_emits_release_without_xy_when_button_pressed();
  test_tablet_reset_emits_release_with_xy_when_button_pressed();
  printf("hid_translate_test: ok\n");
  return 0;
}

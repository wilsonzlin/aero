#pragma once

/*
 * Virtio-input (Linux evdev-style) -> HID report translation.
 *
 * This module is intentionally self-contained and written in portable C so it
 * can be unit-tested on the host while also being usable from the Win7 KMDF
 * minidriver.
 *
 * Report formats (must match the driver's HID report descriptor):
 *   - ReportID 1: Keyboard (boot-protocol-style 8 modifiers + reserved + 6-key array)
 *       Byte 0: ReportID = 0x01
 *       Byte 1: Modifier bitmask (E0..E7 -> bits 0..7)
 *       Byte 2: Reserved (0)
 *       Byte 3..8: Up to 6 concurrent key usages
 *   - ReportID 2: Mouse
 *       Byte 0: ReportID = 0x02
 *       Byte 1: Buttons bitmask (bit0=left, bit1=right, bit2=middle, ...)
 *       Byte 2: X (int8)
 *       Byte 3: Y (int8)
 *       Byte 4: Wheel (int8)
 */

#include <stddef.h>

/*
 * WinDDK 7600 / older MSVC toolchains don't provide C99 <stdbool.h> (or even
 * <stdint.h> in some configurations). Keep this header buildable in both the
 * Windows kernel driver and host-side unit tests.
 */

#if defined(_MSC_VER) && (_MSC_VER < 1600)
typedef signed __int8 int8_t;
typedef unsigned __int8 uint8_t;
typedef signed __int16 int16_t;
typedef unsigned __int16 uint16_t;
typedef signed __int32 int32_t;
typedef unsigned __int32 uint32_t;
#else
#include <stdint.h>
#endif

#if !defined(__cplusplus)
#if defined(_MSC_VER)
typedef unsigned char bool;
#ifndef true
#define true 1
#endif
#ifndef false
#define false 0
#endif
#else
#include <stdbool.h>
#endif
#endif

#ifdef __cplusplus
extern "C" {
#endif

/*
 * virtio-input event layout (as delivered in the event virtqueue).
 *
 * Fields are little-endian on the wire. The Win7 x86/x64 targets are also
 * little-endian, but the translator still treats the fields as LE to make the
 * contract explicit and keep the logic correct if reused elsewhere.
 */
struct virtio_input_event_le {
  uint16_t type;
  uint16_t code;
  uint32_t value;
};

struct virtio_input_event {
  uint16_t type;
  uint16_t code;
  uint32_t value;
};

/* Linux input event types (subset). */
enum virtio_input_ev_type {
  VIRTIO_INPUT_EV_SYN = 0x00,
  VIRTIO_INPUT_EV_KEY = 0x01,
  VIRTIO_INPUT_EV_REL = 0x02,
};

/* EV_SYN codes (subset). */
enum virtio_input_syn_code {
  VIRTIO_INPUT_SYN_REPORT = 0x00,
};

/* EV_REL codes (subset). */
enum virtio_input_rel_code {
  VIRTIO_INPUT_REL_X = 0x00,
  VIRTIO_INPUT_REL_Y = 0x01,
  VIRTIO_INPUT_REL_WHEEL = 0x08,
};

/*
 * EV_KEY codes used by the translator (subset of Linux input-event-codes.h).
 *
 * NOTE: These numeric values are part of the Linux input userspace ABI.
 */
enum virtio_input_key_code {
  /* Alphanumeric row + basic controls. */
  VIRTIO_INPUT_KEY_ESC = 1,
  VIRTIO_INPUT_KEY_1 = 2,
  VIRTIO_INPUT_KEY_2 = 3,
  VIRTIO_INPUT_KEY_3 = 4,
  VIRTIO_INPUT_KEY_4 = 5,
  VIRTIO_INPUT_KEY_5 = 6,
  VIRTIO_INPUT_KEY_6 = 7,
  VIRTIO_INPUT_KEY_7 = 8,
  VIRTIO_INPUT_KEY_8 = 9,
  VIRTIO_INPUT_KEY_9 = 10,
  VIRTIO_INPUT_KEY_0 = 11,
  VIRTIO_INPUT_KEY_MINUS = 12,
  VIRTIO_INPUT_KEY_EQUAL = 13,
  VIRTIO_INPUT_KEY_BACKSPACE = 14,
  VIRTIO_INPUT_KEY_TAB = 15,
  VIRTIO_INPUT_KEY_Q = 16,
  VIRTIO_INPUT_KEY_W = 17,
  VIRTIO_INPUT_KEY_E = 18,
  VIRTIO_INPUT_KEY_R = 19,
  VIRTIO_INPUT_KEY_T = 20,
  VIRTIO_INPUT_KEY_Y = 21,
  VIRTIO_INPUT_KEY_U = 22,
  VIRTIO_INPUT_KEY_I = 23,
  VIRTIO_INPUT_KEY_O = 24,
  VIRTIO_INPUT_KEY_P = 25,
  VIRTIO_INPUT_KEY_LEFTBRACE = 26,
  VIRTIO_INPUT_KEY_RIGHTBRACE = 27,
  VIRTIO_INPUT_KEY_ENTER = 28,
  VIRTIO_INPUT_KEY_LEFTCTRL = 29,
  VIRTIO_INPUT_KEY_A = 30,
  VIRTIO_INPUT_KEY_S = 31,
  VIRTIO_INPUT_KEY_D = 32,
  VIRTIO_INPUT_KEY_F = 33,
  VIRTIO_INPUT_KEY_G = 34,
  VIRTIO_INPUT_KEY_H = 35,
  VIRTIO_INPUT_KEY_J = 36,
  VIRTIO_INPUT_KEY_K = 37,
  VIRTIO_INPUT_KEY_L = 38,
  VIRTIO_INPUT_KEY_SEMICOLON = 39,
  VIRTIO_INPUT_KEY_APOSTROPHE = 40,
  VIRTIO_INPUT_KEY_GRAVE = 41,
  VIRTIO_INPUT_KEY_LEFTSHIFT = 42,
  VIRTIO_INPUT_KEY_BACKSLASH = 43,
  VIRTIO_INPUT_KEY_Z = 44,
  VIRTIO_INPUT_KEY_X = 45,
  VIRTIO_INPUT_KEY_C = 46,
  VIRTIO_INPUT_KEY_V = 47,
  VIRTIO_INPUT_KEY_B = 48,
  VIRTIO_INPUT_KEY_N = 49,
  VIRTIO_INPUT_KEY_M = 50,
  VIRTIO_INPUT_KEY_COMMA = 51,
  VIRTIO_INPUT_KEY_DOT = 52,
  VIRTIO_INPUT_KEY_SLASH = 53,
  VIRTIO_INPUT_KEY_RIGHTSHIFT = 54,
  VIRTIO_INPUT_KEY_KPASTERISK = 55,
  VIRTIO_INPUT_KEY_LEFTALT = 56,
  VIRTIO_INPUT_KEY_SPACE = 57,
  VIRTIO_INPUT_KEY_CAPSLOCK = 58,

  /* Function keys + lock keys. */
  VIRTIO_INPUT_KEY_F1 = 59,
  VIRTIO_INPUT_KEY_F2 = 60,
  VIRTIO_INPUT_KEY_F3 = 61,
  VIRTIO_INPUT_KEY_F4 = 62,
  VIRTIO_INPUT_KEY_F5 = 63,
  VIRTIO_INPUT_KEY_F6 = 64,
  VIRTIO_INPUT_KEY_F7 = 65,
  VIRTIO_INPUT_KEY_F8 = 66,
  VIRTIO_INPUT_KEY_F9 = 67,
  VIRTIO_INPUT_KEY_F10 = 68,
  VIRTIO_INPUT_KEY_NUMLOCK = 69,
  VIRTIO_INPUT_KEY_SCROLLLOCK = 70,

  /* Keypad. */
  VIRTIO_INPUT_KEY_KP7 = 71,
  VIRTIO_INPUT_KEY_KP8 = 72,
  VIRTIO_INPUT_KEY_KP9 = 73,
  VIRTIO_INPUT_KEY_KPMINUS = 74,
  VIRTIO_INPUT_KEY_KP4 = 75,
  VIRTIO_INPUT_KEY_KP5 = 76,
  VIRTIO_INPUT_KEY_KP6 = 77,
  VIRTIO_INPUT_KEY_KPPLUS = 78,
  VIRTIO_INPUT_KEY_KP1 = 79,
  VIRTIO_INPUT_KEY_KP2 = 80,
  VIRTIO_INPUT_KEY_KP3 = 81,
  VIRTIO_INPUT_KEY_KP0 = 82,
  VIRTIO_INPUT_KEY_KPDOT = 83,

  /* Non-US/ISO extra key (e.g. "< > |" next to LeftShift). */
  VIRTIO_INPUT_KEY_102ND = 86,
  VIRTIO_INPUT_KEY_F11 = 87,
  VIRTIO_INPUT_KEY_F12 = 88,
  VIRTIO_INPUT_KEY_RO = 89,

  /* Keypad / system cluster + right-side modifiers. */
  VIRTIO_INPUT_KEY_KPENTER = 96,
  VIRTIO_INPUT_KEY_RIGHTCTRL = 97,
  VIRTIO_INPUT_KEY_KPSLASH = 98,
  VIRTIO_INPUT_KEY_SYSRQ = 99,
  VIRTIO_INPUT_KEY_RIGHTALT = 100,

  /* Navigation cluster. */
  VIRTIO_INPUT_KEY_HOME = 102,
  VIRTIO_INPUT_KEY_UP = 103,
  VIRTIO_INPUT_KEY_PAGEUP = 104,
  VIRTIO_INPUT_KEY_LEFT = 105,
  VIRTIO_INPUT_KEY_RIGHT = 106,
  VIRTIO_INPUT_KEY_END = 107,
  VIRTIO_INPUT_KEY_DOWN = 108,
  VIRTIO_INPUT_KEY_PAGEDOWN = 109,
  VIRTIO_INPUT_KEY_INSERT = 110,
  VIRTIO_INPUT_KEY_DELETE = 111,

  /* System + GUI. */
  VIRTIO_INPUT_KEY_KPEQUAL = 117,
  VIRTIO_INPUT_KEY_PAUSE = 119,
  VIRTIO_INPUT_KEY_KPCOMMA = 121,
  VIRTIO_INPUT_KEY_YEN = 124,
  VIRTIO_INPUT_KEY_LEFTMETA = 125,
  VIRTIO_INPUT_KEY_RIGHTMETA = 126,
  VIRTIO_INPUT_KEY_MENU = 139,

  /* Mouse buttons (EV_KEY). */
  VIRTIO_INPUT_BTN_LEFT = 272,
  VIRTIO_INPUT_BTN_RIGHT = 273,
  VIRTIO_INPUT_BTN_MIDDLE = 274,
  VIRTIO_INPUT_BTN_SIDE = 275,
  VIRTIO_INPUT_BTN_EXTRA = 276,
};

/* HID report IDs used by this driver. */
enum hid_translate_report_id {
  HID_TRANSLATE_REPORT_ID_KEYBOARD = 0x01,
  HID_TRANSLATE_REPORT_ID_MOUSE = 0x02,
};

/*
 * Report mask used to enable/disable subsets of reports.
 *
 * Aero contract v1 exposes virtio-input keyboard and mouse as two separate PCI
 * functions. Each driver instance must expose only the report IDs that exist
 * for that device.
 *
 * The translator defaults to enabling both keyboard and mouse reports for
 * backward compatibility and for host-side unit tests. The Win7 KMDF driver
 * sets this mask per device instance.
 */
enum hid_translate_report_mask {
  HID_TRANSLATE_REPORT_MASK_KEYBOARD = 0x01,
  HID_TRANSLATE_REPORT_MASK_MOUSE = 0x02,
  HID_TRANSLATE_REPORT_MASK_ALL = HID_TRANSLATE_REPORT_MASK_KEYBOARD | HID_TRANSLATE_REPORT_MASK_MOUSE,
};

/* Sizes (bytes) of input reports emitted by the translator. */
enum hid_translate_report_size {
  HID_TRANSLATE_KEYBOARD_REPORT_SIZE = 9,
  HID_TRANSLATE_MOUSE_REPORT_SIZE = 5,
};

/*
 * Optional: keep additional pressed keys beyond the 6-key boot protocol
 * report so we can recover deterministically once slots become free.
 */
#ifndef HID_TRANSLATE_MAX_PRESSED_KEYS
#define HID_TRANSLATE_MAX_PRESSED_KEYS 32
#endif

typedef void (*hid_translate_emit_report_fn)(void *context, const uint8_t *report, size_t report_len);

struct hid_translate {
  hid_translate_emit_report_fn emit_report;
  void *emit_report_context;

  /* Which report IDs this translator is allowed to emit (see hid_translate_report_mask). */
  uint8_t enabled_reports;

  /* Keyboard state. */
  uint8_t keyboard_modifiers;
  uint8_t keyboard_pressed[HID_TRANSLATE_MAX_PRESSED_KEYS]; /* HID usages, in press order. */
  uint8_t keyboard_pressed_len;
  bool keyboard_dirty;

  /* Mouse state. */
  uint8_t mouse_buttons; /* HID button bits. */
  int32_t mouse_rel_x;
  int32_t mouse_rel_y;
  int32_t mouse_wheel;
  bool mouse_dirty;
};

void hid_translate_init(struct hid_translate *t, hid_translate_emit_report_fn emit_report, void *emit_report_context);

void hid_translate_set_enabled_reports(struct hid_translate *t, uint8_t enabled_reports);

/*
 * Clears internal state. If emit_reports is true, emits an all-zero keyboard
 * report (and mouse report) so the OS releases any latched state (prevents
 * "stuck keys" across suspend/focus loss/D0Exit).
 */
void hid_translate_reset(struct hid_translate *t, bool emit_reports);

/* Handles a single virtio-input event in little-endian wire format. */
void hid_translate_handle_event_le(struct hid_translate *t, const struct virtio_input_event_le *ev_le);

/* Handles a single virtio-input event already decoded to host endianness. */
void hid_translate_handle_event(struct hid_translate *t, const struct virtio_input_event *ev);

/*
 * Exposed for unit testing: translate a Linux KEY_* code to a USB HID keyboard
 * usage ID. Returns 0 if unsupported or if the key is represented as a HID
 * modifier bit instead of a usage in the 6-key array.
 */
uint8_t hid_translate_linux_key_to_hid_usage(uint16_t linux_key_code);

#ifdef __cplusplus
} /* extern "C" */
#endif

#include "hid_translate.h"

#if defined(_WIN32)
#include <ntddk.h>
#else
#include <string.h>
#endif

static void hid_translate_memzero(void *ptr, size_t len)
{
#if defined(_WIN32)
  RtlZeroMemory(ptr, len);
#else
  memset(ptr, 0, len);
#endif
}

static void hid_translate_memcpy(void *dst, const void *src, size_t len)
{
#if defined(_WIN32)
  RtlCopyMemory(dst, src, len);
#else
  memcpy(dst, src, len);
#endif
}

/* -------------------------------------------------------------------------- */
/* Little-endian helpers                                                      */
/* -------------------------------------------------------------------------- */

static uint16_t hid_translate_le16_to_cpu(uint16_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return (uint16_t)((v >> 8) | (v << 8));
#else
  return v;
#endif
}

static uint32_t hid_translate_le32_to_cpu(uint32_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return ((v & 0x000000FFu) << 24) | ((v & 0x0000FF00u) << 8) | ((v & 0x00FF0000u) >> 8) |
         ((v & 0xFF000000u) >> 24);
#else
  return v;
#endif
}

static int32_t hid_translate_i32_from_u32_bits(uint32_t u) {
  int32_t i;
  hid_translate_memcpy(&i, &u, sizeof(i));
  return i;
}

/* -------------------------------------------------------------------------- */
/* Linux keycode -> HID usage mapping                                         */
/* -------------------------------------------------------------------------- */

/*
 * Clean-room mapping table from Linux KEY_* codes to USB HID keyboard usages.
 *
 * Only keys represented in the boot keyboard 6-key array are included here.
 * Modifiers (Ctrl/Shift/Alt/GUI) are handled separately as a bitmask.
 */
struct hid_translate_keymap_entry {
  uint16_t linux_key_code;
  uint8_t hid_usage;
};

static const struct hid_translate_keymap_entry kLinuxToHidKeymap[] = {
    /* Letters. */
    {VIRTIO_INPUT_KEY_A, 0x04},
    {VIRTIO_INPUT_KEY_B, 0x05},
    {VIRTIO_INPUT_KEY_C, 0x06},
    {VIRTIO_INPUT_KEY_D, 0x07},
    {VIRTIO_INPUT_KEY_E, 0x08},
    {VIRTIO_INPUT_KEY_F, 0x09},
    {VIRTIO_INPUT_KEY_G, 0x0A},
    {VIRTIO_INPUT_KEY_H, 0x0B},
    {VIRTIO_INPUT_KEY_I, 0x0C},
    {VIRTIO_INPUT_KEY_J, 0x0D},
    {VIRTIO_INPUT_KEY_K, 0x0E},
    {VIRTIO_INPUT_KEY_L, 0x0F},
    {VIRTIO_INPUT_KEY_M, 0x10},
    {VIRTIO_INPUT_KEY_N, 0x11},
    {VIRTIO_INPUT_KEY_O, 0x12},
    {VIRTIO_INPUT_KEY_P, 0x13},
    {VIRTIO_INPUT_KEY_Q, 0x14},
    {VIRTIO_INPUT_KEY_R, 0x15},
    {VIRTIO_INPUT_KEY_S, 0x16},
    {VIRTIO_INPUT_KEY_T, 0x17},
    {VIRTIO_INPUT_KEY_U, 0x18},
    {VIRTIO_INPUT_KEY_V, 0x19},
    {VIRTIO_INPUT_KEY_W, 0x1A},
    {VIRTIO_INPUT_KEY_X, 0x1B},
    {VIRTIO_INPUT_KEY_Y, 0x1C},
    {VIRTIO_INPUT_KEY_Z, 0x1D},

    /* Numbers. */
    {VIRTIO_INPUT_KEY_1, 0x1E},
    {VIRTIO_INPUT_KEY_2, 0x1F},
    {VIRTIO_INPUT_KEY_3, 0x20},
    {VIRTIO_INPUT_KEY_4, 0x21},
    {VIRTIO_INPUT_KEY_5, 0x22},
    {VIRTIO_INPUT_KEY_6, 0x23},
    {VIRTIO_INPUT_KEY_7, 0x24},
    {VIRTIO_INPUT_KEY_8, 0x25},
    {VIRTIO_INPUT_KEY_9, 0x26},
    {VIRTIO_INPUT_KEY_0, 0x27},

    /* Common controls. */
    {VIRTIO_INPUT_KEY_ENTER, 0x28},
    {VIRTIO_INPUT_KEY_ESC, 0x29},
    {VIRTIO_INPUT_KEY_BACKSPACE, 0x2A},
    {VIRTIO_INPUT_KEY_TAB, 0x2B},
    {VIRTIO_INPUT_KEY_SPACE, 0x2C},

    /* Punctuation (not strictly required for minimal viability, but useful). */
    {VIRTIO_INPUT_KEY_MINUS, 0x2D},
    {VIRTIO_INPUT_KEY_EQUAL, 0x2E},
    {VIRTIO_INPUT_KEY_LEFTBRACE, 0x2F},
    {VIRTIO_INPUT_KEY_RIGHTBRACE, 0x30},
    {VIRTIO_INPUT_KEY_BACKSLASH, 0x31},
    {VIRTIO_INPUT_KEY_SEMICOLON, 0x33},
    {VIRTIO_INPUT_KEY_APOSTROPHE, 0x34},
    {VIRTIO_INPUT_KEY_GRAVE, 0x35},
    {VIRTIO_INPUT_KEY_COMMA, 0x36},
    {VIRTIO_INPUT_KEY_DOT, 0x37},
    {VIRTIO_INPUT_KEY_SLASH, 0x38},

    /* Locks. */
    {VIRTIO_INPUT_KEY_CAPSLOCK, 0x39},

    /* Function keys. */
    {VIRTIO_INPUT_KEY_F1, 0x3A},
    {VIRTIO_INPUT_KEY_F2, 0x3B},
    {VIRTIO_INPUT_KEY_F3, 0x3C},
    {VIRTIO_INPUT_KEY_F4, 0x3D},
    {VIRTIO_INPUT_KEY_F5, 0x3E},
    {VIRTIO_INPUT_KEY_F6, 0x3F},
    {VIRTIO_INPUT_KEY_F7, 0x40},
    {VIRTIO_INPUT_KEY_F8, 0x41},
    {VIRTIO_INPUT_KEY_F9, 0x42},
    {VIRTIO_INPUT_KEY_F10, 0x43},
    {VIRTIO_INPUT_KEY_F11, 0x44},
    {VIRTIO_INPUT_KEY_F12, 0x45},

    /* System / locks. */
    {VIRTIO_INPUT_KEY_SCROLLLOCK, 0x47},

    /* Navigation/editing. */
    {VIRTIO_INPUT_KEY_INSERT, 0x49},
    {VIRTIO_INPUT_KEY_HOME, 0x4A},
    {VIRTIO_INPUT_KEY_PAGEUP, 0x4B},
    {VIRTIO_INPUT_KEY_DELETE, 0x4C},
    {VIRTIO_INPUT_KEY_END, 0x4D},
    {VIRTIO_INPUT_KEY_PAGEDOWN, 0x4E},
    {VIRTIO_INPUT_KEY_RIGHT, 0x4F},
    {VIRTIO_INPUT_KEY_LEFT, 0x50},
    {VIRTIO_INPUT_KEY_DOWN, 0x51},
    {VIRTIO_INPUT_KEY_UP, 0x52},
    {VIRTIO_INPUT_KEY_NUMLOCK, 0x53},
};

uint8_t hid_translate_linux_key_to_hid_usage(uint16_t linux_key_code) {
  size_t i;
  for (i = 0; i < (sizeof(kLinuxToHidKeymap) / sizeof(kLinuxToHidKeymap[0])); ++i) {
    if (kLinuxToHidKeymap[i].linux_key_code == linux_key_code) {
      return kLinuxToHidKeymap[i].hid_usage;
    }
  }
  return 0;
}

/* -------------------------------------------------------------------------- */
/* Keyboard handling                                                          */
/* -------------------------------------------------------------------------- */

static uint8_t hid_translate_linux_key_to_modifier_bit(uint16_t linux_key_code) {
  switch (linux_key_code) {
  case VIRTIO_INPUT_KEY_LEFTCTRL:
    return 0x01;
  case VIRTIO_INPUT_KEY_LEFTSHIFT:
    return 0x02;
  case VIRTIO_INPUT_KEY_LEFTALT:
    return 0x04;
  case VIRTIO_INPUT_KEY_LEFTMETA:
    return 0x08;
  case VIRTIO_INPUT_KEY_RIGHTCTRL:
    return 0x10;
  case VIRTIO_INPUT_KEY_RIGHTSHIFT:
    return 0x20;
  case VIRTIO_INPUT_KEY_RIGHTALT:
    return 0x40;
  case VIRTIO_INPUT_KEY_RIGHTMETA:
    return 0x80;
  default:
    return 0;
  }
}

static bool hid_translate_keyboard_list_contains(const struct hid_translate *t, uint8_t usage) {
  uint8_t i;
  for (i = 0; i < t->keyboard_pressed_len; ++i) {
    if (t->keyboard_pressed[i] == usage) {
      return true;
    }
  }
  return false;
}

static bool hid_translate_keyboard_list_remove(struct hid_translate *t, uint8_t usage) {
  uint8_t i;
  uint8_t j;
  for (i = 0; i < t->keyboard_pressed_len; ++i) {
    if (t->keyboard_pressed[i] != usage) {
      continue;
    }

    /* Stable remove. */
    for (j = (uint8_t)(i + 1); j < t->keyboard_pressed_len; ++j) {
      t->keyboard_pressed[j - 1] = t->keyboard_pressed[j];
    }
    t->keyboard_pressed_len--;
    return true;
  }
  return false;
}

static bool hid_translate_keyboard_list_append(struct hid_translate *t, uint8_t usage) {
  if (t->keyboard_pressed_len >= HID_TRANSLATE_MAX_PRESSED_KEYS) {
    /*
     * Deterministic overflow policy: ignore additional keys beyond the fixed
     * tracking capacity.
     */
    return false;
  }
  t->keyboard_pressed[t->keyboard_pressed_len++] = usage;
  return true;
}

static void hid_translate_emit_keyboard_report(struct hid_translate *t) {
  if ((t->enabled_reports & HID_TRANSLATE_REPORT_MASK_KEYBOARD) == 0) {
    t->keyboard_dirty = false;
    return;
  }

  uint8_t report[HID_TRANSLATE_KEYBOARD_REPORT_SIZE];
  uint8_t i;
  hid_translate_memzero(report, sizeof(report));

  report[0] = HID_TRANSLATE_REPORT_ID_KEYBOARD;
  report[1] = t->keyboard_modifiers;
  report[2] = 0;
  for (i = 0; i < 6; ++i) {
    report[3 + i] = (i < t->keyboard_pressed_len) ? t->keyboard_pressed[i] : 0;
  }

  if (t->emit_report) {
    t->emit_report(t->emit_report_context, report, sizeof(report));
  }
  t->keyboard_dirty = false;
}

static void hid_translate_handle_keyboard_key(struct hid_translate *t, uint16_t linux_key_code, uint32_t value) {
  /*
   * Linux evdev semantics:
   *   value=0: release
   *   value=1: press
   *   value=2: autorepeat
   *
   * Policy: treat value=2 as "press" (i.e. key is down) but since the state is
   * already latched, it typically produces no additional report.
   */
  bool pressed = (value != 0);

  uint8_t modifier_bit = hid_translate_linux_key_to_modifier_bit(linux_key_code);
  if (modifier_bit != 0) {
    uint8_t new_modifiers = t->keyboard_modifiers;
    if (pressed) {
      new_modifiers |= modifier_bit;
    } else {
      new_modifiers &= (uint8_t)~modifier_bit;
    }
    if (new_modifiers != t->keyboard_modifiers) {
      t->keyboard_modifiers = new_modifiers;
      t->keyboard_dirty = true;
    }
    return;
  }

  uint8_t usage = hid_translate_linux_key_to_hid_usage(linux_key_code);
  if (usage == 0) {
    return;
  }

  if (pressed) {
    if (hid_translate_keyboard_list_contains(t, usage)) {
      return;
    }
    if (hid_translate_keyboard_list_append(t, usage)) {
      /*
       * Deterministic 6KRO policy:
       *   - Track keys in press order (up to HID_TRANSLATE_MAX_PRESSED_KEYS).
       *   - Emit the first 6 as the boot-protocol key array.
       *   - Additional keys are queued and become visible once earlier keys are
       *     released.
       */
      t->keyboard_dirty = true;
    }
  } else {
    if (hid_translate_keyboard_list_remove(t, usage)) {
      t->keyboard_dirty = true;
    }
  }
}

/* -------------------------------------------------------------------------- */
/* Mouse handling                                                             */
/* -------------------------------------------------------------------------- */

enum {
  HID_TRANSLATE_MOUSE_BUTTON_LEFT = 1u << 0,
  HID_TRANSLATE_MOUSE_BUTTON_RIGHT = 1u << 1,
  HID_TRANSLATE_MOUSE_BUTTON_MIDDLE = 1u << 2,
  HID_TRANSLATE_MOUSE_BUTTON_SIDE = 1u << 3,
  HID_TRANSLATE_MOUSE_BUTTON_EXTRA = 1u << 4,
};

static bool hid_translate_mouse_update_button(struct hid_translate *t, uint8_t bit, bool pressed) {
  uint8_t new_buttons = t->mouse_buttons;
  if (pressed) {
    new_buttons |= bit;
  } else {
    new_buttons &= (uint8_t)~bit;
  }
  if (new_buttons == t->mouse_buttons) {
    return false;
  }
  t->mouse_buttons = new_buttons;
  return true;
}

static int8_t hid_translate_take_rel_chunk(int32_t *accum) {
  /* Typical HID logical range for int8 relative axes: [-127, 127]. */
  const int32_t kMin = -127;
  const int32_t kMax = 127;

  int32_t v = *accum;
  if (v > kMax) {
    *accum -= kMax;
    return (int8_t)kMax;
  }
  if (v < kMin) {
    *accum -= kMin;
    return (int8_t)kMin;
  }

  *accum = 0;
  return (int8_t)v;
}

static void hid_translate_emit_mouse_reports(struct hid_translate *t) {
  if ((t->enabled_reports & HID_TRANSLATE_REPORT_MASK_MOUSE) == 0) {
    t->mouse_dirty = false;
    t->mouse_rel_x = 0;
    t->mouse_rel_y = 0;
    t->mouse_wheel = 0;
    return;
  }

  bool need_report = t->mouse_dirty || (t->mouse_rel_x != 0) || (t->mouse_rel_y != 0) || (t->mouse_wheel != 0);
  if (!need_report) {
    return;
  }

  do {
    int8_t dx = hid_translate_take_rel_chunk(&t->mouse_rel_x);
    int8_t dy = hid_translate_take_rel_chunk(&t->mouse_rel_y);
    int8_t wheel = hid_translate_take_rel_chunk(&t->mouse_wheel);

    uint8_t report[HID_TRANSLATE_MOUSE_REPORT_SIZE];
    report[0] = HID_TRANSLATE_REPORT_ID_MOUSE;
    report[1] = t->mouse_buttons;
    report[2] = (uint8_t)dx;
    report[3] = (uint8_t)dy;
    report[4] = (uint8_t)wheel;

    if (t->emit_report) {
      t->emit_report(t->emit_report_context, report, sizeof(report));
    }

    /* Button changes are represented in the first emitted report. */
    t->mouse_dirty = false;
  } while ((t->mouse_rel_x != 0) || (t->mouse_rel_y != 0) || (t->mouse_wheel != 0));
}

/* -------------------------------------------------------------------------- */
/* Public API                                                                 */
/* -------------------------------------------------------------------------- */

void hid_translate_init(struct hid_translate *t, hid_translate_emit_report_fn emit_report, void *emit_report_context) {
  hid_translate_memzero(t, sizeof(*t));
  t->emit_report = emit_report;
  t->emit_report_context = emit_report_context;
  t->enabled_reports = HID_TRANSLATE_REPORT_MASK_ALL;
}

void hid_translate_set_enabled_reports(struct hid_translate *t, uint8_t enabled_reports) {
  if (t == NULL) {
    return;
  }
  t->enabled_reports = enabled_reports;
}

void hid_translate_reset(struct hid_translate *t, bool emit_reports) {
  t->keyboard_modifiers = 0;
  t->keyboard_pressed_len = 0;
  t->keyboard_dirty = false;

  t->mouse_buttons = 0;
  t->mouse_rel_x = 0;
  t->mouse_rel_y = 0;
  t->mouse_wheel = 0;
  t->mouse_dirty = false;

  if (!emit_reports) {
    return;
  }

  /* Emit all-zero reports to release any latched state in the HID stacks. */
  if ((t->enabled_reports & HID_TRANSLATE_REPORT_MASK_KEYBOARD) != 0) {
    hid_translate_emit_keyboard_report(t);
  }
  if ((t->enabled_reports & HID_TRANSLATE_REPORT_MASK_MOUSE) != 0) {
    t->mouse_dirty = true;
    hid_translate_emit_mouse_reports(t);
  }
}

void hid_translate_handle_event_le(struct hid_translate *t, const struct virtio_input_event_le *ev_le) {
  struct virtio_input_event ev;
  ev.type = hid_translate_le16_to_cpu(ev_le->type);
  ev.code = hid_translate_le16_to_cpu(ev_le->code);
  ev.value = hid_translate_le32_to_cpu(ev_le->value);
  hid_translate_handle_event(t, &ev);
}

void hid_translate_handle_event(struct hid_translate *t, const struct virtio_input_event *ev) {
  switch (ev->type) {
  case VIRTIO_INPUT_EV_KEY: {
    bool pressed = (ev->value != 0);

    switch (ev->code) {
    case VIRTIO_INPUT_BTN_LEFT:
      if (hid_translate_mouse_update_button(t, HID_TRANSLATE_MOUSE_BUTTON_LEFT, pressed)) {
        t->mouse_dirty = true;
      }
      break;
    case VIRTIO_INPUT_BTN_RIGHT:
      if (hid_translate_mouse_update_button(t, HID_TRANSLATE_MOUSE_BUTTON_RIGHT, pressed)) {
        t->mouse_dirty = true;
      }
      break;
    case VIRTIO_INPUT_BTN_MIDDLE:
      if (hid_translate_mouse_update_button(t, HID_TRANSLATE_MOUSE_BUTTON_MIDDLE, pressed)) {
        t->mouse_dirty = true;
      }
      break;
    case VIRTIO_INPUT_BTN_SIDE:
      if (hid_translate_mouse_update_button(t, HID_TRANSLATE_MOUSE_BUTTON_SIDE, pressed)) {
        t->mouse_dirty = true;
      }
      break;
    case VIRTIO_INPUT_BTN_EXTRA:
      if (hid_translate_mouse_update_button(t, HID_TRANSLATE_MOUSE_BUTTON_EXTRA, pressed)) {
        t->mouse_dirty = true;
      }
      break;
    default:
      hid_translate_handle_keyboard_key(t, ev->code, ev->value);
      break;
    }
    break;
  }

  case VIRTIO_INPUT_EV_REL: {
    int32_t delta = hid_translate_i32_from_u32_bits(ev->value);
    switch (ev->code) {
    case VIRTIO_INPUT_REL_X:
      t->mouse_rel_x += delta;
      break;
    case VIRTIO_INPUT_REL_Y:
      t->mouse_rel_y += delta;
      break;
    case VIRTIO_INPUT_REL_WHEEL:
      t->mouse_wheel += delta;
      break;
    default:
      break;
    }
    if (delta != 0) {
      t->mouse_dirty = true;
    }
    break;
  }

  case VIRTIO_INPUT_EV_SYN:
    if (ev->code == VIRTIO_INPUT_SYN_REPORT) {
      if (t->keyboard_dirty) {
        hid_translate_emit_keyboard_report(t);
      }
      hid_translate_emit_mouse_reports(t);
    }
    break;

  default:
    break;
  }
}

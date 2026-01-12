import { describe, expect, it } from "vitest";

import { keyboardCodeToHidUsage } from "../../input/hid_usage";
import { hidUsageToLinuxKeyCode } from "./virtio_input";

describe("io/devices/virtio_input hidUsageToLinuxKeyCode", () => {
  it("maps contract-required alphanumerics, basic keys, and function keys", () => {
    // Letters.
    expect(hidUsageToLinuxKeyCode(0x04)).toBe(30); // KEY_A
    expect(hidUsageToLinuxKeyCode(0x1d)).toBe(44); // KEY_Z

    // Digits.
    expect(hidUsageToLinuxKeyCode(0x27)).toBe(11); // KEY_0

    // Basic.
    expect(hidUsageToLinuxKeyCode(0x28)).toBe(28); // KEY_ENTER
    expect(hidUsageToLinuxKeyCode(0x29)).toBe(1); // KEY_ESC

    // Function keys.
    expect(hidUsageToLinuxKeyCode(0x3a)).toBe(59); // KEY_F1
    expect(hidUsageToLinuxKeyCode(0x45)).toBe(88); // KEY_F12
  });

  it("maps punctuation + locks + meta HID usages to Linux KEY_* codes", () => {
    const cases: Array<{ usage: number; linuxKey: number }> = [
      // Punctuation.
      { usage: 0x2d, linuxKey: 12 }, // Minus -> KEY_MINUS
      { usage: 0x2e, linuxKey: 13 }, // Equal -> KEY_EQUAL
      { usage: 0x2f, linuxKey: 26 }, // BracketLeft -> KEY_LEFTBRACE
      { usage: 0x30, linuxKey: 27 }, // BracketRight -> KEY_RIGHTBRACE
      { usage: 0x31, linuxKey: 43 }, // Backslash -> KEY_BACKSLASH
      { usage: 0x32, linuxKey: 43 }, // IntlHash -> KEY_BACKSLASH (alias)
      { usage: 0x33, linuxKey: 39 }, // Semicolon -> KEY_SEMICOLON
      { usage: 0x34, linuxKey: 40 }, // Quote -> KEY_APOSTROPHE
      { usage: 0x35, linuxKey: 41 }, // Backquote -> KEY_GRAVE
      { usage: 0x36, linuxKey: 51 }, // Comma -> KEY_COMMA
      { usage: 0x37, linuxKey: 52 }, // Period -> KEY_DOT
      { usage: 0x38, linuxKey: 53 }, // Slash -> KEY_SLASH

      // Locks.
      { usage: 0x47, linuxKey: 70 }, // ScrollLock -> KEY_SCROLLLOCK
      { usage: 0x53, linuxKey: 69 }, // NumLock -> KEY_NUMLOCK

      // Meta.
      { usage: 0xe3, linuxKey: 125 }, // Left GUI -> KEY_LEFTMETA
      { usage: 0xe7, linuxKey: 126 }, // Right GUI -> KEY_RIGHTMETA
    ];

    for (const tc of cases) {
      expect(hidUsageToLinuxKeyCode(tc.usage)).toBe(tc.linuxKey);
    }
  });

  it("maps boot-keyboard-compatible sysrq/pause + keypad keys used by the Win7 virtio-input driver", () => {
    const cases: Array<{ usage: number; linuxKey: number }> = [
      // System.
      { usage: 0x46, linuxKey: 99 }, // PrintScreen -> KEY_SYSRQ
      { usage: 0x48, linuxKey: 119 }, // Pause -> KEY_PAUSE

      // Keypad (boot keyboard usages 0x54..0x63).
      { usage: 0x54, linuxKey: 98 }, // NumpadDivide -> KEY_KPSLASH
      { usage: 0x55, linuxKey: 55 }, // NumpadMultiply -> KEY_KPASTERISK
      { usage: 0x56, linuxKey: 74 }, // NumpadSubtract -> KEY_KPMINUS
      { usage: 0x57, linuxKey: 78 }, // NumpadAdd -> KEY_KPPLUS
      { usage: 0x58, linuxKey: 96 }, // NumpadEnter -> KEY_KPENTER
      { usage: 0x59, linuxKey: 79 }, // Numpad1 -> KEY_KP1
      { usage: 0x5a, linuxKey: 80 }, // Numpad2 -> KEY_KP2
      { usage: 0x5b, linuxKey: 81 }, // Numpad3 -> KEY_KP3
      { usage: 0x5c, linuxKey: 75 }, // Numpad4 -> KEY_KP4
      { usage: 0x5d, linuxKey: 76 }, // Numpad5/Clear -> KEY_KP5
      { usage: 0x5e, linuxKey: 77 }, // Numpad6 -> KEY_KP6
      { usage: 0x5f, linuxKey: 71 }, // Numpad7 -> KEY_KP7
      { usage: 0x60, linuxKey: 72 }, // Numpad8 -> KEY_KP8
      { usage: 0x61, linuxKey: 73 }, // Numpad9 -> KEY_KP9
      { usage: 0x62, linuxKey: 82 }, // Numpad0 -> KEY_KP0
      { usage: 0x63, linuxKey: 83 }, // NumpadDecimal -> KEY_KPDOT

      // Intl / menu.
      { usage: 0x64, linuxKey: 86 }, // IntlBackslash -> KEY_102ND
      { usage: 0x65, linuxKey: 139 }, // ContextMenu -> KEY_MENU
    ];

    for (const tc of cases) {
      expect(hidUsageToLinuxKeyCode(tc.usage)).toBe(tc.linuxKey);
    }
  });

  it("keeps DOM->HID->Linux key mapping consistent for representative keys", () => {
    const cases: Array<{ code: string; linuxKey: number }> = [
      { code: "Minus", linuxKey: 12 }, // KEY_MINUS
      { code: "BracketLeft", linuxKey: 26 }, // KEY_LEFTBRACE
      { code: "Backquote", linuxKey: 41 }, // KEY_GRAVE
      { code: "MetaLeft", linuxKey: 125 }, // KEY_LEFTMETA
      { code: "NumLock", linuxKey: 69 }, // KEY_NUMLOCK
      { code: "ScrollLock", linuxKey: 70 }, // KEY_SCROLLLOCK
      { code: "PrintScreen", linuxKey: 99 }, // KEY_SYSRQ
      { code: "Pause", linuxKey: 119 }, // KEY_PAUSE
      { code: "NumpadDivide", linuxKey: 98 }, // KEY_KPSLASH
      { code: "NumpadEnter", linuxKey: 96 }, // KEY_KPENTER
      { code: "NumpadDecimal", linuxKey: 83 }, // KEY_KPDOT
      { code: "IntlHash", linuxKey: 43 }, // KEY_BACKSLASH (alias)
      { code: "IntlBackslash", linuxKey: 86 }, // KEY_102ND
      { code: "ContextMenu", linuxKey: 139 }, // KEY_MENU
    ];

    for (const tc of cases) {
      const usage = keyboardCodeToHidUsage(tc.code);
      if (usage === null) throw new Error(`expected keyboardCodeToHidUsage(${JSON.stringify(tc.code)}) to be non-null`);
      expect(hidUsageToLinuxKeyCode(usage)).toBe(tc.linuxKey);
    }
  });
});

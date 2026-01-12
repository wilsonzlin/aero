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

  it("maps contract-required alphanumerics, Enter/Esc, and function keys", () => {
    // A..Z.
    expect(hidUsageToLinuxKeyCode(0x04)).toBe(30); // KEY_A
    expect(hidUsageToLinuxKeyCode(0x1d)).toBe(44); // KEY_Z

    // 0..9.
    expect(hidUsageToLinuxKeyCode(0x27)).toBe(11); // KEY_0

    // Enter / Esc.
    expect(hidUsageToLinuxKeyCode(0x28)).toBe(28); // KEY_ENTER
    expect(hidUsageToLinuxKeyCode(0x29)).toBe(1); // KEY_ESC

    // Function keys.
    expect(hidUsageToLinuxKeyCode(0x3a)).toBe(59); // KEY_F1
    expect(hidUsageToLinuxKeyCode(0x45)).toBe(88); // KEY_F12
  });

  it("keeps DOM->HID->Linux key mapping consistent for representative keys", () => {
    const cases: Array<{ code: string; linuxKey: number }> = [
      { code: "Minus", linuxKey: 12 }, // KEY_MINUS
      { code: "BracketLeft", linuxKey: 26 }, // KEY_LEFTBRACE
      { code: "Backquote", linuxKey: 41 }, // KEY_GRAVE
      { code: "MetaLeft", linuxKey: 125 }, // KEY_LEFTMETA
      { code: "NumLock", linuxKey: 69 }, // KEY_NUMLOCK
      { code: "ScrollLock", linuxKey: 70 }, // KEY_SCROLLLOCK
    ];

    for (const tc of cases) {
      const usage = keyboardCodeToHidUsage(tc.code);
      if (usage === null) throw new Error(`expected keyboardCodeToHidUsage(${JSON.stringify(tc.code)}) to be non-null`);
      expect(hidUsageToLinuxKeyCode(usage)).toBe(tc.linuxKey);
    }
  });
});

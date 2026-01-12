import { describe, expect, it } from "vitest";

import { keyboardCodeToHidUsage } from "../../input/hid_usage";
import { hidUsageToLinuxKeyCode } from "./virtio_input";

describe("io/devices/virtio_input hidUsageToLinuxKeyCode", () => {
  it("maps common punctuation HID usages to Linux KEY_* codes", () => {
    expect(hidUsageToLinuxKeyCode(0x2d)).toBe(12); // KEY_MINUS
    expect(hidUsageToLinuxKeyCode(0x2e)).toBe(13); // KEY_EQUAL
    expect(hidUsageToLinuxKeyCode(0x2f)).toBe(26); // KEY_LEFTBRACE
    expect(hidUsageToLinuxKeyCode(0x30)).toBe(27); // KEY_RIGHTBRACE
    expect(hidUsageToLinuxKeyCode(0x31)).toBe(43); // KEY_BACKSLASH
    expect(hidUsageToLinuxKeyCode(0x33)).toBe(39); // KEY_SEMICOLON
    expect(hidUsageToLinuxKeyCode(0x34)).toBe(40); // KEY_APOSTROPHE
    expect(hidUsageToLinuxKeyCode(0x35)).toBe(41); // KEY_GRAVE
    expect(hidUsageToLinuxKeyCode(0x36)).toBe(51); // KEY_COMMA
    expect(hidUsageToLinuxKeyCode(0x37)).toBe(52); // KEY_DOT
    expect(hidUsageToLinuxKeyCode(0x38)).toBe(53); // KEY_SLASH
  });

  it("maps locks and meta keys already advertised by the virtio-input keyboard", () => {
    expect(hidUsageToLinuxKeyCode(0x47)).toBe(70); // KEY_SCROLLLOCK
    expect(hidUsageToLinuxKeyCode(0x53)).toBe(69); // KEY_NUMLOCK
    expect(hidUsageToLinuxKeyCode(0xe3)).toBe(125); // KEY_LEFTMETA
    expect(hidUsageToLinuxKeyCode(0xe7)).toBe(126); // KEY_RIGHTMETA
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

    for (const c of cases) {
      const usage = keyboardCodeToHidUsage(c.code);
      if (usage === null) {
        throw new Error(`keyboardCodeToHidUsage(${JSON.stringify(c.code)}) unexpectedly returned null`);
      }
      expect(hidUsageToLinuxKeyCode(usage)).toBe(c.linuxKey);
    }
  });
});


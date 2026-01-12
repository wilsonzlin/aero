import { describe, expect, it } from "vitest";

import { hidUsageToLinuxKeyCode } from "./virtio_input";

describe("hidUsageToLinuxKeyCode", () => {
  it("maps modifier (GUI) keys and lock keys used by the virtio-input path", () => {
    // Modifiers (HID usages 0xE0..=0xE7).
    expect(hidUsageToLinuxKeyCode(0xe3)).toBe(125); // KEY_LEFTMETA
    expect(hidUsageToLinuxKeyCode(0xe7)).toBe(126); // KEY_RIGHTMETA

    // Locks / system.
    expect(hidUsageToLinuxKeyCode(0x47)).toBe(70); // KEY_SCROLLLOCK
    expect(hidUsageToLinuxKeyCode(0x53)).toBe(69); // KEY_NUMLOCK
  });

  it("maps contract-required function keys", () => {
    expect(hidUsageToLinuxKeyCode(0x3a)).toBe(59); // KEY_F1
    expect(hidUsageToLinuxKeyCode(0x45)).toBe(88); // KEY_F12
  });
});


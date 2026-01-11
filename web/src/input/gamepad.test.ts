import { describe, expect, it } from "vitest";

import {
  GAMEPAD_HAT_NEUTRAL,
  computeGamepadHat,
  gamepadButtonsToBitfield,
  packGamepadReport,
  quantizeGamepadAxis,
  unpackGamepadReport,
} from "./gamepad";

describe("computeGamepadHat", () => {
  it("maps d-pad combos to HID hat values", () => {
    expect(computeGamepadHat(true, false, false, false)).toBe(0); // up
    expect(computeGamepadHat(true, true, false, false)).toBe(1); // up-right
    expect(computeGamepadHat(false, true, false, false)).toBe(2); // right
    expect(computeGamepadHat(false, true, true, false)).toBe(3); // down-right
    expect(computeGamepadHat(false, false, true, false)).toBe(4); // down
    expect(computeGamepadHat(false, false, true, true)).toBe(5); // down-left
    expect(computeGamepadHat(false, false, false, true)).toBe(6); // left
    expect(computeGamepadHat(true, false, false, true)).toBe(7); // up-left
    expect(computeGamepadHat(false, false, false, false)).toBe(GAMEPAD_HAT_NEUTRAL);
  });

  it("treats impossible opposing d-pad combinations as neutral", () => {
    expect(computeGamepadHat(true, false, true, false)).toBe(GAMEPAD_HAT_NEUTRAL); // up + down
    expect(computeGamepadHat(false, true, false, true)).toBe(GAMEPAD_HAT_NEUTRAL); // left + right
  });
});

describe("quantizeGamepadAxis", () => {
  it("applies deadzone and quantizes to i8", () => {
    expect(quantizeGamepadAxis(0, 0.1)).toBe(0);
    expect(quantizeGamepadAxis(0.05, 0.1)).toBe(0);
    expect(quantizeGamepadAxis(-0.05, 0.1)).toBe(0);

    expect(quantizeGamepadAxis(0.2, 0.1)).toBe(Math.round(0.2 * 127));
    expect(quantizeGamepadAxis(-0.2, 0.1)).toBe(-Math.round(0.2 * 127));

    expect(quantizeGamepadAxis(1, 0.1)).toBe(127);
    expect(quantizeGamepadAxis(-1, 0.1)).toBe(-127);
  });
});

describe("gamepadButtonsToBitfield", () => {
  it("maps standard buttons into a 16-bit bitfield (excluding d-pad)", () => {
    const buttons = Array.from({ length: 20 }, () => ({ pressed: false }));
    buttons[0]!.pressed = true; // A
    buttons[9]!.pressed = true; // Start
    buttons[12]!.pressed = true; // d-pad up (excluded)
    buttons[16]!.pressed = true; // Guide/Home

    const bits = gamepadButtonsToBitfield(buttons);
    expect(bits).toBe((1 << 0) | (1 << 9) | (1 << 12));
  });
});

describe("packGamepadReport", () => {
  it("packs into two u32 words that round-trip to 8 bytes", () => {
    const { packedLo, packedHi } = packGamepadReport({
      buttons: 0x1234,
      hat: 2,
      x: 1,
      y: -1,
      rx: 127,
      ry: -127,
    });

    const bytes = Array.from(unpackGamepadReport(packedLo, packedHi));
    expect(bytes).toEqual([0x34, 0x12, 0x02, 0x01, 0xff, 0x7f, 0x81, 0x00]);
  });
});

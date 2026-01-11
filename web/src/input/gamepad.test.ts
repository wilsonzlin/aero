import { describe, expect, it } from "vitest";

import {
  GAMEPAD_HAT_NEUTRAL,
  GamepadCapture,
  computeGamepadHat,
  packGamepadReport,
  gamepadButtonsToBitfield,
  quantizeGamepadAxis,
  unpackGamepadReport,
} from "./gamepad";
import { InputEventQueue, InputEventType, type InputBatchMessage, type InputBatchTarget } from "./event_queue";

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

describe("GamepadCapture", () => {
  it("de-dups unchanged reports and can emit a neutral report", () => {
    type Btn = { pressed: boolean };
    const makeButtons = (pressed: number[]): Btn[] => {
      const buttons = Array.from({ length: 20 }, () => ({ pressed: false }));
      for (const idx of pressed) buttons[idx]!.pressed = true;
      return buttons;
    };

    type StubGamepad = {
      buttons: Btn[];
      axes: number[];
      index: number;
      connected: boolean;
    };

    let pad: StubGamepad | null = null;
    const capture = new GamepadCapture({
      deadzone: 0,
      getGamepads: () => [pad as unknown as Gamepad],
    });
    const queue = new InputEventQueue(16);

    // No pad: neutral is already the implicit baseline, so no event.
    capture.poll(queue, 1, { active: true });
    expect(queue.size).toBe(0);

    pad = { buttons: makeButtons([0]), axes: [0, 0, 0, 0], index: 0, connected: true };
    capture.poll(queue, 2, { active: true });
    // Same state again: no additional event.
    capture.poll(queue, 3, { active: true });

    // Change axis.
    pad = { buttons: makeButtons([0]), axes: [1, 0, 0, 0], index: 0, connected: true };
    capture.poll(queue, 4, { active: true });

    // Explicit neutral.
    capture.emitNeutral(queue, 5);

    let posted: InputBatchMessage | null = null;
    const target: InputBatchTarget = {
      postMessage: (msg) => {
        posted = msg;
      },
    };
    queue.flush(target);
    if (!posted) throw new Error("expected flush to post a batch");

    const words = new Int32Array(posted.buffer);
    expect(words[0]).toBe(3);

    const expected1 = packGamepadReport({ buttons: 1, hat: GAMEPAD_HAT_NEUTRAL, x: 0, y: 0, rx: 0, ry: 0 });
    const expected2 = packGamepadReport({ buttons: 1, hat: GAMEPAD_HAT_NEUTRAL, x: 127, y: 0, rx: 0, ry: 0 });
    const expected3 = packGamepadReport({ buttons: 0, hat: GAMEPAD_HAT_NEUTRAL, x: 0, y: 0, rx: 0, ry: 0 });

    const base = 2;
    const ev0 = base + 0 * 4;
    expect(words[ev0]).toBe(InputEventType.GamepadReport);
    expect(words[ev0 + 1]).toBe(2);
    expect(words[ev0 + 2] >>> 0).toBe(expected1.packedLo >>> 0);
    expect(words[ev0 + 3] >>> 0).toBe(expected1.packedHi >>> 0);

    const ev1 = base + 1 * 4;
    expect(words[ev1]).toBe(InputEventType.GamepadReport);
    expect(words[ev1 + 1]).toBe(4);
    expect(words[ev1 + 2] >>> 0).toBe(expected2.packedLo >>> 0);
    expect(words[ev1 + 3] >>> 0).toBe(expected2.packedHi >>> 0);

    const ev2 = base + 2 * 4;
    expect(words[ev2]).toBe(InputEventType.GamepadReport);
    expect(words[ev2 + 1]).toBe(5);
    expect(words[ev2 + 2] >>> 0).toBe(expected3.packedLo >>> 0);
    expect(words[ev2 + 3] >>> 0).toBe(expected3.packedHi >>> 0);
  });
});

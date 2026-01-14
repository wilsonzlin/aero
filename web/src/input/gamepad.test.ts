import { Buffer } from "node:buffer";
import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";

import {
  GAMEPAD_HAT_NEUTRAL,
  GamepadCapture,
  computeGamepadHat,
  decodeGamepadReport,
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

  it("clamps out-of-range hat/axes values", () => {
    const { packedLo, packedHi } = packGamepadReport({
      buttons: 0,
      hat: 99,
      x: 200,
      y: -200,
      rx: 200,
      ry: -200,
    });

    const bytes = Array.from(unpackGamepadReport(packedLo, packedHi));
    expect(bytes[2]).toBe(GAMEPAD_HAT_NEUTRAL);
    expect(bytes[3]).toBe(0x7f);
    expect(bytes[4]).toBe(0x81);
    expect(bytes[5]).toBe(0x7f);
    expect(bytes[6]).toBe(0x81);
  });
});

describe("hid_gamepad_report_vectors fixture", () => {
  it("matches cross-language report byte packing", async () => {
    type FixtureVector = {
      name?: string;
      buttons: number;
      hat: number;
      x: number;
      y: number;
      rx: number;
      ry: number;
      bytes: number[];
    };

    const raw = await readFile(new URL("../../../docs/fixtures/hid_gamepad_report_vectors.json", import.meta.url), "utf8");
    expect(Buffer.byteLength(raw, "utf8")).toBeLessThanOrEqual(64 * 1024);
    const vectors = JSON.parse(raw) as FixtureVector[];

    expect(vectors.length).toBeGreaterThan(0);
    expect(vectors.length).toBeLessThanOrEqual(64);

    const uniqueNames = new Set<string>();

    for (const [idx, v] of vectors.entries()) {
      expect(v.bytes, v.name ?? `vector ${idx}`).toHaveLength(8);
      // These vectors are inputs to `packGamepadReport` and intentionally stay within the
      // Rust-side canonical report field ranges so the fixture primarily validates layout/packing.
      expect(Number.isInteger(v.buttons), v.name ?? `vector ${idx}`).toBe(true);
      expect(v.buttons, v.name ?? `vector ${idx}`).toBeGreaterThanOrEqual(0);
      expect(v.buttons, v.name ?? `vector ${idx}`).toBeLessThanOrEqual(0xffff);
      expect(Number.isInteger(v.hat), v.name ?? `vector ${idx}`).toBe(true);
      expect(v.hat, v.name ?? `vector ${idx}`).toBeGreaterThanOrEqual(0);
      expect(v.hat, v.name ?? `vector ${idx}`).toBeLessThanOrEqual(GAMEPAD_HAT_NEUTRAL);
      if (v.name) {
        expect(uniqueNames.has(v.name), `duplicate fixture vector name: ${v.name}`).toBe(false);
        uniqueNames.add(v.name);
      }
      for (const [axisName, axis] of [
        ["x", v.x],
        ["y", v.y],
        ["rx", v.rx],
        ["ry", v.ry],
      ] as const) {
        expect(Number.isInteger(axis), `${v.name ?? `vector ${idx}`}.${axisName}`).toBe(true);
        expect(axis, `${v.name ?? `vector ${idx}`}.${axisName}`).toBeGreaterThanOrEqual(-127);
        expect(axis, `${v.name ?? `vector ${idx}`}.${axisName}`).toBeLessThanOrEqual(127);
      }
      for (const [bIdx, b] of v.bytes.entries()) {
        expect(Number.isInteger(b), `${v.name ?? `vector ${idx}`}.bytes[${bIdx}]`).toBe(true);
        expect(b, `${v.name ?? `vector ${idx}`}.bytes[${bIdx}]`).toBeGreaterThanOrEqual(0);
        expect(b, `${v.name ?? `vector ${idx}`}.bytes[${bIdx}]`).toBeLessThanOrEqual(0xff);
      }

      const { packedLo, packedHi } = packGamepadReport({
        buttons: v.buttons,
        hat: v.hat,
        x: v.x,
        y: v.y,
        rx: v.rx,
        ry: v.ry,
      });

      const bytes = Array.from(unpackGamepadReport(packedLo, packedHi));
      expect(bytes, v.name ?? `vector ${idx}`).toEqual(v.bytes);

      const decoded = decodeGamepadReport(packedLo, packedHi);
      expect(decoded, v.name ?? `vector ${idx}`).toEqual({
        buttons: v.buttons,
        hat: v.hat,
        x: v.x,
        y: v.y,
        rx: v.rx,
        ry: v.ry,
      });
    }
  });
});

describe("hid_gamepad_report_clamping_vectors fixture", () => {
  it("matches cross-language clamping semantics for packing", async () => {
    type FixtureVector = {
      name?: string;
      buttons: number;
      hat: number;
      x: number;
      y: number;
      rx: number;
      ry: number;
      bytes: number[];
    };

    const raw = await readFile(
      new URL("../../../docs/fixtures/hid_gamepad_report_clamping_vectors.json", import.meta.url),
      "utf8",
    );
    const vectors = JSON.parse(raw) as FixtureVector[];

    expect(vectors.length).toBeGreaterThan(0);
    expect(vectors.length).toBeLessThanOrEqual(64);

    const clampHat = (hat: number): number =>
      (Number.isFinite(hat) && hat >= 0 && hat <= GAMEPAD_HAT_NEUTRAL ? hat : GAMEPAD_HAT_NEUTRAL) | 0;
    const clampAxis = (axis: number): number => Math.max(-127, Math.min(127, axis | 0)) | 0;

    for (const [idx, v] of vectors.entries()) {
      expect(v.bytes, v.name ?? `vector ${idx}`).toHaveLength(8);

      // These vectors intentionally include out-of-range fields (e.g. hat outside [0..8] or axes
      // outside [-127..127]) so we can pin down clamping/masking semantics across implementations.
      expect(Number.isSafeInteger(v.buttons), v.name ?? `vector ${idx}`).toBe(true);
      expect(Number.isSafeInteger(v.hat), v.name ?? `vector ${idx}`).toBe(true);
      for (const [axisName, axis] of [
        ["x", v.x],
        ["y", v.y],
        ["rx", v.rx],
        ["ry", v.ry],
      ] as const) {
        expect(Number.isSafeInteger(axis), `${v.name ?? `vector ${idx}`}.${axisName}`).toBe(true);
      }
      for (const [bIdx, b] of v.bytes.entries()) {
        expect(Number.isInteger(b), `${v.name ?? `vector ${idx}`}.bytes[${bIdx}]`).toBe(true);
        expect(b, `${v.name ?? `vector ${idx}`}.bytes[${bIdx}]`).toBeGreaterThanOrEqual(0);
        expect(b, `${v.name ?? `vector ${idx}`}.bytes[${bIdx}]`).toBeLessThanOrEqual(0xff);
      }

      const { packedLo, packedHi } = packGamepadReport({
        buttons: v.buttons,
        hat: v.hat,
        x: v.x,
        y: v.y,
        rx: v.rx,
        ry: v.ry,
      });

      const bytes = Array.from(unpackGamepadReport(packedLo, packedHi));
      expect(bytes, v.name ?? `vector ${idx}`).toEqual(v.bytes);

      const decoded = decodeGamepadReport(packedLo, packedHi);
      expect(decoded, v.name ?? `vector ${idx}`).toEqual({
        buttons: v.buttons & 0xffff,
        hat: clampHat(v.hat),
        x: clampAxis(v.x),
        y: clampAxis(v.y),
        rx: clampAxis(v.rx),
        ry: clampAxis(v.ry),
      });
    }
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

    const state: { posted: InputBatchMessage | null } = { posted: null };
    const target: InputBatchTarget = {
      postMessage: (msg, _transfer) => {
        state.posted = msg;
      },
    };
    queue.flush(target);
    if (!state.posted) throw new Error("expected flush to post a batch");

    const words = new Int32Array(state.posted.buffer);
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

  it("selects a single active pad when multiple gamepads are present", () => {
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

    let pad0: StubGamepad = { buttons: makeButtons([0]), axes: [0, 0, 0, 0], index: 0, connected: false };
    let pad1: StubGamepad = { buttons: makeButtons([1]), axes: [0, 0, 0, 0], index: 1, connected: true };

    const capture = new GamepadCapture({
      deadzone: 0,
      getGamepads: () => [pad0 as unknown as Gamepad, pad1 as unknown as Gamepad],
    });
    const queue = new InputEventQueue(16);

    // First poll should choose the first connected pad (pad1) and emit its state.
    capture.poll(queue, 1, { active: true });
    expect(queue.size).toBe(1);

    // If another pad becomes connected later, the capture should keep using the
    // existing active pad (no flip-flop).
    pad0 = { ...pad0, connected: true };
    capture.poll(queue, 2, { active: true });
    expect(queue.size).toBe(1);

    // Changes to the active pad should be emitted...
    pad1 = { ...pad1, axes: [1, 0, 0, 0] };
    capture.poll(queue, 3, { active: true });
    expect(queue.size).toBe(2);

    // ...but changes to the inactive pad should be ignored.
    pad0 = { ...pad0, axes: [1, 0, 0, 0] };
    capture.poll(queue, 4, { active: true });
    expect(queue.size).toBe(2);

    const state: { posted: InputBatchMessage | null } = { posted: null };
    const target: InputBatchTarget = {
      postMessage: (msg, _transfer) => {
        state.posted = msg;
      },
    };
    queue.flush(target);
    if (!state.posted) throw new Error("expected flush to post a batch");

    const words = new Int32Array(state.posted.buffer);
    expect(words[0]).toBe(2);

    const expected1 = packGamepadReport({ buttons: 1 << 1, hat: GAMEPAD_HAT_NEUTRAL, x: 0, y: 0, rx: 0, ry: 0 });
    const expected2 = packGamepadReport({ buttons: 1 << 1, hat: GAMEPAD_HAT_NEUTRAL, x: 127, y: 0, rx: 0, ry: 0 });

    const base = 2;
    const ev0 = base + 0 * 4;
    expect(words[ev0]).toBe(InputEventType.GamepadReport);
    expect(words[ev0 + 1]).toBe(1);
    expect(words[ev0 + 2] >>> 0).toBe(expected1.packedLo >>> 0);
    expect(words[ev0 + 3] >>> 0).toBe(expected1.packedHi >>> 0);

    const ev1 = base + 1 * 4;
    expect(words[ev1]).toBe(InputEventType.GamepadReport);
    expect(words[ev1 + 1]).toBe(3);
    expect(words[ev1 + 2] >>> 0).toBe(expected2.packedLo >>> 0);
    expect(words[ev1 + 3] >>> 0).toBe(expected2.packedHi >>> 0);
  });

  it("switches active pads on disconnect and emits neutral exactly once", () => {
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

    let pad0: StubGamepad = { buttons: makeButtons([0]), axes: [0, 0, 0, 0], index: 0, connected: true };
    let pad1: StubGamepad = { buttons: makeButtons([]), axes: [0, 0, 0, 0], index: 1, connected: true };

    const capture = new GamepadCapture({
      deadzone: 0,
      getGamepads: () => [pad0 as unknown as Gamepad, pad1 as unknown as Gamepad],
    });
    const queue = new InputEventQueue(16);

    // Initial poll should select pad0 and emit a non-neutral report.
    capture.poll(queue, 1, { active: true });
    expect(queue.size).toBe(1);

    // Disconnect pad0 while pad1 remains connected; poll should switch to pad1.
    // Pad1 is neutral, so this should emit a neutral report once.
    pad0 = { ...pad0, connected: false };
    capture.poll(queue, 2, { active: true });
    expect(queue.size).toBe(2);

    // Still neutral: no additional report (de-duped).
    capture.poll(queue, 3, { active: true });
    expect(queue.size).toBe(2);

    // Now make pad1 non-neutral; subsequent polls should emit from pad1.
    pad1 = { ...pad1, buttons: makeButtons([1]) };
    capture.poll(queue, 4, { active: true });
    expect(queue.size).toBe(3);

    const state: { posted: InputBatchMessage | null } = { posted: null };
    const target: InputBatchTarget = {
      postMessage: (msg, _transfer) => {
        state.posted = msg;
      },
    };
    queue.flush(target);
    if (!state.posted) throw new Error("expected flush to post a batch");

    const words = new Int32Array(state.posted.buffer);
    expect(words[0]).toBe(3);

    const expectedPad0 = packGamepadReport({ buttons: 1 << 0, hat: GAMEPAD_HAT_NEUTRAL, x: 0, y: 0, rx: 0, ry: 0 });
    const expectedNeutral = packGamepadReport({ buttons: 0, hat: GAMEPAD_HAT_NEUTRAL, x: 0, y: 0, rx: 0, ry: 0 });
    const expectedPad1 = packGamepadReport({ buttons: 1 << 1, hat: GAMEPAD_HAT_NEUTRAL, x: 0, y: 0, rx: 0, ry: 0 });

    const base = 2;

    const ev0 = base + 0 * 4;
    expect(words[ev0]).toBe(InputEventType.GamepadReport);
    expect(words[ev0 + 1]).toBe(1);
    expect(words[ev0 + 2] >>> 0).toBe(expectedPad0.packedLo >>> 0);
    expect(words[ev0 + 3] >>> 0).toBe(expectedPad0.packedHi >>> 0);

    const ev1 = base + 1 * 4;
    expect(words[ev1]).toBe(InputEventType.GamepadReport);
    expect(words[ev1 + 1]).toBe(2);
    expect(words[ev1 + 2] >>> 0).toBe(expectedNeutral.packedLo >>> 0);
    expect(words[ev1 + 3] >>> 0).toBe(expectedNeutral.packedHi >>> 0);

    const ev2 = base + 2 * 4;
    expect(words[ev2]).toBe(InputEventType.GamepadReport);
    expect(words[ev2 + 1]).toBe(4);
    expect(words[ev2 + 2] >>> 0).toBe(expectedPad1.packedLo >>> 0);
    expect(words[ev2 + 3] >>> 0).toBe(expectedPad1.packedHi >>> 0);
  });
});

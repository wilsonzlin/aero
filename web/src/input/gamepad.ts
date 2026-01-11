import { InputEventQueue } from "./event_queue";

export const GAMEPAD_REPORT_SIZE_BYTES = 8;

// HID Hat Switch convention:
//   0 = up, 1 = up-right, 2 = right, ... 7 = up-left, 8 = neutral
export const GAMEPAD_HAT_NEUTRAL = 8;

export type GamepadButtonLike = {
  readonly pressed: boolean;
};

export type BrowserGamepadLike = {
  readonly buttons: readonly GamepadButtonLike[];
  readonly axes: readonly number[];
  readonly index?: number;
  readonly connected?: boolean;
};

export interface GamepadReportFields {
  /** 16-bit button bitfield (excluding d-pad; see `gamepadButtonsToBitfield`). */
  buttons: number;
  /** Hat switch value; see `GAMEPAD_HAT_NEUTRAL`. */
  hat: number;
  /** X axis (i8). */
  x: number;
  /** Y axis (i8). */
  y: number;
  /** Right stick X axis (i8). */
  rx: number;
  /** Right stick Y axis (i8). */
  ry: number;
}

export interface PackedGamepadReport {
  packedLo: number;
  packedHi: number;
}

export interface DecodedGamepadReport {
  buttons: number;
  hat: number;
  x: number;
  y: number;
  rx: number;
  ry: number;
}

/**
 * Converts a normalized Gamepad axis (typically in [-1, 1]) to a signed 8-bit
 * integer in [-127, 127] with a deadzone around 0.
 */
export function quantizeGamepadAxis(value: number, deadzone: number): number {
  if (!Number.isFinite(value)) return 0;
  const clamped = Math.max(-1, Math.min(1, value));
  const dz = Math.max(0, Math.min(1, deadzone));
  if (Math.abs(clamped) < dz) return 0;
  const q = Math.round(clamped * 127);
  return Math.max(-127, Math.min(127, q)) | 0;
}

export function computeGamepadHat(up: boolean, right: boolean, down: boolean, left: boolean): number {
  // Impossible / conflicting combinations: treat as neutral.
  if ((up && down) || (left && right)) return GAMEPAD_HAT_NEUTRAL;

  if (up) {
    if (right) return 1;
    if (left) return 7;
    return 0;
  }
  if (down) {
    if (right) return 3;
    if (left) return 5;
    return 4;
  }
  if (right) return 2;
  if (left) return 6;
  return GAMEPAD_HAT_NEUTRAL;
}

// Standard Gamepad mapping (https://w3c.github.io/gamepad/#remapping) button indices:
//   0=A, 1=B, 2=X, 3=Y, 4=LB, 5=RB, 6=LT, 7=RT, 8=Back, 9=Start,
//   10=LStick, 11=RStick, 12..15=D-pad, 16=Guide.
// Bit positions are chosen to preserve this ordering (bit0=A, bit1=B, ...),
// with the d-pad excluded (encoded as hat) and `Guide` mapped to bit12.
//
// We exclude d-pad (12..15) from the button bitfield because it is encoded as a
// hat switch.
const BUTTON_INDEX_TO_BIT: ReadonlyArray<readonly [index: number, bit: number]> = [
  [0, 0],
  [1, 1],
  [2, 2],
  [3, 3],
  [4, 4],
  [5, 5],
  [6, 6],
  [7, 7],
  [8, 8],
  [9, 9],
  [10, 10],
  [11, 11],
  [16, 12],
  // Optional extra buttons (e.g. "touchpad" on DualShock).
  [17, 13],
  [18, 14],
  [19, 15],
];

export function gamepadButtonsToBitfield(buttons: readonly GamepadButtonLike[]): number {
  let bits = 0;
  for (const [index, bit] of BUTTON_INDEX_TO_BIT) {
    if (buttons[index]?.pressed) {
      bits |= 1 << bit;
    }
  }
  return bits & 0xffff;
}

/**
 * 8-byte report layout (little-endian):
 *   (Must match `crates/emulator/src/io/usb/hid/gamepad.rs`.)
 *   Byte 0: buttons low 8
 *   Byte 1: buttons high 8
 *   Byte 2: hat (low 4 bits; 8=neutral/null)
 *   Byte 3: x (i8)
 *   Byte 4: y (i8)
 *   Byte 5: rx (i8)
 *   Byte 6: ry (i8)
 *   Byte 7: padding (0)
 */
export function packGamepadReport(fields: GamepadReportFields): PackedGamepadReport {
  const buttons = fields.buttons & 0xffff;
  const b0 = buttons & 0xff;
  const b1 = (buttons >>> 8) & 0xff;
  // Match emulator-side clamping semantics (`UsbHidGamepad::set_hat` / `set_axes`).
  const hat = Number.isFinite(fields.hat) && fields.hat >= 0 && fields.hat <= GAMEPAD_HAT_NEUTRAL ? fields.hat : GAMEPAD_HAT_NEUTRAL;
  const b2 = hat & 0x0f;
  const x = Math.max(-127, Math.min(127, fields.x | 0));
  const y = Math.max(-127, Math.min(127, fields.y | 0));
  const rx = Math.max(-127, Math.min(127, fields.rx | 0));
  const ry = Math.max(-127, Math.min(127, fields.ry | 0));
  const b3 = x & 0xff;
  const b4 = y & 0xff;
  const b5 = rx & 0xff;
  const b6 = ry & 0xff;
  const b7 = 0;

  const packedLo = (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) | 0;
  const packedHi = (b4 | (b5 << 8) | (b6 << 16) | (b7 << 24)) | 0;
  return { packedLo, packedHi };
}

export function unpackGamepadReport(packedLo: number, packedHi: number): Uint8Array {
  const out = new Uint8Array(GAMEPAD_REPORT_SIZE_BYTES);
  out[0] = packedLo & 0xff;
  out[1] = (packedLo >>> 8) & 0xff;
  out[2] = (packedLo >>> 16) & 0xff;
  out[3] = (packedLo >>> 24) & 0xff;
  out[4] = packedHi & 0xff;
  out[5] = (packedHi >>> 8) & 0xff;
  out[6] = (packedHi >>> 16) & 0xff;
  out[7] = (packedHi >>> 24) & 0xff;
  return out;
}

/**
 * Decodes a packed 8-byte HID gamepad report (`InputEventType.GamepadReport`)
 * into fields.
 *
 * Note: This is primarily intended for debugging/diagnostics and therefore
 * returns a new object.
 */
export function decodeGamepadReport(packedLo: number, packedHi: number): DecodedGamepadReport {
  const lo = packedLo | 0;
  const hi = packedHi | 0;
  return {
    buttons: lo & 0xffff,
    hat: (lo >>> 16) & 0x0f,
    x: lo >> 24,
    y: (hi << 24) >> 24,
    rx: (hi << 16) >> 24,
    ry: (hi << 8) >> 24,
  };
}

export function formatGamepadHat(hat: number): string {
  switch (hat & 0x0f) {
    case 0:
      return "up";
    case 1:
      return "up-right";
    case 2:
      return "right";
    case 3:
      return "down-right";
    case 4:
      return "down";
    case 5:
      return "down-left";
    case 6:
      return "left";
    case 7:
      return "up-left";
    default:
      return "neutral";
  }
}

export function encodeBrowserGamepadReport(gamepad: BrowserGamepadLike, deadzone: number): PackedGamepadReport {
  const buttons = gamepadButtonsToBitfield(gamepad.buttons);

  const up = gamepad.buttons[12]?.pressed ?? false;
  const down = gamepad.buttons[13]?.pressed ?? false;
  const left = gamepad.buttons[14]?.pressed ?? false;
  const right = gamepad.buttons[15]?.pressed ?? false;
  const hat = computeGamepadHat(up, right, down, left);

  const x = quantizeGamepadAxis(gamepad.axes[0] ?? 0, deadzone);
  const y = quantizeGamepadAxis(gamepad.axes[1] ?? 0, deadzone);
  const rx = quantizeGamepadAxis(gamepad.axes[2] ?? 0, deadzone);
  const ry = quantizeGamepadAxis(gamepad.axes[3] ?? 0, deadzone);

  return packGamepadReport({ buttons, hat, x, y, rx, ry });
}

export interface GamepadCaptureOptions {
  deadzone?: number;
  /**
   * Override `navigator.getGamepads()` (used for tests / shims).
   * If omitted, the capture will attempt to use `navigator.getGamepads` (or
   * `navigator.webkitGetGamepads`) when available.
   */
  getGamepads?: () => readonly (Gamepad | null)[];
}

export class GamepadCapture {
  private readonly deadzone: number;
  private readonly getGamepads: () => readonly (Gamepad | null)[];

  private activeIndex: number | null = null;

  private lastPackedLo: number;
  private lastPackedHi: number;

  constructor({ deadzone = 0.12, getGamepads }: GamepadCaptureOptions = {}) {
    this.deadzone = deadzone;
    this.getGamepads = getGamepads ?? defaultGetGamepads;

    // Treat the default state as already sent to avoid emitting redundant all-zero
    // reports on startup.
    const neutral = packGamepadReport({
      buttons: 0,
      hat: GAMEPAD_HAT_NEUTRAL,
      x: 0,
      y: 0,
      rx: 0,
      ry: 0,
    });
    this.lastPackedLo = neutral.packedLo;
    this.lastPackedHi = neutral.packedHi;
  }

  poll(queue: InputEventQueue, timestampUs: number, { active }: { active: boolean }): void {
    if (!active) {
      return;
    }

    const pad = this.getActiveGamepad();
    if (!pad) {
      this.emitNeutral(queue, timestampUs);
      return;
    }

    const { packedLo, packedHi } = encodeBrowserGamepadReport(pad, this.deadzone);
    this.emitIfChanged(queue, timestampUs, packedLo, packedHi);
  }

  emitNeutral(queue: InputEventQueue, timestampUs: number): void {
    const neutral = packGamepadReport({
      buttons: 0,
      hat: GAMEPAD_HAT_NEUTRAL,
      x: 0,
      y: 0,
      rx: 0,
      ry: 0,
    });
    this.emitIfChanged(queue, timestampUs, neutral.packedLo, neutral.packedHi);
  }

  private emitIfChanged(queue: InputEventQueue, timestampUs: number, packedLo: number, packedHi: number): void {
    if ((packedLo | 0) === this.lastPackedLo && (packedHi | 0) === this.lastPackedHi) {
      return;
    }
    this.lastPackedLo = packedLo | 0;
    this.lastPackedHi = packedHi | 0;
    queue.pushGamepadReport(timestampUs, packedLo, packedHi);
  }

  private getActiveGamepad(): Gamepad | null {
    let pads: readonly (Gamepad | null)[];
    try {
      pads = this.getGamepads();
    } catch {
      pads = [];
    }

    if (this.activeIndex !== null) {
      const candidate = pads[this.activeIndex] ?? null;
      if (candidate && (candidate.connected ?? true)) {
        return candidate;
      }
    }

    for (const gp of pads) {
      if (!gp) continue;
      if (gp.connected === false) continue;
      this.activeIndex = gp.index;
      return gp;
    }

    this.activeIndex = null;
    return null;
  }
}

function defaultGetGamepads(): readonly (Gamepad | null)[] {
  const nav = typeof navigator !== "undefined" ? navigator : undefined;
  if (!nav) return [];
  // Older Chromium exposed `webkitGetGamepads`; keep this best-effort.
  const getter: (() => readonly (Gamepad | null)[]) | undefined =
    nav.getGamepads?.bind(nav) ?? ((nav as Navigator & { webkitGetGamepads?: () => (Gamepad | null)[] }).webkitGetGamepads?.bind(nav) as
      | (() => (Gamepad | null)[])
      | undefined);
  if (!getter) return [];
  try {
    return getter();
  } catch {
    return [];
  }
}

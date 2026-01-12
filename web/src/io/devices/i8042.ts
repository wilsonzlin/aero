import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PortIoHandler } from "../bus/portio.ts";
import type { IrqSink } from "../device_manager.ts";

// i8042 status register bits.
const STATUS_OBF = 0x01; // Output buffer full.
const STATUS_IBF = 0x02; // Input buffer full.
const STATUS_SYS = 0x04; // System flag.
const STATUS_A2 = 0x08; // Last write was to command port (0x64).
const STATUS_AUX_OBF = 0x20; // Mouse output buffer full.

const OUTPUT_PORT_RESET = 0x01; // Bit 0 (active-low reset line)
const OUTPUT_PORT_A20 = 0x02; // Bit 1

// `aero-io-snapshot` header layout (16 bytes):
// - magic: "AERO"
// - format_version: u16 major, u16 minor
// - device_id: [u8;4]
// - device_version: u16 major, u16 minor
//
// The outer VM snapshot code uses `device_version` as `(DeviceState.version, DeviceState.flags)`,
// so JS-only device snapshots should follow the same header convention.
const IO_SNAPSHOT_FORMAT_VERSION_MAJOR = 1;
const IO_SNAPSHOT_FORMAT_VERSION_MINOR = 0;
const IO_SNAPSHOT_DEVICE_VERSION_MAJOR = 1;
const IO_SNAPSHOT_DEVICE_VERSION_MINOR = 0;

const MAX_CONTROLLER_OUTPUT_QUEUE = 1024;
const MAX_KEYBOARD_OUTPUT_QUEUE = 1024;
const MAX_MOUSE_OUTPUT_QUEUE = 1024;

type OutputSource = "controller" | "keyboard" | "mouse";

interface OutputByte {
  value: number;
  source: OutputSource;
}

export interface I8042SystemControlSink {
  setA20(enabled: boolean): void;
  requestReset(): void;
}

export interface I8042ControllerOptions {
  systemControl?: I8042SystemControlSink;
}

class ByteWriter {
  #buf: Uint8Array;
  #len = 0;

  constructor(initialCapacity = 256) {
    this.#buf = new Uint8Array(initialCapacity);
  }

  bytes(): Uint8Array {
    return this.#buf.slice(0, this.#len);
  }

  #ensure(additional: number): void {
    const required = this.#len + additional;
    if (required <= this.#buf.byteLength) return;
    let cap = this.#buf.byteLength;
    while (cap < required) cap *= 2;
    const next = new Uint8Array(cap);
    next.set(this.#buf);
    this.#buf = next;
  }

  u8(v: number): void {
    this.#ensure(1);
    this.#buf[this.#len++] = v & 0xff;
  }

  u16(v: number): void {
    this.#ensure(2);
    const x = v >>> 0;
    this.#buf[this.#len++] = x & 0xff;
    this.#buf[this.#len++] = (x >>> 8) & 0xff;
  }

  u32(v: number): void {
    this.#ensure(4);
    const x = v >>> 0;
    this.#buf[this.#len++] = x & 0xff;
    this.#buf[this.#len++] = (x >>> 8) & 0xff;
    this.#buf[this.#len++] = (x >>> 16) & 0xff;
    this.#buf[this.#len++] = (x >>> 24) & 0xff;
  }

  i32(v: number): void {
    this.u32(v | 0);
  }

  bytesRaw(bytes: Uint8Array): void {
    this.#ensure(bytes.byteLength);
    this.#buf.set(bytes, this.#len);
    this.#len += bytes.byteLength;
  }
}

class ByteReader {
  readonly #buf: Uint8Array;
  #off = 0;

  constructor(bytes: Uint8Array) {
    this.#buf = bytes;
  }

  remaining(): number {
    return this.#buf.byteLength - this.#off;
  }

  #normalizeLen(len: number): number {
    if (!Number.isFinite(len) || len < 0) {
      throw new Error(`i8042 snapshot requested an invalid byte length: ${String(len)}.`);
    }
    return Math.floor(len);
  }

  #need(n: number): void {
    if (this.#off + n > this.#buf.byteLength) {
      throw new Error(`i8042 snapshot is truncated (need ${n} bytes, have ${this.remaining()}).`);
    }
  }

  u8(): number {
    this.#need(1);
    return this.#buf[this.#off++]!;
  }

  u16(): number {
    this.#need(2);
    const a = this.#buf[this.#off++]!;
    const b = this.#buf[this.#off++]!;
    return (a | (b << 8)) >>> 0;
  }

  u32(): number {
    this.#need(4);
    const a = this.#buf[this.#off++]!;
    const b = this.#buf[this.#off++]!;
    const c = this.#buf[this.#off++]!;
    const d = this.#buf[this.#off++]!;
    return (a | (b << 8) | (c << 16) | (d << 24)) >>> 0;
  }

  i32(): number {
    return this.u32() | 0;
  }

  bytesRaw(len: number): Uint8Array {
    const n = this.#normalizeLen(len);
    this.#need(n);
    const out = this.#buf.subarray(this.#off, this.#off + n);
    this.#off += n;
    return out;
  }

  skip(len: number): void {
    const n = this.#normalizeLen(len);
    this.#need(n);
    this.#off += n;
  }
}

function encodeOutputSource(source: OutputSource): number {
  switch (source) {
    case "controller":
      return 0;
    case "keyboard":
      return 1;
    case "mouse":
      return 2;
    default: {
      const neverSource: never = source;
      throw new Error(`Unknown OutputSource: ${String(neverSource)}`);
    }
  }
}

function decodeOutputSource(code: number): OutputSource {
  switch (code & 0xff) {
    case 0:
      return "controller";
    case 1:
      return "keyboard";
    case 2:
      return "mouse";
    default:
      return "controller";
  }
}

/**
 * Set-2 -> Set-1 translation state, used when i8042 command-byte bit 6 is set.
 *
 * Ported from `crates/aero-devices-input/src/i8042.rs`.
 */
class Set2ToSet1 {
  sawE0 = false;
  sawF0 = false;

  feed(byte: number): number[] {
    const b = byte & 0xff;
    switch (b) {
      case 0xe0:
        this.sawE0 = true;
        return [0xe0];
      case 0xe1:
        // Pause/Break sequence.
        this.sawE0 = false;
        this.sawF0 = false;
        return [0xe1];
      case 0xf0:
        this.sawF0 = true;
        return [];
      default: {
        const extended = this.sawE0;
        const breakCode = this.sawF0;
        this.sawE0 = false;
        this.sawF0 = false;

        let out = set2ToSet1(b, extended);
        if (breakCode) out |= 0x80;
        return [out & 0xff];
      }
    }
  }
}

function set2ToSet1(code: number, extended: boolean): number {
  const c = code & 0xff;
  const e = Boolean(extended);

  // Flatten (code, extended) into a single switch key for speed.
  // eslint-disable-next-line @typescript-eslint/switch-exhaustiveness-check
  switch ((c << 1) | (e ? 1 : 0)) {
    // Non-extended
    case (0x76 << 1) | 0:
      return 0x01; // Esc
    case (0x16 << 1) | 0:
      return 0x02; // 1
    case (0x1e << 1) | 0:
      return 0x03; // 2
    case (0x26 << 1) | 0:
      return 0x04; // 3
    case (0x25 << 1) | 0:
      return 0x05; // 4
    case (0x2e << 1) | 0:
      return 0x06; // 5
    case (0x36 << 1) | 0:
      return 0x07; // 6
    case (0x3d << 1) | 0:
      return 0x08; // 7
    case (0x3e << 1) | 0:
      return 0x09; // 8
    case (0x46 << 1) | 0:
      return 0x0a; // 9
    case (0x45 << 1) | 0:
      return 0x0b; // 0
    case (0x4e << 1) | 0:
      return 0x0c; // -
    case (0x55 << 1) | 0:
      return 0x0d; // =
    case (0x66 << 1) | 0:
      return 0x0e; // Backspace
    case (0x0d << 1) | 0:
      return 0x0f; // Tab
    case (0x15 << 1) | 0:
      return 0x10; // Q
    case (0x1d << 1) | 0:
      return 0x11; // W
    case (0x24 << 1) | 0:
      return 0x12; // E
    case (0x2d << 1) | 0:
      return 0x13; // R
    case (0x2c << 1) | 0:
      return 0x14; // T
    case (0x35 << 1) | 0:
      return 0x15; // Y
    case (0x3c << 1) | 0:
      return 0x16; // U
    case (0x43 << 1) | 0:
      return 0x17; // I
    case (0x44 << 1) | 0:
      return 0x18; // O
    case (0x4d << 1) | 0:
      return 0x19; // P
    case (0x54 << 1) | 0:
      return 0x1a; // [
    case (0x5b << 1) | 0:
      return 0x1b; // ]
    case (0x5a << 1) | 0:
      return 0x1c; // Enter
    case (0x14 << 1) | 0:
      return 0x1d; // Left Ctrl
    case (0x1c << 1) | 0:
      return 0x1e; // A
    case (0x1b << 1) | 0:
      return 0x1f; // S
    case (0x23 << 1) | 0:
      return 0x20; // D
    case (0x2b << 1) | 0:
      return 0x21; // F
    case (0x34 << 1) | 0:
      return 0x22; // G
    case (0x33 << 1) | 0:
      return 0x23; // H
    case (0x3b << 1) | 0:
      return 0x24; // J
    case (0x42 << 1) | 0:
      return 0x25; // K
    case (0x4b << 1) | 0:
      return 0x26; // L
    case (0x4c << 1) | 0:
      return 0x27; // ;
    case (0x52 << 1) | 0:
      return 0x28; // '
    case (0x0e << 1) | 0:
      return 0x29; // `
    case (0x12 << 1) | 0:
      return 0x2a; // Left Shift
    case (0x5d << 1) | 0:
      return 0x2b; // \
    case (0x1a << 1) | 0:
      return 0x2c; // Z
    case (0x22 << 1) | 0:
      return 0x2d; // X
    case (0x21 << 1) | 0:
      return 0x2e; // C
    case (0x2a << 1) | 0:
      return 0x2f; // V
    case (0x32 << 1) | 0:
      return 0x30; // B
    case (0x31 << 1) | 0:
      return 0x31; // N
    case (0x3a << 1) | 0:
      return 0x32; // M
    case (0x41 << 1) | 0:
      return 0x33; // ,
    case (0x49 << 1) | 0:
      return 0x34; // .
    case (0x4a << 1) | 0:
      return 0x35; // /
    case (0x59 << 1) | 0:
      return 0x36; // Right Shift
    case (0x11 << 1) | 0:
      return 0x38; // Left Alt
    case (0x29 << 1) | 0:
      return 0x39; // Space
    case (0x58 << 1) | 0:
      return 0x3a; // CapsLock
    case (0x05 << 1) | 0:
      return 0x3b; // F1
    case (0x06 << 1) | 0:
      return 0x3c; // F2
    case (0x04 << 1) | 0:
      return 0x3d; // F3
    case (0x0c << 1) | 0:
      return 0x3e; // F4
    case (0x03 << 1) | 0:
      return 0x3f; // F5
    case (0x0b << 1) | 0:
      return 0x40; // F6
    case (0x83 << 1) | 0:
      return 0x41; // F7
    case (0x0a << 1) | 0:
      return 0x42; // F8
    case (0x01 << 1) | 0:
      return 0x43; // F9
    case (0x09 << 1) | 0:
      return 0x44; // F10
    case (0x78 << 1) | 0:
      return 0x57; // F11
    case (0x07 << 1) | 0:
      return 0x58; // F12
    case (0x77 << 1) | 0:
      return 0x45; // NumLock
    case (0x7e << 1) | 0:
      return 0x46; // ScrollLock
    case (0x6c << 1) | 0:
      return 0x47; // Numpad7
    case (0x75 << 1) | 0:
      return 0x48; // Numpad8
    case (0x7d << 1) | 0:
      return 0x49; // Numpad9
    case (0x7b << 1) | 0:
      return 0x4a; // NumpadSubtract
    case (0x6b << 1) | 0:
      return 0x4b; // Numpad4
    case (0x73 << 1) | 0:
      return 0x4c; // Numpad5
    case (0x74 << 1) | 0:
      return 0x4d; // Numpad6
    case (0x79 << 1) | 0:
      return 0x4e; // NumpadAdd
    case (0x69 << 1) | 0:
      return 0x4f; // Numpad1
    case (0x72 << 1) | 0:
      return 0x50; // Numpad2
    case (0x7a << 1) | 0:
      return 0x51; // Numpad3
    case (0x70 << 1) | 0:
      return 0x52; // Numpad0
    case (0x71 << 1) | 0:
      return 0x53; // NumpadDecimal
    case (0x7c << 1) | 0:
      return 0x37; // NumpadMultiply
    case (0x61 << 1) | 0:
      return 0x56; // IntlBackslash (ISO 102-key)

    // Extended
    case (0x14 << 1) | 1:
      return 0x1d; // Right Ctrl
    case (0x11 << 1) | 1:
      return 0x38; // Right Alt
    case (0x75 << 1) | 1:
      return 0x48; // Up
    case (0x72 << 1) | 1:
      return 0x50; // Down
    case (0x6b << 1) | 1:
      return 0x4b; // Left
    case (0x74 << 1) | 1:
      return 0x4d; // Right
    case (0x6c << 1) | 1:
      return 0x47; // Home
    case (0x69 << 1) | 1:
      return 0x4f; // End
    case (0x7d << 1) | 1:
      return 0x49; // PageUp
    case (0x7a << 1) | 1:
      return 0x51; // PageDown
    case (0x70 << 1) | 1:
      return 0x52; // Insert
    case (0x71 << 1) | 1:
      return 0x53; // Delete
    case (0x5a << 1) | 1:
      return 0x1c; // Numpad Enter
    case (0x4a << 1) | 1:
      return 0x35; // Numpad Divide
    case (0x1f << 1) | 1:
      return 0x5b; // Left Meta / Windows
    case (0x27 << 1) | 1:
      return 0x5c; // Right Meta / Windows
    case (0x2f << 1) | 1:
      return 0x5d; // ContextMenu
    case (0x12 << 1) | 1:
      return 0x2a; // PrintScreen sequence
    case (0x7c << 1) | 1:
      return 0x37; // PrintScreen sequence

    default:
      return c;
  }
}

class Ps2Keyboard {
  scancodeSet = 2;
  leds = 0;
  typematicDelay = 0x0b;
  typematicRate = 0x0b;
  scanningEnabled = true;
  expectingData = false;
  lastCommand = 0;
  readonly outQueue: number[] = [];

  resetDefaults(): void {
    this.scancodeSet = 2;
    this.leds = 0;
    this.typematicDelay = 0x0b;
    this.typematicRate = 0x0b;
    this.scanningEnabled = true;
    this.expectingData = false;
    this.lastCommand = 0;
    this.outQueue.length = 0;
  }

  injectScancodes(bytes: Uint8Array): void {
    if (!this.scanningEnabled) return;
    // InputCapture produces Set-2 sequences. If the guest switched the device to
    // a different set, we currently drop injected bytes (matching the Rust model).
    if (this.scancodeSet !== 2) return;
    for (const b of bytes) {
      if (this.outQueue.length >= MAX_KEYBOARD_OUTPUT_QUEUE) break;
      this.outQueue.push(b & 0xff);
    }
  }

  receiveByte(byte: number): void {
    const b = byte & 0xff;
    if (this.expectingData) {
      this.expectingData = false;
      this.#handleCommandData(this.lastCommand, b);
      return;
    }
    this.#handleCommand(b);
  }

  popOutputByte(): number | null {
    const b = this.outQueue.shift();
    return typeof b === "number" ? (b & 0xff) : null;
  }

  #queueByte(b: number): void {
    if (this.outQueue.length >= MAX_KEYBOARD_OUTPUT_QUEUE) return;
    this.outQueue.push(b & 0xff);
  }

  #handleCommand(cmd: number): void {
    switch (cmd & 0xff) {
      case 0xed: // Set LEDs (next byte)
        this.expectingData = true;
        this.lastCommand = cmd & 0xff;
        this.#queueByte(0xfa);
        return;
      case 0xee: // Echo
        this.#queueByte(0xee);
        return;
      case 0xf0: // Get/Set scancode set (next byte)
        this.expectingData = true;
        this.lastCommand = cmd & 0xff;
        this.#queueByte(0xfa);
        return;
      case 0xf2: // Identify
        this.#queueByte(0xfa);
        this.#queueByte(0xab);
        this.#queueByte(0x83);
        return;
      case 0xf3: // Set typematic rate/delay (next byte)
        this.expectingData = true;
        this.lastCommand = cmd & 0xff;
        this.#queueByte(0xfa);
        return;
      case 0xf4: // Enable scanning
        this.scanningEnabled = true;
        this.#queueByte(0xfa);
        return;
      case 0xf5: // Disable scanning
        this.scanningEnabled = false;
        this.#queueByte(0xfa);
        return;
      case 0xf6: // Set defaults
        this.scancodeSet = 2;
        this.typematicDelay = 0x0b;
        this.typematicRate = 0x0b;
        this.scanningEnabled = true;
        this.expectingData = false;
        this.lastCommand = 0;
        this.#queueByte(0xfa);
        return;
      case 0xff: // Reset
        this.resetDefaults();
        this.#queueByte(0xfa);
        this.#queueByte(0xaa);
        return;
      default:
        // ACK unknown commands by default.
        this.#queueByte(0xfa);
        return;
    }
  }

  #handleCommandData(cmd: number, data: number): void {
    switch (cmd & 0xff) {
      case 0xed: // Set LEDs
        this.leds = data & 0x07;
        this.#queueByte(0xfa);
        return;
      case 0xf0: {
        // Get/set scancode set.
        const set = data & 0xff;
        if (set === 0x00) {
          this.#queueByte(0xfa);
          this.#queueByte(this.scancodeSet & 0xff);
          return;
        }
        if (set === 0x01 || set === 0x02 || set === 0x03) {
          this.scancodeSet = set;
        }
        this.#queueByte(0xfa);
        return;
      }
      case 0xf3: {
        // Set typematic rate/delay. Keep the raw components; callers may choose to interpret.
        const delay = (data >>> 5) & 0x03;
        const rate = data & 0x1f;
        this.typematicDelay = delay & 0xff;
        this.typematicRate = rate & 0xff;
        this.#queueByte(0xfa);
        return;
      }
      default:
        this.#queueByte(0xfa);
        return;
    }
  }

  saveState(w: ByteWriter): void {
    w.u8(this.scancodeSet);
    w.u8(this.leds);
    w.u8(this.typematicDelay);
    w.u8(this.typematicRate);
    w.u8(this.scanningEnabled ? 1 : 0);
    w.u8(this.expectingData ? 1 : 0);
    w.u8(this.lastCommand);
    w.u8(0); // padding

    const len = Math.min(this.outQueue.length, MAX_KEYBOARD_OUTPUT_QUEUE);
    w.u32(len);
    for (let i = 0; i < len; i++) w.u8(this.outQueue[i]!);
  }

  loadState(r: ByteReader): void {
    this.scancodeSet = r.u8() & 0xff;
    this.leds = r.u8() & 0xff;
    this.typematicDelay = r.u8() & 0xff;
    this.typematicRate = r.u8() & 0xff;
    this.scanningEnabled = (r.u8() & 1) !== 0;
    this.expectingData = (r.u8() & 1) !== 0;
    this.lastCommand = r.u8() & 0xff;
    r.u8(); // padding

    const lenRaw = r.u32();
    const len = Math.min(lenRaw, MAX_KEYBOARD_OUTPUT_QUEUE);
    this.outQueue.length = 0;
    for (let i = 0; i < len; i++) this.outQueue.push(r.u8() & 0xff);
    if (lenRaw > len) r.skip(lenRaw - len);
  }
}

type MouseMode = "stream" | "remote" | "wrap";
type MouseScaling = "linear" | "double";

function encodeMouseMode(mode: MouseMode): number {
  switch (mode) {
    case "stream":
      return 0;
    case "remote":
      return 1;
    case "wrap":
      return 2;
    default: {
      const neverMode: never = mode;
      throw new Error(`Unknown MouseMode: ${String(neverMode)}`);
    }
  }
}

function decodeMouseMode(code: number): MouseMode {
  switch (code & 0xff) {
    case 0:
      return "stream";
    case 1:
      return "remote";
    case 2:
      return "wrap";
    default:
      return "stream";
  }
}

function encodeMouseScaling(s: MouseScaling): number {
  return s === "double" ? 1 : 0;
}

function decodeMouseScaling(code: number): MouseScaling {
  return (code & 0xff) === 1 ? "double" : "linear";
}

class Ps2Mouse {
  mode: MouseMode = "stream";
  scaling: MouseScaling = "linear";
  resolution = 4;
  sampleRate = 100;
  reportingEnabled = false;
  deviceId = 0x00;
  buttons = 0;
  dx = 0;
  dy = 0;
  dz = 0;
  readonly sampleRateSeq: number[] = [];
  expectingData = false;
  lastCommand = 0;
  readonly outQueue: number[] = [];

  resetDefaults(): void {
    this.mode = "stream";
    this.scaling = "linear";
    this.resolution = 4;
    this.sampleRate = 100;
    this.reportingEnabled = false;
    this.deviceId = 0x00;
    this.buttons = 0;
    this.dx = 0;
    this.dy = 0;
    this.dz = 0;
    this.sampleRateSeq.length = 0;
    this.expectingData = false;
    this.lastCommand = 0;
    this.outQueue.length = 0;
  }

  receiveByte(byte: number): void {
    const b = byte & 0xff;
    if (this.expectingData) {
      this.expectingData = false;
      this.#handleCommandData(this.lastCommand, b);
      return;
    }
    this.#handleCommand(b);
  }

  movement(dx: number, dy: number, dz = 0): void {
    this.dx += dx | 0;
    this.dy += dy | 0;
    this.dz += dz | 0;

    if (this.mode === "stream" && this.reportingEnabled) {
      this.#sendMovementPacket();
    }
  }

  setButtons(buttonMask: number): void {
    this.buttons = buttonMask & 0xff;
    if (this.mode === "stream" && this.reportingEnabled) {
      this.#sendMovementPacket();
    }
  }

  popOutputByte(): number | null {
    const b = this.outQueue.shift();
    return typeof b === "number" ? (b & 0xff) : null;
  }

  #queueByte(b: number): void {
    if (this.outQueue.length >= MAX_MOUSE_OUTPUT_QUEUE) return;
    this.outQueue.push(b & 0xff);
  }

  #statusByte(): number {
    // Bit0/1/2 = buttons, bit3=always 1, bit4=scale21, bit5=data reporting.
    let st = (this.buttons & 0x07) | 0x08;
    if (this.scaling === "double") st |= 0x10;
    if (this.reportingEnabled) st |= 0x20;
    // bits6/7 reserved
    return st & 0xff;
  }

  #recordSampleRate(rate: number): void {
    this.sampleRateSeq.push(rate & 0xff);
    while (this.sampleRateSeq.length > 3) this.sampleRateSeq.shift();

    if (this.sampleRateSeq.length === 3) {
      const [a, b, c] = this.sampleRateSeq;
      // IntelliMouse (wheel)
      if (a === 200 && b === 100 && c === 80) this.deviceId = 0x03;
      // IntelliMouse Explorer (5-button)
      else if (a === 200 && b === 200 && c === 80) this.deviceId = 0x04;
    }
  }

  #handleCommand(cmd: number): void {
    switch (cmd & 0xff) {
      case 0xe6: // Set scaling 1:1
        this.scaling = "linear";
        this.#queueByte(0xfa);
        return;
      case 0xe7: // Set scaling 2:1
        this.scaling = "double";
        this.#queueByte(0xfa);
        return;
      case 0xe8: // Set resolution (next byte)
        this.expectingData = true;
        this.lastCommand = cmd & 0xff;
        this.#queueByte(0xfa);
        return;
      case 0xe9: // Status request
        this.#queueByte(0xfa);
        this.#queueByte(this.#statusByte());
        this.#queueByte(this.resolution & 0xff);
        this.#queueByte(this.sampleRate & 0xff);
        return;
      case 0xea: // Set stream mode
        this.mode = "stream";
        this.#queueByte(0xfa);
        return;
      case 0xeb: // Read data (remote mode)
        this.#queueByte(0xfa);
        this.#sendMovementPacket();
        return;
      case 0xec: // Reset wrap mode
        this.#queueByte(0xfa);
        return;
      case 0xee: // Set wrap mode
        this.mode = "wrap";
        this.#queueByte(0xfa);
        return;
      case 0xf0: // Set remote mode
        this.mode = "remote";
        this.#queueByte(0xfa);
        return;
      case 0xf2: // Get device ID
        this.#queueByte(0xfa);
        this.#queueByte(this.deviceId & 0xff);
        return;
      case 0xf3: // Set sample rate (next byte)
        this.expectingData = true;
        this.lastCommand = cmd & 0xff;
        this.#queueByte(0xfa);
        return;
      case 0xf4: // Enable data reporting
        this.reportingEnabled = true;
        this.#queueByte(0xfa);
        return;
      case 0xf5: // Disable data reporting
        this.reportingEnabled = false;
        this.#queueByte(0xfa);
        return;
      case 0xf6: // Set defaults
        this.resetDefaults();
        this.#queueByte(0xfa);
        return;
      case 0xff: // Reset
        this.resetDefaults();
        this.#queueByte(0xfa);
        this.#queueByte(0xaa);
        this.#queueByte(0x00);
        return;
      default:
        this.#queueByte(0xfa);
        return;
    }
  }

  #handleCommandData(cmd: number, data: number): void {
    switch (cmd & 0xff) {
      case 0xe8: // Set resolution
        this.resolution = data & 0xff;
        this.#queueByte(0xfa);
        return;
      case 0xf3: // Set sample rate
        this.sampleRate = data & 0xff;
        this.#recordSampleRate(this.sampleRate);
        this.#queueByte(0xfa);
        return;
      default:
        this.#queueByte(0xfa);
        return;
    }
  }

  #sendMovementPacket(): void {
    // PS/2 packet uses 9-bit signed deltas with explicit sign bits.
    const dx = Math.max(-256, Math.min(255, this.dx | 0));
    const dy = Math.max(-256, Math.min(255, this.dy | 0));

    let b0 = (this.buttons & 0x07) | 0x08;
    if (dx < 0) b0 |= 0x10;
    if (dy < 0) b0 |= 0x20;
    // Overflow bits are ignored for now.

    this.#queueByte(b0);
    this.#queueByte(dx & 0xff);
    this.#queueByte(dy & 0xff);

    // IntelliMouse wheel / extra buttons.
    if (this.deviceId === 0x03 || this.deviceId === 0x04) {
      const dz = Math.max(-8, Math.min(7, this.dz | 0));
      this.#queueByte(dz & 0x0f);
    }

    this.dx = 0;
    this.dy = 0;
    this.dz = 0;
  }

  saveState(w: ByteWriter): void {
    w.u8(encodeMouseMode(this.mode));
    w.u8(encodeMouseScaling(this.scaling));
    w.u8(this.resolution);
    w.u8(this.sampleRate);
    w.u8(this.reportingEnabled ? 1 : 0);
    w.u8(this.deviceId);
    w.u8(this.buttons);
    w.u8(this.expectingData ? 1 : 0);
    w.u8(this.lastCommand);
    const seqLen = Math.min(this.sampleRateSeq.length, 3);
    w.u8(seqLen);
    for (let i = 0; i < seqLen; i++) w.u8(this.sampleRateSeq[i]!);
    for (let i = seqLen; i < 3; i++) w.u8(0); // padding to fixed 3 bytes
    w.i32(this.dx);
    w.i32(this.dy);
    w.i32(this.dz);

    const outLen = Math.min(this.outQueue.length, MAX_MOUSE_OUTPUT_QUEUE);
    w.u32(outLen);
    for (let i = 0; i < outLen; i++) w.u8(this.outQueue[i]!);
  }

  loadState(r: ByteReader): void {
    this.mode = decodeMouseMode(r.u8());
    this.scaling = decodeMouseScaling(r.u8());
    this.resolution = r.u8() & 0xff;
    this.sampleRate = r.u8() & 0xff;
    this.reportingEnabled = (r.u8() & 1) !== 0;
    this.deviceId = r.u8() & 0xff;
    this.buttons = r.u8() & 0xff;
    this.expectingData = (r.u8() & 1) !== 0;
    this.lastCommand = r.u8() & 0xff;
    const seqLenRaw = r.u8() & 0xff;
    const seqLen = Math.min(seqLenRaw, 3);
    this.sampleRateSeq.length = 0;
    for (let i = 0; i < seqLen; i++) this.sampleRateSeq.push(r.u8() & 0xff);
    // Consume remaining fixed 3-byte slot.
    for (let i = seqLen; i < 3; i++) r.u8();
    this.dx = r.i32();
    this.dy = r.i32();
    this.dz = r.i32();

    const outLenRaw = r.u32();
    const outLen = Math.min(outLenRaw, MAX_MOUSE_OUTPUT_QUEUE);
    this.outQueue.length = 0;
    for (let i = 0; i < outLen; i++) this.outQueue.push(r.u8() & 0xff);
    if (outLenRaw > outLen) r.skip(outLenRaw - outLen);
  }
}

/**
 * Minimal i8042 PS/2 controller model sufficient for early boot and tests.
 *
 * Implemented:
 * - Ports 0x60 (data) and 0x64 (status/command)
 * - Controller commands: 0x20 (read command byte), 0x60 (write command byte),
 *   0xAA (self test), 0xD0/0xD1 (output port), 0xFE (reset pulse)
 * - Keyboard command: 0xFF (reset) -> 0xFA, 0xAA
 * - IRQ1/IRQ12 *edge* signalling when a keyboard/mouse byte becomes available and interrupts are enabled.
 *
 * IRQ semantics:
 * The real i8042 behaves like an edge-triggered source for the legacy PIC: it generates a pulse
 * when it loads a byte into the output buffer. To model that over the web runtime's
 * `raiseIrq`/`lowerIrq` API (which represents *line level transitions*), we emit an explicit
 * pulse (`raiseIrq` then `lowerIrq`) each time the head output byte changes to a keyboard byte
 * (IRQ1) or mouse byte (IRQ12).
 *
 * See `docs/irq-semantics.md`.
 */
export class I8042Controller implements PortIoHandler {
  static readonly MAX_CONTROLLER_OUTPUT_QUEUE = MAX_CONTROLLER_OUTPUT_QUEUE;
  static readonly MAX_KEYBOARD_OUTPUT_QUEUE = MAX_KEYBOARD_OUTPUT_QUEUE;
  static readonly MAX_MOUSE_OUTPUT_QUEUE = MAX_MOUSE_OUTPUT_QUEUE;

  readonly #irq: IrqSink;
  readonly #sysCtrl?: I8042SystemControlSink;

  #status = STATUS_SYS;
  // Default command byte matches the canonical Rust model:
  //  - IRQ1 enabled
  //  - system flag set
  //  - Set-2 -> Set-1 translation enabled
  #commandByte = 0x45;
  #pendingCommand: number | null = null;
  #lastWriteWasCommand = false;

  #outQueue: OutputByte[] = [];
  #irqLastHead: OutputByte | null = null;

  #outputPort = OUTPUT_PORT_RESET;

  readonly #keyboard = new Ps2Keyboard();
  readonly #mouse = new Ps2Mouse();
  readonly #translator = new Set2ToSet1();

  constructor(irq: IrqSink, opts: I8042ControllerOptions = {}) {
    this.#irq = irq;
    this.#sysCtrl = opts.systemControl;
  }

  portRead(port: number, size: number): number {
    if (size !== 1) return defaultReadValue(size);

    switch (port & 0xffff) {
      case 0x0060: {
        const item = this.#outQueue.shift() ?? null;
        this.#pumpDeviceQueues();
        this.#syncStatusAndIrq();
        return item ? item.value & 0xff : 0x00;
      }
      case 0x0064:
        return this.#readStatus();
      default:
        return defaultReadValue(size);
    }
  }

  portWrite(port: number, size: number, value: number): void {
    if (size !== 1) return;
    const v = value & 0xff;

    switch (port & 0xffff) {
      case 0x0064:
        this.#writeCommand(v);
        return;
      case 0x0060:
        this.#writeData(v);
        return;
      default:
        return;
    }
  }

  /**
   * Inject host keyboard scancode bytes into the controller output buffer.
   *
   * Bytes injected via this path are treated as coming from the keyboard device
   * (as opposed to controller replies), so IRQ1 signalling follows the command
   * byte interrupt-enable bit.
   */
  injectKeyboardBytes(bytes: Uint8Array): void {
    this.#keyboard.injectScancodes(bytes);
    this.#pumpDeviceQueues();
    this.#syncStatusAndIrq();
  }

  /**
   * Host-side injection API: relative mouse motion in PS/2 coordinate space.
   *
   * - dx: right is positive
   * - dy: up is positive (InputCapture already inverts DOM Y)
   * - wheel: positive is wheel up
   */
  injectMouseMotion(dx: number, dy: number, wheel: number): void {
    // Controller command 0xA7 sets command byte bit 5 to disable the mouse port.
    if ((this.#commandByte & 0x20) !== 0) return;
    // In stream mode, drop movement while reporting is disabled to avoid buffering host deltas.
    if (this.#mouse.mode === "stream" && !this.#mouse.reportingEnabled) return;

    let remX = dx | 0;
    let remY = dy | 0;
    let remWheel = wheel | 0;

    const deviceId = this.#mouse.deviceId & 0xff;
    const wheelEnabled = deviceId === 0x03 || deviceId === 0x04;
    if (!wheelEnabled) remWheel = 0;

    // Split into multiple packets so each axis fits in a signed 8-bit delta and wheel fits
    // in the IntelliMouse 4-bit signed nibble.
    while (remX !== 0 || remY !== 0 || remWheel !== 0) {
      const stepX = Math.max(-128, Math.min(127, remX));
      const stepY = Math.max(-128, Math.min(127, remY));
      const stepWheel = Math.max(-8, Math.min(7, remWheel));
      remX = (remX - stepX) | 0;
      remY = (remY - stepY) | 0;
      remWheel = (remWheel - stepWheel) | 0;
      this.#mouse.movement(stepX, stepY, stepWheel);
    }

    this.#pumpDeviceQueues();
    this.#syncStatusAndIrq();
  }

  /**
   * Inject relative mouse movement (PS/2 convention: positive Y = up).
   */
  injectMouseMove(dx: number, dy: number): void {
    this.injectMouseMotion(dx, dy, 0);
  }

  /**
   * Inject mouse wheel movement.
   */
  injectMouseWheel(dz: number): void {
    this.injectMouseMotion(0, 0, dz);
  }

  /**
   * Host-side injection API: set absolute mouse button state bitmask.
   *
   * Bits: 0=left, 1=right, 2=middle.
   */
  injectMouseButtons(buttonMask: number): void {
    if ((this.#commandByte & 0x20) !== 0) return;
    this.#mouse.setButtons(buttonMask & 0xff);
    this.#pumpDeviceQueues();
    this.#syncStatusAndIrq();
  }

  #readStatus(): number {
    let st = this.#status;
    if (this.#lastWriteWasCommand) st |= STATUS_A2;
    else st &= ~STATUS_A2;
    return st & 0xff;
  }
  #writeCommand(cmd: number): void {
    this.#lastWriteWasCommand = true;
    this.#status |= STATUS_IBF;
    try {
      switch (cmd & 0xff) {
        case 0x20: // Read command byte
          this.#enqueue(this.#commandByte, "controller");
          return;
        case 0x60: // Write command byte (next data byte)
          this.#pendingCommand = 0x60;
          return;
        case 0xaa: // Self test
          this.#enqueue(0x55, "controller");
          return;
        case 0xd0: // Read output port
          this.#enqueue(this.#outputPort, "controller");
          return;
        case 0xd1: // Write output port (next data byte)
          this.#pendingCommand = 0xd1;
          return;
        case 0xd2: // Write next data byte into output buffer (as keyboard data)
          this.#pendingCommand = 0xd2;
          return;
        case 0xd3: // Write next data byte into output buffer (as mouse data)
          this.#pendingCommand = 0xd3;
          return;
        case 0xa7: // Disable mouse port
          this.#commandByte |= 0x20;
          this.#pumpDeviceQueues();
          this.#syncStatusAndIrq();
          return;
        case 0xa8: // Enable mouse port
          this.#commandByte &= ~0x20;
          this.#pumpDeviceQueues();
          this.#syncStatusAndIrq();
          return;
        case 0xa9: // Test mouse port
          this.#enqueue(0x00, "controller");
          return;
        case 0xab: // Test keyboard port
          this.#enqueue(0x00, "controller");
          return;
        case 0xad: // Disable keyboard
          this.#commandByte |= 0x10;
          this.#syncStatusAndIrq();
          return;
        case 0xae: // Enable keyboard
          this.#commandByte &= ~0x10;
          this.#pumpDeviceQueues();
          this.#syncStatusAndIrq();
          return;
        case 0xd4: // Write to mouse (next data byte)
          this.#pendingCommand = 0xd4;
          return;
        case 0xdd: // Non-standard: disable A20 gate
          this.#setOutputPort(this.#outputPort & ~OUTPUT_PORT_A20);
          return;
        case 0xdf: // Non-standard: enable A20 gate
          this.#setOutputPort(this.#outputPort | OUTPUT_PORT_A20);
          return;
        case 0xfe: // Pulse output port bit 0 low (system reset)
          this.#sysCtrl?.requestReset();
          return;
        default:
          // Unknown/unimplemented controller command.
          return;
      }
    } finally {
      this.#status &= ~STATUS_IBF;
    }
  }

  #writeData(data: number): void {
    this.#lastWriteWasCommand = false;
    this.#status |= STATUS_IBF;
    try {
      if (this.#pendingCommand === 0x60) {
        this.#pendingCommand = null;
        this.#commandByte = data & 0xff;
        this.#pumpDeviceQueues();
        this.#syncStatusAndIrq();
        return;
      }

      if (this.#pendingCommand === 0xd1) {
        this.#pendingCommand = null;
        this.#setOutputPort(data);
        this.#syncStatusAndIrq();
        return;
      }

      if (this.#pendingCommand === 0xd2) {
        this.#pendingCommand = null;
        // Bypass translation and device state; this is a controller command.
        this.#enqueue(data, "keyboard");
        return;
      }

      if (this.#pendingCommand === 0xd3) {
        this.#pendingCommand = null;
        // Same as 0xD2, but marks the byte as mouse-originated (AUX).
        this.#enqueue(data, "mouse");
        return;
      }

      if (this.#pendingCommand === 0xd4) {
        this.#pendingCommand = null;
        this.#mouse.receiveByte(data);
        this.#pumpDeviceQueues();
        this.#syncStatusAndIrq();
        return;
      }

      // Default: send byte to PS/2 keyboard.
      this.#keyboard.receiveByte(data);
      this.#pumpDeviceQueues();
      this.#syncStatusAndIrq();
    } finally {
      this.#status &= ~STATUS_IBF;
    }
  }

  #translationEnabled(): boolean {
    return (this.#commandByte & 0x40) !== 0;
  }

  #keyboardPortEnabled(): boolean {
    // Bit 4: 0=enabled, 1=disabled.
    return (this.#commandByte & 0x10) === 0;
  }

  #mousePortEnabled(): boolean {
    // Bit 5: 0=enabled, 1=disabled.
    return (this.#commandByte & 0x20) === 0;
  }

  #enqueue(value: number, source: OutputSource): void {
    if (this.#outQueue.length >= MAX_CONTROLLER_OUTPUT_QUEUE) return;
    this.#outQueue.push({ value: value & 0xff, source });
    this.#syncStatusAndIrq();
  }

  #pumpDeviceQueues(): void {
    while (this.#outQueue.length < MAX_CONTROLLER_OUTPUT_QUEUE) {
      if (this.#keyboardPortEnabled()) {
        const kb = this.#keyboard.popOutputByte();
        if (kb !== null) {
          if (this.#translationEnabled()) {
            const outs = this.#translator.feed(kb);
            for (const out of outs) {
              if (this.#outQueue.length >= MAX_CONTROLLER_OUTPUT_QUEUE) break;
              this.#outQueue.push({ value: out & 0xff, source: "keyboard" });
            }
          } else {
            this.#outQueue.push({ value: kb & 0xff, source: "keyboard" });
          }
          // Either we pushed output bytes, or we consumed a prefix byte (F0) that
          // produced no output. In both cases, keep pumping.
          continue;
        }
      }

      if (this.#mousePortEnabled()) {
        const ms = this.#mouse.popOutputByte();
        if (ms !== null) {
          this.#outQueue.push({ value: ms & 0xff, source: "mouse" });
          continue;
        }
      }

      break;
    }
  }

  #syncStatusAndIrq(): void {
    if (this.#outQueue.length > 0) this.#status |= STATUS_OBF;
    else this.#status &= ~STATUS_OBF;

    const head = this.#outQueue[0] ?? null;
    const headSource = head?.source ?? null;
    if (headSource === "mouse") this.#status |= STATUS_AUX_OBF;
    else this.#status &= ~STATUS_AUX_OBF;

    // i8042 IRQs (IRQ1 keyboard, IRQ12 mouse) behave like edge-triggered sources for the legacy
    // PIC: the controller generates a pulse when it loads a byte into the output buffer.
    //
    // The web runtime transports IRQs as line level transitions (`raiseIrq`/`lowerIrq`), so we
    // represent the edge by emitting an explicit pulse each time the head output byte changes.
    if (head !== this.#irqLastHead) {
      this.#irqLastHead = head;
      if (headSource === "keyboard") {
        if ((this.#commandByte & 0x01) !== 0) {
          this.#irq.raiseIrq(1);
          this.#irq.lowerIrq(1);
        }
      } else if (headSource === "mouse") {
        if ((this.#commandByte & 0x02) !== 0) {
          this.#irq.raiseIrq(12);
          this.#irq.lowerIrq(12);
        }
      }
    }
  }

  saveState(): Uint8Array {
    const w = new ByteWriter(256);
    // `aero-io-snapshot` header.
    w.u8(0x41); // A
    w.u8(0x45); // E
    w.u8(0x52); // R
    w.u8(0x4f); // O
    w.u16(IO_SNAPSHOT_FORMAT_VERSION_MAJOR);
    w.u16(IO_SNAPSHOT_FORMAT_VERSION_MINOR);
    // device id = "8042"
    w.u8(0x38);
    w.u8(0x30);
    w.u8(0x34);
    w.u8(0x32);
    w.u16(IO_SNAPSHOT_DEVICE_VERSION_MAJOR);
    let flags = IO_SNAPSHOT_DEVICE_VERSION_MINOR;
    if (this.#lastWriteWasCommand) flags |= 1 << 0;
    if (this.#translator.sawE0) flags |= 1 << 1;
    if (this.#translator.sawF0) flags |= 1 << 2;
    w.u16(flags);

    w.u8(this.#status);
    w.u8(this.#commandByte);
    w.u8(this.#outputPort);
    w.u8(this.#pendingCommand === null ? 0xff : this.#pendingCommand & 0xff);

    const outLen = Math.min(this.#outQueue.length, MAX_CONTROLLER_OUTPUT_QUEUE);
    w.u32(outLen);
    for (let i = 0; i < outLen; i++) {
      const item = this.#outQueue[i]!;
      w.u8(item.value);
      w.u8(encodeOutputSource(item.source));
    }

    this.#keyboard.saveState(w);
    this.#mouse.saveState(w);

    return w.bytes();
  }

  loadState(bytes: Uint8Array): void {
    const MAX_SNAPSHOT_BYTES = 256 * 1024;
    if (bytes.byteLength > MAX_SNAPSHOT_BYTES) {
      throw new Error(`i8042 snapshot too large: ${bytes.byteLength} bytes (max ${MAX_SNAPSHOT_BYTES}).`);
    }

    const r = new ByteReader(bytes);
    const m0 = r.u8();
    const m1 = r.u8();
    const m2 = r.u8();
    const m3 = r.u8();
    if (m0 !== 0x41 || m1 !== 0x45 || m2 !== 0x52 || m3 !== 0x4f) {
      throw new Error("i8042 snapshot has invalid magic (expected AERO).");
    }

    const formatMajor = r.u16();
    const formatMinor = r.u16();
    if (formatMajor !== IO_SNAPSHOT_FORMAT_VERSION_MAJOR) {
      throw new Error(`Unsupported i8042 snapshot format version: ${formatMajor}.${formatMinor}.`);
    }

    const id0 = r.u8();
    const id1 = r.u8();
    const id2 = r.u8();
    const id3 = r.u8();
    if (id0 !== 0x38 || id1 !== 0x30 || id2 !== 0x34 || id3 !== 0x32) {
      throw new Error("i8042 snapshot has unexpected device id (expected 8042).");
    }

    const deviceMajor = r.u16();
    const deviceMinor = r.u16();
    if (deviceMajor !== IO_SNAPSHOT_DEVICE_VERSION_MAJOR) {
      throw new Error(`Unsupported i8042 snapshot device version: ${deviceMajor}.${deviceMinor}.`);
    }
    const flags = deviceMinor & 0xffff;
    this.#lastWriteWasCommand = (flags & (1 << 0)) !== 0;
    this.#translator.sawE0 = (flags & (1 << 1)) !== 0;
    this.#translator.sawF0 = (flags & (1 << 2)) !== 0;

    this.#status = r.u8() & 0xff;
    this.#commandByte = r.u8() & 0xff;
    this.#outputPort = r.u8() & 0xff;
    const pending = r.u8() & 0xff;
    this.#pendingCommand = pending === 0xff ? null : pending;

    // `#outputPort` controls platform A20 gate state (bit 1). When restoring from a snapshot,
    // resynchronize the sink so the rest of the VM observes the same A20 state as the restored
    // output-port image. Snapshot restore should not request a reset even if the reset bit is low.
    const a20Enabled = (this.#outputPort & OUTPUT_PORT_A20) !== 0;
    try {
      this.#sysCtrl?.setA20(a20Enabled);
    } catch {
      // Ignore system control errors during snapshot restore; the VM can still continue.
    }

    this.#outQueue.length = 0;
    const outLenRaw = r.u32();
    const outLen = Math.min(outLenRaw, MAX_CONTROLLER_OUTPUT_QUEUE);
    for (let i = 0; i < outLen; i++) {
      const value = r.u8() & 0xff;
      const source = decodeOutputSource(r.u8());
      this.#outQueue.push({ value, source });
    }
    if (outLenRaw > outLen) {
      // Each entry is (value:u8, source:u8).
      r.skip((outLenRaw - outLen) * 2);
    }

    this.#keyboard.loadState(r);
    this.#mouse.loadState(r);

    // Restore derived status bits. Snapshot restore should not emit spurious IRQ pulses for any
    // already-buffered output byte; pending edge-triggered interrupts must be captured/restored
    // by the interrupt controller (PIC/APIC) model instead.
    this.#irqLastHead = this.#outQueue[0] ?? null;
    this.#syncStatusAndIrq();
  }

  #setOutputPort(value: number): void {
    const next = value & 0xff;
    const prev = this.#outputPort;
    this.#outputPort = next;

    const prevA20 = (prev & OUTPUT_PORT_A20) !== 0;
    const nextA20 = (next & OUTPUT_PORT_A20) !== 0;
    if (prevA20 !== nextA20) {
      this.#sysCtrl?.setA20(nextA20);
    }

    // Bit 0 is active-low: transitioning from 1 -> 0 asserts reset.
    const prevResetDeasserted = (prev & OUTPUT_PORT_RESET) !== 0;
    const nextResetDeasserted = (next & OUTPUT_PORT_RESET) !== 0;
    if (prevResetDeasserted && !nextResetDeasserted) {
      this.#sysCtrl?.requestReset();
    }
  }
}

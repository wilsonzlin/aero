import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciAddress, PciBar, PciCapability, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioInputPciDeviceLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  poll(): void;
  driver_ok(): boolean;
  irq_asserted(): boolean;
  inject_key(linux_key: number, pressed: boolean): void;
  inject_rel(dx: number, dy: number): void;
  inject_button(btn: number, pressed: boolean): void;
  inject_wheel(delta: number): void;
  free(): void;
};

export type VirtioInputKind = "keyboard" | "mouse";

const VIRTIO_VENDOR_ID = 0x1af4;
const VIRTIO_INPUT_DEVICE_ID = 0x1052;
const VIRTIO_INPUT_REVISION_ID = 0x01;
const VIRTIO_INPUT_CLASS_CODE = 0x09_80_00;

const VIRTIO_SUBSYSTEM_VENDOR_ID = 0x1af4;
const VIRTIO_INPUT_SUBSYSTEM_KEYBOARD = 0x0010;
const VIRTIO_INPUT_SUBSYSTEM_MOUSE = 0x0011;

const VIRTIO_INPUT_MMIO_BAR_SIZE = 0x4000;

// IRQ5 is unused by the other built-in devices (i8042=IRQ1/12, UART=IRQ4, UHCI=IRQ11, E1000=IRQ10).
const VIRTIO_INPUT_IRQ_LINE = 0x05;
// Canonical multifunction virtio-input device location (keyboard=fn0, mouse=fn1).
const VIRTIO_INPUT_PCI_DEVICE = 10;

// Virtio input event codes we need on the host side.
const BTN_LEFT = 0x110;
const BTN_RIGHT = 0x111;
const BTN_MIDDLE = 0x112;

// Linux input key codes used by the browser runtime.
//
// This includes the minimum set required by the Windows 7 virtio-input contract, plus a few
// common keys that are advertised by Aero's virtio-input device model (e.g. GUI keys and locks).
const KEY_ESC = 1;
const KEY_1 = 2;
const KEY_2 = 3;
const KEY_3 = 4;
const KEY_4 = 5;
const KEY_5 = 6;
const KEY_6 = 7;
const KEY_7 = 8;
const KEY_8 = 9;
const KEY_9 = 10;
const KEY_0 = 11;
const KEY_MINUS = 12;
const KEY_EQUAL = 13;
const KEY_BACKSPACE = 14;
const KEY_TAB = 15;
const KEY_Q = 16;
const KEY_W = 17;
const KEY_E = 18;
const KEY_R = 19;
const KEY_T = 20;
const KEY_Y = 21;
const KEY_U = 22;
const KEY_I = 23;
const KEY_O = 24;
const KEY_P = 25;
const KEY_LEFTBRACE = 26;
const KEY_RIGHTBRACE = 27;
const KEY_ENTER = 28;
const KEY_LEFTCTRL = 29;
const KEY_A = 30;
const KEY_S = 31;
const KEY_D = 32;
const KEY_F = 33;
const KEY_G = 34;
const KEY_H = 35;
const KEY_J = 36;
const KEY_K = 37;
const KEY_L = 38;
const KEY_SEMICOLON = 39;
const KEY_APOSTROPHE = 40;
const KEY_GRAVE = 41;
const KEY_LEFTSHIFT = 42;
const KEY_BACKSLASH = 43;
const KEY_Z = 44;
const KEY_X = 45;
const KEY_C = 46;
const KEY_V = 47;
const KEY_B = 48;
const KEY_N = 49;
const KEY_M = 50;
const KEY_COMMA = 51;
const KEY_DOT = 52;
const KEY_SLASH = 53;
const KEY_RIGHTSHIFT = 54;
const KEY_LEFTALT = 56;
const KEY_SPACE = 57;
const KEY_CAPSLOCK = 58;
const KEY_F1 = 59;
const KEY_F2 = 60;
const KEY_F3 = 61;
const KEY_F4 = 62;
const KEY_F5 = 63;
const KEY_F6 = 64;
const KEY_F7 = 65;
const KEY_F8 = 66;
const KEY_F9 = 67;
const KEY_F10 = 68;
const KEY_NUMLOCK = 69;
const KEY_SCROLLLOCK = 70;
const KEY_F11 = 87;
const KEY_F12 = 88;
const KEY_RIGHTCTRL = 97;
const KEY_RIGHTALT = 100;
const KEY_HOME = 102;
const KEY_UP = 103;
const KEY_PAGEUP = 104;
const KEY_LEFT = 105;
const KEY_RIGHT = 106;
const KEY_END = 107;
const KEY_DOWN = 108;
const KEY_PAGEDOWN = 109;
const KEY_INSERT = 110;
const KEY_DELETE = 111;
const KEY_LEFTMETA = 125;
const KEY_RIGHTMETA = 126;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

function writeU32LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
  buf[off + 2] = (value >>> 16) & 0xff;
  buf[off + 3] = (value >>> 24) & 0xff;
}

function virtioVendorCap(opts: {
  cfgType: number;
  bar: number;
  offset: number;
  length: number;
  notifyOffMultiplier?: number;
}): PciCapability {
  const capLen = opts.notifyOffMultiplier !== undefined ? 20 : 16;
  const bytes = new Uint8Array(capLen);
  // Standard PCI capability header.
  bytes[0] = 0x09; // Vendor-specific
  bytes[1] = 0x00; // next pointer (patched by PCI bus)
  bytes[2] = capLen & 0xff;

  // virtio_pci_cap fields.
  bytes[3] = opts.cfgType & 0xff;
  bytes[4] = opts.bar & 0xff;
  bytes[5] = 0x00; // id (unused)
  bytes[6] = 0x00;
  bytes[7] = 0x00;
  writeU32LE(bytes, 8, opts.offset >>> 0);
  writeU32LE(bytes, 12, opts.length >>> 0);
  if (opts.notifyOffMultiplier !== undefined) {
    writeU32LE(bytes, 16, opts.notifyOffMultiplier >>> 0);
  }
  return { bytes };
}

/**
 * Map a USB HID keyboard Usage ID (Usage Page 0x07) to a Linux input `KEY_*` code.
 *
 * Returns `null` for unsupported usages.
 */
export function hidUsageToLinuxKeyCode(usage: number): number | null {
  switch (usage & 0xff) {
    // Letters.
    case 0x04:
      return KEY_A;
    case 0x05:
      return KEY_B;
    case 0x06:
      return KEY_C;
    case 0x07:
      return KEY_D;
    case 0x08:
      return KEY_E;
    case 0x09:
      return KEY_F;
    case 0x0a:
      return KEY_G;
    case 0x0b:
      return KEY_H;
    case 0x0c:
      return KEY_I;
    case 0x0d:
      return KEY_J;
    case 0x0e:
      return KEY_K;
    case 0x0f:
      return KEY_L;
    case 0x10:
      return KEY_M;
    case 0x11:
      return KEY_N;
    case 0x12:
      return KEY_O;
    case 0x13:
      return KEY_P;
    case 0x14:
      return KEY_Q;
    case 0x15:
      return KEY_R;
    case 0x16:
      return KEY_S;
    case 0x17:
      return KEY_T;
    case 0x18:
      return KEY_U;
    case 0x19:
      return KEY_V;
    case 0x1a:
      return KEY_W;
    case 0x1b:
      return KEY_X;
    case 0x1c:
      return KEY_Y;
    case 0x1d:
      return KEY_Z;

    // Digits.
    case 0x1e:
      return KEY_1;
    case 0x1f:
      return KEY_2;
    case 0x20:
      return KEY_3;
    case 0x21:
      return KEY_4;
    case 0x22:
      return KEY_5;
    case 0x23:
      return KEY_6;
    case 0x24:
      return KEY_7;
    case 0x25:
      return KEY_8;
    case 0x26:
      return KEY_9;
    case 0x27:
      return KEY_0;

    // Basic.
    case 0x28:
      return KEY_ENTER;
    case 0x29:
      return KEY_ESC;
    case 0x2a:
      return KEY_BACKSPACE;
    case 0x2b:
      return KEY_TAB;
    case 0x2c:
      return KEY_SPACE;
    case 0x2d:
      return KEY_MINUS;
    case 0x2e:
      return KEY_EQUAL;
    case 0x2f:
      return KEY_LEFTBRACE;
    case 0x30:
      return KEY_RIGHTBRACE;
    case 0x31:
      return KEY_BACKSLASH;
    case 0x33:
      return KEY_SEMICOLON;
    case 0x34:
      return KEY_APOSTROPHE;
    case 0x35:
      return KEY_GRAVE;
    case 0x36:
      return KEY_COMMA;
    case 0x37:
      return KEY_DOT;
    case 0x38:
      return KEY_SLASH;

    // Modifiers.
    case 0xe0:
      return KEY_LEFTCTRL;
    case 0xe1:
      return KEY_LEFTSHIFT;
    case 0xe2:
      return KEY_LEFTALT;
    case 0xe3:
      return KEY_LEFTMETA;
    case 0xe4:
      return KEY_RIGHTCTRL;
    case 0xe5:
      return KEY_RIGHTSHIFT;
    case 0xe6:
      return KEY_RIGHTALT;
    case 0xe7:
      return KEY_RIGHTMETA;

    case 0x39:
      return KEY_CAPSLOCK;

    // Function keys.
    case 0x3a:
      return KEY_F1;
    case 0x3b:
      return KEY_F2;
    case 0x3c:
      return KEY_F3;
    case 0x3d:
      return KEY_F4;
    case 0x3e:
      return KEY_F5;
    case 0x3f:
      return KEY_F6;
    case 0x40:
      return KEY_F7;
    case 0x41:
      return KEY_F8;
    case 0x42:
      return KEY_F9;
    case 0x43:
      return KEY_F10;
    case 0x44:
      return KEY_F11;
    case 0x45:
      return KEY_F12;

    // Locks.
    case 0x47:
      return KEY_SCROLLLOCK;

    // Navigation.
    case 0x49:
      return KEY_INSERT;
    case 0x4a:
      return KEY_HOME;
    case 0x4b:
      return KEY_PAGEUP;
    case 0x4c:
      return KEY_DELETE;
    case 0x4d:
      return KEY_END;
    case 0x4e:
      return KEY_PAGEDOWN;
    case 0x4f:
      return KEY_RIGHT;
    case 0x50:
      return KEY_LEFT;
    case 0x51:
      return KEY_DOWN;
    case 0x52:
      return KEY_UP;

    // Keypad.
    case 0x53:
      return KEY_NUMLOCK;

    default:
      return null;
  }
}

export class VirtioInputPciFunction implements PciDevice, TickableDevice {
  readonly name: string;
  readonly vendorId = VIRTIO_VENDOR_ID;
  readonly deviceId = VIRTIO_INPUT_DEVICE_ID;
  readonly classCode = VIRTIO_INPUT_CLASS_CODE;
  readonly revisionId = VIRTIO_INPUT_REVISION_ID;

  readonly subsystemVendorId = VIRTIO_SUBSYSTEM_VENDOR_ID;
  readonly subsystemId: number;
  readonly headerType: number;
  readonly irqLine = VIRTIO_INPUT_IRQ_LINE;
  readonly interruptPin = 1 as const;
  readonly bdf: PciAddress;

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio64", size: VIRTIO_INPUT_MMIO_BAR_SIZE }, null, null, null, null, null];
  readonly capabilities: ReadonlyArray<PciCapability> = [
    // Virtio modern vendor-specific capabilities (contract v1 fixed BAR0 layout).
    // The PCI bus will install these starting at 0x40 with 4-byte aligned pointers.
    virtioVendorCap({ cfgType: 1, bar: 0, offset: 0x0000, length: 0x0100 }), // COMMON_CFG
    virtioVendorCap({ cfgType: 2, bar: 0, offset: 0x1000, length: 0x0100, notifyOffMultiplier: 4 }), // NOTIFY_CFG
    virtioVendorCap({ cfgType: 3, bar: 0, offset: 0x2000, length: 0x0020 }), // ISR_CFG
    virtioVendorCap({ cfgType: 4, bar: 0, offset: 0x3000, length: 0x0100 }), // DEVICE_CFG
  ];

  readonly #dev: VirtioInputPciDeviceLike;
  readonly #irqSink: IrqSink;
  readonly #kind: VirtioInputKind;

  #irqLevel = false;
  #destroyed = false;
  #driverOkLogged = false;
  #mouseButtons = 0;

  constructor(opts: { kind: VirtioInputKind; device: VirtioInputPciDeviceLike; irqSink: IrqSink }) {
    this.#kind = opts.kind;
    this.#dev = opts.device;
    this.#irqSink = opts.irqSink;
    this.name = `virtio_input_${opts.kind}`;
    this.subsystemId = opts.kind === "keyboard" ? VIRTIO_INPUT_SUBSYSTEM_KEYBOARD : VIRTIO_INPUT_SUBSYSTEM_MOUSE;
    this.headerType = opts.kind === "keyboard" ? 0x80 : 0x00;
    this.bdf = { bus: 0, device: VIRTIO_INPUT_PCI_DEVICE, function: opts.kind === "keyboard" ? 0 : 1 };
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_INPUT_MMIO_BAR_SIZE) return defaultReadValue(size);

    let value = 0;
    try {
      value = this.#dev.mmio_read(off >>> 0, size >>> 0) >>> 0;
    } catch {
      value = 0;
    }

    // Reads from the ISR register are read-to-ack and may deassert the IRQ.
    if (off >= 0x2000 && off < 0x2000 + 0x20) {
      this.#syncIrq();
    }

    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_INPUT_MMIO_BAR_SIZE) return;

    try {
      this.#dev.mmio_write(off >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest MMIO
    }
    this.#syncIrq();
  }

  tick(_nowMs: number): void {
    if (this.#destroyed) return;
    try {
      // Drive notified virtqueues (especially `statusq` LED/output events) so the guest
      // never wedges waiting for completions when no input events are flowing.
      this.#dev.poll();
    } catch {
      // ignore device errors during tick
    }
    this.#syncIrq();
  }

  driverOk(): boolean {
    let ok = false;
    try {
      ok = Boolean(this.#dev.driver_ok());
    } catch {
      ok = false;
    }
    if (ok && !this.#driverOkLogged) {
      this.#driverOkLogged = true;
      console.info(`[virtio-input] ${this.#kind} driver_ok`);
    }
    return ok;
  }

  injectKey(code: number, pressed: boolean): void {
    if (this.#destroyed) return;
    try {
      this.#dev.inject_key(code >>> 0, Boolean(pressed));
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  injectRelMove(dx: number, dy: number): void {
    if (this.#destroyed) return;
    try {
      this.#dev.inject_rel(dx | 0, dy | 0);
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  injectWheel(delta: number): void {
    if (this.#destroyed) return;
    try {
      this.#dev.inject_wheel(delta | 0);
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  /**
   * Update current mouse button bitmask and emit EV_KEY transitions for changes.
   *
   * Input batches carry the *current* bitmask (bit0=left, bit1=right, bit2=middle).
   */
  injectMouseButtons(buttonMask: number): void {
    if (this.#destroyed) return;

    const next = buttonMask & 0x07;
    const prev = this.#mouseButtons & 0x07;
    const delta = prev ^ next;
    if (delta === 0) return;

    const changes: Array<{ code: number; pressed: boolean }> = [];
    if (delta & 0x01) changes.push({ code: BTN_LEFT, pressed: (next & 0x01) !== 0 });
    if (delta & 0x02) changes.push({ code: BTN_RIGHT, pressed: (next & 0x02) !== 0 });
    if (delta & 0x04) changes.push({ code: BTN_MIDDLE, pressed: (next & 0x04) !== 0 });

      for (const ch of changes) {
        try {
          this.#dev.inject_button(ch.code >>> 0, ch.pressed);
        } catch {
          // ignore
        }
      }

    this.#mouseButtons = next;
    this.#syncIrq();
  }

  destroy(): void {
    if (this.#destroyed) return;
    this.#destroyed = true;

    if (this.#irqLevel) {
      this.#irqSink.lowerIrq(this.irqLine);
      this.#irqLevel = false;
    }

    try {
      this.#dev.free();
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    let asserted = false;
    try {
      asserted = Boolean(this.#dev.irq_asserted());
    } catch {
      asserted = false;
    }
    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}

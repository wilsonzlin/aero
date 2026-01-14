import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciAddress, PciBar, PciCapability, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioInputPciDeviceLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  /**
   * Legacy virtio-pci (0.9) I/O port register block accessors (BAR2).
   *
   * Newer WASM builds expose these as `legacy_io_read`/`legacy_io_write`. Older builds used
   * `io_read`/`io_write` and those names are retained for back-compat.
   */
  legacy_io_read?(offset: number, size: number): number;
  legacy_io_write?(offset: number, size: number, value: number): void;
  io_read?(offset: number, size: number): number;
  io_write?(offset: number, size: number, value: number): void;
  poll(): void;
  /**
   * Optional hook for mirroring PCI command register writes into the underlying device model.
   *
   * When present, this can be used by WASM bridges to enforce DMA gating based on Bus Master Enable.
   */
  set_pci_command?(command: number): void;
  driver_ok(): boolean;
  irq_asserted(): boolean;
  inject_key(linux_key: number, pressed: boolean): void;
  inject_rel(dx: number, dy: number): void;
  inject_button(btn: number, pressed: boolean): void;
  inject_wheel(delta: number): void;
  inject_hwheel?(delta: number): void;
  inject_wheel2?(wheel: number, hwheel: number): void;
  // Optional snapshot hooks (aero-io-snapshot deterministic bytes).
  save_state?: () => Uint8Array;
  snapshot_state?: () => Uint8Array;
  load_state?: (bytes: Uint8Array) => void;
  restore_state?: (bytes: Uint8Array) => void;
  free(): void;
};

export type VirtioInputKind = "keyboard" | "mouse";
export type VirtioInputPciMode = "modern" | "transitional" | "legacy";

const VIRTIO_VENDOR_ID = 0x1af4;
// Modern virtio-pci device ID space is 0x1040 + <virtio device type>. virtio-input type is 18 (0x12).
const VIRTIO_INPUT_MODERN_DEVICE_ID = 0x1052;
// Transitional virtio-pci device IDs are 0x1000 + (type - 1).
const VIRTIO_INPUT_TRANSITIONAL_DEVICE_ID = 0x1011;
const VIRTIO_INPUT_REVISION_ID = 0x01;
const VIRTIO_INPUT_CLASS_CODE = 0x09_80_00;

const VIRTIO_SUBSYSTEM_VENDOR_ID = 0x1af4;
const VIRTIO_INPUT_SUBSYSTEM_KEYBOARD = 0x0010;
const VIRTIO_INPUT_SUBSYSTEM_MOUSE = 0x0011;

const VIRTIO_INPUT_MMIO_BAR_SIZE = 0x4000;
// Keep in sync with `crates/aero-virtio/src/pci.rs` (`bar2_size` when legacy I/O is enabled).
const VIRTIO_LEGACY_IO_BAR2_SIZE = 0x100;

// IRQ5 is unused by the other built-in devices (i8042=IRQ1/12, UART=IRQ4, UHCI=IRQ11, E1000=IRQ10).
const VIRTIO_INPUT_IRQ_LINE = 0x05;
// Canonical multifunction virtio-input device location (keyboard=fn0, mouse=fn1).
export const VIRTIO_INPUT_PCI_DEVICE = 10;

// Virtio input event codes we need on the host side.
const BTN_LEFT = 0x110;
const BTN_RIGHT = 0x111;
const BTN_MIDDLE = 0x112;
const BTN_SIDE = 0x113;
const BTN_EXTRA = 0x114;
const BTN_FORWARD = 0x115;
const BTN_BACK = 0x116;
const BTN_TASK = 0x117;

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
const KEY_KPASTERISK = 55;
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
const KEY_KP7 = 71;
const KEY_KP8 = 72;
const KEY_KP9 = 73;
const KEY_KPMINUS = 74;
const KEY_KP4 = 75;
const KEY_KP5 = 76;
const KEY_KP6 = 77;
const KEY_KPPLUS = 78;
const KEY_KP1 = 79;
const KEY_KP2 = 80;
const KEY_KP3 = 81;
const KEY_KP0 = 82;
const KEY_KPDOT = 83;
const KEY_102ND = 86;
const KEY_F11 = 87;
const KEY_F12 = 88;
const KEY_RO = 89;
const KEY_KPENTER = 96;
const KEY_RIGHTCTRL = 97;
const KEY_KPSLASH = 98;
const KEY_SYSRQ = 99;
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
// Consumer/media keys. These are used by the Windows 7 virtio-input driver to expose a Consumer Control
// HID collection (ReportID=3) on the keyboard device.
const KEY_MUTE = 113;
const KEY_VOLUMEDOWN = 114;
const KEY_VOLUMEUP = 115;
const KEY_KPEQUAL = 117;
const KEY_PAUSE = 119;
const KEY_KPCOMMA = 121;
const KEY_YEN = 124;
const KEY_LEFTMETA = 125;
const KEY_RIGHTMETA = 126;
const KEY_MENU = 139;
const KEY_NEXTSONG = 163;
const KEY_PLAYPAUSE = 164;
const KEY_PREVIOUSSONG = 165;
const KEY_STOPCD = 166;

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
    // `IntlHash` is a layout-specific variant of the same physical key position as `Backslash`.
    // Windows keyboard layouts commonly map both to the same scan code, so treat this as an alias.
    case 0x32:
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
    case 0x46:
      return KEY_SYSRQ;

    // Locks.
    case 0x47:
      return KEY_SCROLLLOCK;

    case 0x48:
      return KEY_PAUSE;

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
    case 0x54:
      return KEY_KPSLASH;
    case 0x55:
      return KEY_KPASTERISK;
    case 0x56:
      return KEY_KPMINUS;
    case 0x57:
      return KEY_KPPLUS;
    case 0x58:
      return KEY_KPENTER;
    case 0x59:
      return KEY_KP1;
    case 0x5a:
      return KEY_KP2;
    case 0x5b:
      return KEY_KP3;
    case 0x5c:
      return KEY_KP4;
    case 0x5d:
      return KEY_KP5;
    case 0x5e:
      return KEY_KP6;
    case 0x5f:
      return KEY_KP7;
    case 0x60:
      return KEY_KP8;
    case 0x61:
      return KEY_KP9;
    case 0x62:
      return KEY_KP0;
    case 0x63:
      return KEY_KPDOT;
    case 0x64:
      return KEY_102ND;
    case 0x65:
      return KEY_MENU;
    case 0x67:
      return KEY_KPEQUAL;
    case 0x85:
      return KEY_KPCOMMA;
    case 0x87:
      return KEY_RO;
    case 0x89:
      return KEY_YEN;

    default:
      return null;
  }
}

/**
 * Map a HID Consumer Control Usage ID (Usage Page 0x0C) to a Linux input `KEY_*` code.
 *
 * This is used when routing browser media keys through virtio-input (instead of the dedicated
 * synthetic USB HID consumer-control device).
 *
 * Note: this intentionally only maps the subset of media keys that the Windows 7 virtio-input
 * driver is known to expose. Browser/application control usages (AC Back/Forward/etc.) should
 * continue to route via the synthetic USB consumer-control device so they work even when the
 * virtio-input path does not model them.
 *
 * Returns `null` for unsupported usages.
 */
export function hidConsumerUsageToLinuxKeyCode(usageId: number): number | null {
  switch (usageId & 0xffff) {
    case 0x00e2:
      return KEY_MUTE;
    case 0x00ea:
      return KEY_VOLUMEDOWN;
    case 0x00e9:
      return KEY_VOLUMEUP;
    case 0x00cd:
      return KEY_PLAYPAUSE;
    case 0x00b5:
      return KEY_NEXTSONG;
    case 0x00b6:
      return KEY_PREVIOUSSONG;
    case 0x00b7:
      return KEY_STOPCD;
    default:
      return null;
  }
}

export class VirtioInputPciFunction implements PciDevice, TickableDevice {
  readonly name: string;
  readonly vendorId = VIRTIO_VENDOR_ID;
  readonly deviceId: number;
  readonly classCode = VIRTIO_INPUT_CLASS_CODE;
  readonly revisionId = VIRTIO_INPUT_REVISION_ID;

  readonly subsystemVendorId = VIRTIO_SUBSYSTEM_VENDOR_ID;
  readonly subsystemId: number;
  readonly headerType: number;
  readonly irqLine = VIRTIO_INPUT_IRQ_LINE;
  readonly interruptPin = 1 as const;
  readonly bdf: PciAddress;

  readonly bars: ReadonlyArray<PciBar | null>;
  readonly capabilities: ReadonlyArray<PciCapability>;

  readonly #dev: VirtioInputPciDeviceLike;
  readonly #mmioReadFn: (offset: number, size: number) => number;
  readonly #mmioWriteFn: (offset: number, size: number, value: number) => void;
  readonly #pollFn: () => void;
  readonly #driverOkFn: () => boolean;
  readonly #irqAssertedFn: () => boolean;
  readonly #injectKeyFn: (linuxKey: number, pressed: boolean) => void;
  readonly #injectRelFn: (dx: number, dy: number) => void;
  readonly #injectButtonFn: (btn: number, pressed: boolean) => void;
  readonly #injectWheelFn: (delta: number) => void;
  readonly #injectHwheelFn: ((delta: number) => void) | null;
  readonly #injectWheel2Fn: ((wheel: number, hwheel: number) => void) | null;
  readonly #setPciCommandFn: ((command: number) => void) | null;
  readonly #freeFn: () => void;
  readonly #irqSink: IrqSink;
  readonly #kind: VirtioInputKind;
  readonly #mode: VirtioInputPciMode;

  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;
  #driverOkLogged = false;
  #mouseButtons = 0;

  constructor(opts: { kind: VirtioInputKind; device: VirtioInputPciDeviceLike; irqSink: IrqSink; mode?: VirtioInputPciMode }) {
    this.#kind = opts.kind;
    this.#dev = opts.device;
    this.#irqSink = opts.irqSink;
    this.#mode = opts.mode ?? "modern";

    // Backwards compatibility: accept both snake_case and camelCase exports and always call
    // extracted methods via `.call(dev, ...)` to avoid wasm-bindgen `this` binding pitfalls.
    const devAny = opts.device as unknown as Record<string, unknown>;
    const mmioRead = devAny.mmio_read ?? devAny.mmioRead;
    const mmioWrite = devAny.mmio_write ?? devAny.mmioWrite;
    const poll = devAny.poll;
    const driverOk = devAny.driver_ok ?? devAny.driverOk;
    const irqAsserted = devAny.irq_asserted ?? devAny.irqAsserted;
    const injectKey = devAny.inject_key ?? devAny.injectKey;
    const injectRel = devAny.inject_rel ?? devAny.injectRel;
    const injectButton = devAny.inject_button ?? devAny.injectButton;
    const injectWheel = devAny.inject_wheel ?? devAny.injectWheel;
    const free = devAny.free;

    if (typeof mmioRead !== "function" || typeof mmioWrite !== "function") {
      throw new Error("virtio-input device missing mmio_read/mmioRead or mmio_write/mmioWrite exports.");
    }
    if (typeof poll !== "function") {
      throw new Error("virtio-input device missing poll() export.");
    }
    if (typeof driverOk !== "function") {
      throw new Error("virtio-input device missing driver_ok/driverOk export.");
    }
    if (typeof irqAsserted !== "function") {
      throw new Error("virtio-input device missing irq_asserted/irqAsserted export.");
    }
    if (typeof injectKey !== "function" || typeof injectRel !== "function") {
      throw new Error("virtio-input device missing inject_key/injectKey or inject_rel/injectRel exports.");
    }
    if (typeof injectButton !== "function" || typeof injectWheel !== "function") {
      throw new Error("virtio-input device missing inject_button/injectButton or inject_wheel/injectWheel exports.");
    }
    if (typeof free !== "function") {
      throw new Error("virtio-input device missing free() export.");
    }

    this.#mmioReadFn = mmioRead as (offset: number, size: number) => number;
    this.#mmioWriteFn = mmioWrite as (offset: number, size: number, value: number) => void;
    this.#pollFn = poll as () => void;
    this.#driverOkFn = driverOk as () => boolean;
    this.#irqAssertedFn = irqAsserted as () => boolean;
    this.#injectKeyFn = injectKey as (linuxKey: number, pressed: boolean) => void;
    this.#injectRelFn = injectRel as (dx: number, dy: number) => void;
    this.#injectButtonFn = injectButton as (btn: number, pressed: boolean) => void;
    this.#injectWheelFn = injectWheel as (delta: number) => void;
    this.#freeFn = free as () => void;

    const injectHwheel = devAny.inject_hwheel ?? devAny.injectHwheel ?? devAny.injectHWheel;
    this.#injectHwheelFn = typeof injectHwheel === "function" ? (injectHwheel as (delta: number) => void) : null;
    const injectWheel2 = devAny.inject_wheel2 ?? devAny.injectWheel2;
    this.#injectWheel2Fn =
      typeof injectWheel2 === "function" ? (injectWheel2 as (wheel: number, hwheel: number) => void) : null;
    const setCmd = devAny.set_pci_command ?? devAny.setPciCommand;
    this.#setPciCommandFn = typeof setCmd === "function" ? (setCmd as (command: number) => void) : null;

    this.name = `virtio_input_${opts.kind}`;
    this.subsystemId = opts.kind === "keyboard" ? VIRTIO_INPUT_SUBSYSTEM_KEYBOARD : VIRTIO_INPUT_SUBSYSTEM_MOUSE;
    this.headerType = opts.kind === "keyboard" ? 0x80 : 0x00;
    this.bdf = { bus: 0, device: VIRTIO_INPUT_PCI_DEVICE, function: opts.kind === "keyboard" ? 0 : 1 };

    const caps: ReadonlyArray<PciCapability> = [
      // Virtio modern vendor-specific capabilities (contract v1 fixed BAR0 layout).
      // The PCI bus will install these starting at 0x40 with 4-byte aligned pointers.
      virtioVendorCap({ cfgType: 1, bar: 0, offset: 0x0000, length: 0x0100 }), // COMMON_CFG
      virtioVendorCap({ cfgType: 2, bar: 0, offset: 0x1000, length: 0x0100, notifyOffMultiplier: 4 }), // NOTIFY_CFG
      virtioVendorCap({ cfgType: 3, bar: 0, offset: 0x2000, length: 0x0020 }), // ISR_CFG
      virtioVendorCap({ cfgType: 4, bar: 0, offset: 0x3000, length: 0x0100 }), // DEVICE_CFG
    ];

    // Legacy-only mode intentionally disables modern virtio-pci capabilities so guests take the
    // virtio 0.9 I/O-port transport path.
    this.capabilities = this.#mode === "legacy" ? [] : caps;

    this.deviceId = this.#mode === "modern" ? VIRTIO_INPUT_MODERN_DEVICE_ID : VIRTIO_INPUT_TRANSITIONAL_DEVICE_ID;
    this.bars =
      this.#mode === "modern"
        ? [{ kind: "mmio64", size: VIRTIO_INPUT_MMIO_BAR_SIZE }, null, null, null, null, null]
        : [
            { kind: "mmio64", size: VIRTIO_INPUT_MMIO_BAR_SIZE },
            null,
            { kind: "io", size: VIRTIO_LEGACY_IO_BAR2_SIZE },
            null,
            null,
            null,
          ];
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_INPUT_MMIO_BAR_SIZE) return defaultReadValue(size);

    let value = 0;
    try {
      value = this.#mmioReadFn.call(this.#dev, off >>> 0, size >>> 0) >>> 0;
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
      this.#mmioWriteFn.call(this.#dev, off >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest MMIO
    }
    this.#syncIrq();
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 2) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);
    if (this.#mode === "modern") return defaultReadValue(size);

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return defaultReadValue(size);

    const dev = this.#dev as unknown as Record<string, unknown>;
    const fn =
      (typeof dev.legacy_io_read === "function"
        ? (dev.legacy_io_read as (offset: number, size: number) => number)
        : typeof dev.legacyIoRead === "function"
          ? (dev.legacyIoRead as (offset: number, size: number) => number)
          : typeof dev.io_read === "function"
            ? (dev.io_read as (offset: number, size: number) => number)
            : typeof dev.ioRead === "function"
              ? (dev.ioRead as (offset: number, size: number) => number)
              : undefined) ?? undefined;
    if (typeof fn !== "function") return defaultReadValue(size);

    let value: number;
    try {
      value = fn.call(this.#dev, off, size) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }
    this.#syncIrq();
    return maskToSize(value, size);
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 2) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    if (this.#mode === "modern") return;

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return;

    const dev = this.#dev as unknown as Record<string, unknown>;
    const fn =
      (typeof dev.legacy_io_write === "function"
        ? (dev.legacy_io_write as (offset: number, size: number, value: number) => void)
        : typeof dev.legacyIoWrite === "function"
          ? (dev.legacyIoWrite as (offset: number, size: number, value: number) => void)
          : typeof dev.io_write === "function"
            ? (dev.io_write as (offset: number, size: number, value: number) => void)
            : typeof dev.ioWrite === "function"
              ? (dev.ioWrite as (offset: number, size: number, value: number) => void)
              : undefined) ?? undefined;
    if (typeof fn === "function") {
      try {
        fn.call(this.#dev, off, size, maskToSize(value >>> 0, size));
      } catch {
        // ignore device errors during guest IO
      }
    }
    this.#syncIrq();
  }

  onPciCommandWrite(command: number): void {
    if (this.#destroyed) return;
    const cmd = command & 0xffff;
    this.#pciCommand = cmd;

    // Mirror into the underlying device model so it can enforce DMA gating based on Bus Master Enable.
    const setCmd = this.#setPciCommandFn;
    if (typeof setCmd === "function") {
      try {
        setCmd.call(this.#dev, cmd >>> 0);
      } catch {
        // ignore device errors during PCI config writes
      }
    }

    // Interrupt Disable bit can immediately drop INTx level.
    this.#syncIrq();
  }

  tick(_nowMs: number): void {
    if (this.#destroyed) return;

    // PCI Bus Master Enable (command bit 2) gates whether the device is allowed to DMA into guest
    // memory (virtqueue descriptor reads / used-ring writes / event buffer fills).
    //
    // Mirror/gating note:
    // - Newer WASM builds can also enforce this via `set_pci_command`, but keep a wrapper-side gate
    //   so older builds remain correct and we avoid invoking poll unnecessarily.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;

    if (busMasterEnabled) {
      try {
        // Drive notified virtqueues (especially `statusq` LED/output events) so the guest
        // never wedges waiting for completions when no input events are flowing.
        this.#pollFn.call(this.#dev);
      } catch {
        // ignore device errors during tick
      }
    }
    this.#syncIrq();
  }

  driverOk(): boolean {
    let ok = false;
    try {
      ok = Boolean(this.#driverOkFn.call(this.#dev));
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
    // Linux input defines code 0 as KEY_RESERVED / BTN_RESERVED. Treat it as a no-op so host-side
    // injection cannot generate spurious events for an invalid code.
    const key = code >>> 0;
    if (key === 0) return;
    try {
      this.#injectKeyFn.call(this.#dev, key, Boolean(pressed));
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  injectRelMove(dx: number, dy: number): void {
    if (this.#destroyed) return;
    const x = dx | 0;
    const y = dy | 0;
    if (x === 0 && y === 0) return;
    try {
      this.#injectRelFn.call(this.#dev, x, y);
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  injectWheel(delta: number): void {
    if (this.#destroyed) return;
    try {
      this.#injectWheelFn.call(this.#dev, delta | 0);
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  injectHWheel(delta: number): void {
    if (this.#destroyed) return;
    const fn = this.#injectHwheelFn;
    if (!fn) return;
    try {
      fn.call(this.#dev, delta | 0);
    } catch {
      // ignore
    }
    this.#syncIrq();
  }

  injectWheel2(wheel: number, hwheel: number): void {
    if (this.#destroyed) return;

    const fn = this.#injectWheel2Fn;
    if (fn) {
      try {
        fn.call(this.#dev, wheel | 0, hwheel | 0);
      } catch {
        // ignore
      }
      this.#syncIrq();
      return;
    }

    // Backwards compatibility: older WASM builds can only inject each axis separately (which
    // produces two SYN frames if both axes are non-zero).
    if (wheel !== 0) this.injectWheel(wheel);
    if (hwheel !== 0) this.injectHWheel(hwheel);
  }

  /**
   * Update current mouse button bitmask and emit EV_KEY transitions for changes.
   *
   * Input batches carry the *current* bitmask (bit0..bit7 => buttons 1..8).
   */
  injectMouseButtons(buttonMask: number): void {
    if (this.#destroyed) return;

    const next = buttonMask & 0xff;
    const prev = this.#mouseButtons & 0xff;
    const delta = prev ^ next;
    if (delta === 0) return;

    const changes: Array<{ code: number; pressed: boolean }> = [];
    if (delta & 0x01) changes.push({ code: BTN_LEFT, pressed: (next & 0x01) !== 0 });
    if (delta & 0x02) changes.push({ code: BTN_RIGHT, pressed: (next & 0x02) !== 0 });
    if (delta & 0x04) changes.push({ code: BTN_MIDDLE, pressed: (next & 0x04) !== 0 });
    if (delta & 0x08) changes.push({ code: BTN_SIDE, pressed: (next & 0x08) !== 0 });
    if (delta & 0x10) changes.push({ code: BTN_EXTRA, pressed: (next & 0x10) !== 0 });
    if (delta & 0x20) changes.push({ code: BTN_FORWARD, pressed: (next & 0x20) !== 0 });
    if (delta & 0x40) changes.push({ code: BTN_BACK, pressed: (next & 0x40) !== 0 });
    if (delta & 0x80) changes.push({ code: BTN_TASK, pressed: (next & 0x80) !== 0 });

    for (const ch of changes) {
      try {
        this.#injectButtonFn.call(this.#dev, ch.code >>> 0, ch.pressed);
      } catch {
        // ignore
      }
    }

    this.#mouseButtons = next;
    this.#syncIrq();
  }

  canSaveState(): boolean {
    const dev = this.#dev as unknown as Record<string, unknown>;
    return (
      typeof dev["save_state"] === "function" ||
      typeof dev["snapshot_state"] === "function" ||
      typeof dev["saveState"] === "function" ||
      typeof dev["snapshotState"] === "function"
    );
  }

  canLoadState(): boolean {
    const dev = this.#dev as unknown as Record<string, unknown>;
    return (
      typeof dev["load_state"] === "function" ||
      typeof dev["restore_state"] === "function" ||
      typeof dev["loadState"] === "function" ||
      typeof dev["restoreState"] === "function"
    );
  }

  saveState(): Uint8Array | null {
    if (this.#destroyed) return null;
    const devAny = this.#dev as unknown as Record<string, unknown>;
    const save = devAny.save_state ?? devAny.snapshot_state ?? devAny.saveState ?? devAny.snapshotState;
    if (typeof save !== "function") return null;
    try {
      const bytes = (save as () => unknown).call(this.#dev) as unknown;
      if (bytes instanceof Uint8Array) return bytes;
    } catch {
      // ignore
    }
    return null;
  }

  loadState(bytes: Uint8Array): boolean {
    if (this.#destroyed) return false;
    const devAny = this.#dev as unknown as Record<string, unknown>;
    const load = devAny.load_state ?? devAny.restore_state ?? devAny.loadState ?? devAny.restoreState;
    if (typeof load !== "function") return false;
    try {
      (load as (bytes: Uint8Array) => unknown).call(this.#dev, bytes);
      this.#syncIrq();
      return true;
    } catch {
      return false;
    }
  }

  // `io_worker_vm_snapshot.ts` expects snake_case `save_state/load_state` hooks.
  save_state(): Uint8Array {
    const bytes = this.saveState();
    if (!bytes) throw new Error("virtio-input snapshot exports unavailable");
    return bytes;
  }

  load_state(bytes: Uint8Array): void {
    if (!this.loadState(bytes)) {
      throw new Error("virtio-input snapshot restore exports unavailable");
    }
  }

  destroy(): void {
    if (this.#destroyed) return;
    this.#destroyed = true;

    if (this.#irqLevel) {
      this.#irqSink.lowerIrq(this.irqLine);
      this.#irqLevel = false;
    }

    try {
      this.#freeFn.call(this.#dev);
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    let asserted = false;
    try {
      asserted = Boolean(this.#irqAssertedFn.call(this.#dev));
    } catch {
      asserted = false;
    }

    // Respect PCI command register Interrupt Disable bit (bit 10). When set, the device must not
    // assert INTx.
    if ((this.#pciCommand & (1 << 10)) !== 0) {
      asserted = false;
    }
    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}

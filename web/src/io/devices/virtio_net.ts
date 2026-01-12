import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioNetPciBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  /**
   * Optional legacy virtio-pci I/O port register block accessors.
   *
   * Present when the WASM bridge supports PCI transitional devices (legacy + modern).
   */
  io_read?(offset: number, size: number): number;
  io_write?(offset: number, size: number, value: number): void;
  /**
   * Optional poll hook (some WASM builds expose `poll()` rather than `tick()`).
   */
  poll?: () => void;
  /**
   * Optional per-tick hook (canonical in current WASM builds).
   */
  tick?: (nowMs?: number) => void;
  irq_level?(): boolean;
  irq_asserted?(): boolean;
  free(): void;
};

export type VirtioNetPciMode = "modern" | "transitional";

const VIRTIO_VENDOR_ID = 0x1af4;
// Modern virtio-pci device ID space is 0x1040 + <virtio device type>.
const VIRTIO_NET_MODERN_DEVICE_ID = 0x1041;
// Transitional virtio-pci device IDs are 0x1000 + (type - 1). virtio-net type is 1.
const VIRTIO_NET_TRANSITIONAL_DEVICE_ID = 0x1000;
const VIRTIO_NET_SUBSYSTEM_DEVICE_ID = 0x0001;
const VIRTIO_NET_CLASS_CODE = 0x02_00_00;
const VIRTIO_CONTRACT_REVISION_ID = 0x01;

// BAR0 size in the Aero Windows 7 virtio contract v1 (see docs/windows7-virtio-driver-contract.md).
const VIRTIO_MMIO_BAR0_SIZE = 0x4000;
// Keep in sync with `crates/aero-virtio/src/pci.rs` (`bar2_size` when legacy I/O is enabled).
const VIRTIO_LEGACY_IO_BAR2_SIZE = 0x100;

// Fixed virtio-pci capability layout within BAR0 (contract v1).
const VIRTIO_MMIO_COMMON_OFFSET = 0x0000;
const VIRTIO_MMIO_COMMON_LEN = 0x0100;
const VIRTIO_MMIO_NOTIFY_OFFSET = 0x1000;
const VIRTIO_MMIO_NOTIFY_LEN = 0x0100;
const VIRTIO_MMIO_ISR_OFFSET = 0x2000;
const VIRTIO_MMIO_ISR_LEN = 0x0020;
const VIRTIO_MMIO_DEVICE_OFFSET = 0x3000;
const VIRTIO_MMIO_DEVICE_LEN = 0x0100;
const VIRTIO_MMIO_NOTIFY_OFF_MULTIPLIER = 4;

// IRQ line choice:
// - 0x0b is used by the UHCI controller model.
// - PCI INTx lines are level-triggered and may be shared; the IO worker uses refcounts.
// Use IRQ 10 (0x0a) as an additional, stable line.
const VIRTIO_NET_IRQ_LINE = 0x0a;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

function isInRange(off: number, size: number, base: number, len: number): boolean {
  return off >= base && off + size <= base + len;
}

function writeU16LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
}

function writeU32LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
  buf[off + 2] = (value >>> 16) & 0xff;
  buf[off + 3] = (value >>> 24) & 0xff;
}

function writeVirtioPciCap(
  config: Uint8Array,
  off: number,
  opts: { next: number; capLen: number; cfgType: number; bar: number; offset: number; length: number; notifyOffMultiplier?: number },
): void {
  config[off + 0x00] = 0x09; // PCI capability ID: vendor-specific.
  config[off + 0x01] = opts.next & 0xff; // cap_next
  config[off + 0x02] = opts.capLen & 0xff;
  config[off + 0x03] = opts.cfgType & 0xff;
  config[off + 0x04] = opts.bar & 0xff;
  config[off + 0x05] = 0x00; // id
  config[off + 0x06] = 0x00; // padding[0]
  config[off + 0x07] = 0x00; // padding[1]
  writeU32LE(config, off + 0x08, opts.offset >>> 0);
  writeU32LE(config, off + 0x0c, opts.length >>> 0);
  if (opts.notifyOffMultiplier !== undefined) {
    writeU32LE(config, off + 0x10, opts.notifyOffMultiplier >>> 0);
  }
}

/**
 * Virtio-net PCI function (virtio-pci modern or transitional transport) backed by the WASM `VirtioNetPciBridge`.
 *
 * Exposes:
 * - BAR0: 64-bit MMIO BAR, size 0x4000 (Aero Windows 7 virtio contract v1 modern layout).
 * - BAR2 (transitional mode only): legacy I/O port register block, size 0x100.
 */
export class VirtioNetPciDevice implements PciDevice, TickableDevice {
  readonly name = "virtio_net";
  readonly vendorId = VIRTIO_VENDOR_ID;
  readonly deviceId: number;
  readonly subsystemVendorId = VIRTIO_VENDOR_ID;
  readonly subsystemId = VIRTIO_NET_SUBSYSTEM_DEVICE_ID;
  readonly classCode = VIRTIO_NET_CLASS_CODE;
  readonly revisionId = VIRTIO_CONTRACT_REVISION_ID;
  readonly irqLine = VIRTIO_NET_IRQ_LINE;
  readonly bdf = { bus: 0, device: 8, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null>;

  readonly #bridge: VirtioNetPciBridgeLike;
  readonly #irqSink: IrqSink;
  readonly #mode: VirtioNetPciMode;

  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: VirtioNetPciBridgeLike; irqSink: IrqSink; mode?: VirtioNetPciMode }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
    this.#mode = opts.mode ?? "modern";

    this.deviceId = this.#mode === "transitional" ? VIRTIO_NET_TRANSITIONAL_DEVICE_ID : VIRTIO_NET_MODERN_DEVICE_ID;
    this.bars =
      this.#mode === "transitional"
        ? [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, { kind: "io", size: VIRTIO_LEGACY_IO_BAR2_SIZE }, null, null, null]
        : [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, null, null, null, null];
  }

  initPciConfig(config: Uint8Array): void {
    // Subsystem IDs (Aero Windows 7 virtio contract v1).
    writeU16LE(config, 0x2c, VIRTIO_VENDOR_ID);
    writeU16LE(config, 0x2e, VIRTIO_NET_SUBSYSTEM_DEVICE_ID);

    // PCI status register: Capabilities List bit (bit 4) at offset 0x06.
    config[0x06] = (config[0x06]! | 0x10) & 0xff;
    // Capabilities pointer.
    config[0x34] = 0x50;

    // Vendor-specific virtio-pci capability chain (PCI cap ID 0x09).
    //
    // Layout is fixed by `docs/windows7-virtio-driver-contract.md` §1.4:
    // - BAR0, 0x0000..0x00ff COMMON
    // - BAR0, 0x1000..0x10ff NOTIFY (notify_off_multiplier=4)
    // - BAR0, 0x2000..0x201f ISR
    // - BAR0, 0x3000..0x30ff DEVICE
    //
    // Capabilities are 4-byte aligned and the list is acyclic.
    writeVirtioPciCap(config, 0x50, {
      next: 0x60,
      capLen: 16,
      cfgType: 1,
      bar: 0,
      offset: VIRTIO_MMIO_COMMON_OFFSET,
      length: VIRTIO_MMIO_COMMON_LEN,
    });
    writeVirtioPciCap(config, 0x60, {
      next: 0x74,
      capLen: 20,
      cfgType: 2,
      bar: 0,
      offset: VIRTIO_MMIO_NOTIFY_OFFSET,
      length: VIRTIO_MMIO_NOTIFY_LEN,
      notifyOffMultiplier: VIRTIO_MMIO_NOTIFY_OFF_MULTIPLIER,
    });
    writeVirtioPciCap(config, 0x74, {
      next: 0x84,
      capLen: 16,
      cfgType: 3,
      bar: 0,
      offset: VIRTIO_MMIO_ISR_OFFSET,
      length: VIRTIO_MMIO_ISR_LEN,
    });
    writeVirtioPciCap(config, 0x84, {
      next: 0x00,
      capLen: 16,
      cfgType: 4,
      bar: 0,
      offset: VIRTIO_MMIO_DEVICE_OFFSET,
      length: VIRTIO_MMIO_DEVICE_LEN,
    });
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_MMIO_BAR0_SIZE) return 0;
    // Undefined offsets within BAR0 must read as 0 (contract v1).
    const defined =
      isInRange(off, size, VIRTIO_MMIO_COMMON_OFFSET, VIRTIO_MMIO_COMMON_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_NOTIFY_OFFSET, VIRTIO_MMIO_NOTIFY_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_ISR_OFFSET, VIRTIO_MMIO_ISR_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_DEVICE_OFFSET, VIRTIO_MMIO_DEVICE_LEN);
    if (!defined) return 0;

    let value: number;
    try {
      // BAR0 is 0x4000 bytes, so offset fits in a JS number.
      value = this.#bridge.mmio_read(off >>> 0, size) >>> 0;
    } catch {
      // Virtio contract v1 expects undefined MMIO reads within BAR0 to return 0.
      // Treat device-side errors the same way to avoid spurious all-ones reads
      // confusing drivers.
      value = 0;
    }
    // Reads from the ISR config region are read-to-ack and may deassert the IRQ.
    if (isInRange(off, size, VIRTIO_MMIO_ISR_OFFSET, VIRTIO_MMIO_ISR_LEN)) {
      this.#syncIrq();
    }
    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_MMIO_BAR0_SIZE) return;
    // Undefined offsets within BAR0 must ignore writes (contract v1).
    const defined =
      isInRange(off, size, VIRTIO_MMIO_COMMON_OFFSET, VIRTIO_MMIO_COMMON_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_NOTIFY_OFFSET, VIRTIO_MMIO_NOTIFY_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_ISR_OFFSET, VIRTIO_MMIO_ISR_LEN) ||
      isInRange(off, size, VIRTIO_MMIO_DEVICE_OFFSET, VIRTIO_MMIO_DEVICE_LEN);
    if (!defined) return;
    try {
      this.#bridge.mmio_write(off >>> 0, size, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
    }
    this.#syncIrq();
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 2) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);
    if (this.#mode !== "transitional") return defaultReadValue(size);

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return defaultReadValue(size);

    const bridge = this.#bridge;
    const fn = bridge.io_read;
    if (typeof fn !== "function") return defaultReadValue(size);

    let value: number;
    try {
      value = fn.call(bridge, off, size) >>> 0;
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
    if (this.#mode !== "transitional") return;

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return;

    const bridge = this.#bridge;
    const fn = bridge.io_write;
    if (typeof fn === "function") {
      try {
        fn.call(bridge, off, size, maskToSize(value >>> 0, size));
      } catch {
        // ignore device errors during guest IO
      }
    }
    this.#syncIrq();
  }

  tick(nowMs: number): void {
    if (this.#destroyed) return;

    const bridge = this.#bridge;
    if (typeof bridge.poll === "function") {
      try {
        bridge.poll();
      } catch {
        // ignore device errors during tick
      }
    } else if (typeof bridge.tick === "function") {
      try {
        // Some wasm-bindgen builds enforce method arity; pass `nowMs` only when accepted.
        if (bridge.tick.length >= 1) bridge.tick(nowMs);
        else bridge.tick();
      } catch {
        // ignore device errors during tick
      }
    }

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
      this.#bridge.free();
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    const bridge = this.#bridge as unknown as { irq_level?: unknown; irq_asserted?: unknown };

    let asserted = false;
    try {
      if (typeof bridge.irq_asserted === "function") {
        asserted = Boolean(bridge.irq_asserted.call(this.#bridge));
      } else if (typeof bridge.irq_level === "function") {
        asserted = Boolean(bridge.irq_level.call(this.#bridge));
      }
    } catch {
      asserted = false;
    }
    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}

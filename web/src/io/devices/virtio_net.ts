import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioNetPciBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
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

const VIRTIO_VENDOR_ID = 0x1af4;
const VIRTIO_NET_DEVICE_ID = 0x1041;
const VIRTIO_NET_SUBSYSTEM_DEVICE_ID = 0x0001;
const VIRTIO_NET_CLASS_CODE = 0x02_00_00;
const VIRTIO_CONTRACT_REVISION_ID = 0x01;

// BAR0 size in the Aero Windows 7 virtio contract v1 (see docs/windows7-virtio-driver-contract.md).
const VIRTIO_MMIO_BAR0_SIZE = 0x4000;

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
 * Virtio-net PCI function (modern virtio-pci transport) backed by the WASM `VirtioNetPciBridge`.
 *
 * Exposes a single 64-bit MMIO BAR (BAR0) of size 0x4000 and implements the
 * vendor-specific virtio capability list required by the Aero Windows 7 contract v1.
 */
export class VirtioNetPciDevice implements PciDevice, TickableDevice {
  readonly name = "virtio_net";
  readonly vendorId = VIRTIO_VENDOR_ID;
  readonly deviceId = VIRTIO_NET_DEVICE_ID;
  readonly subsystemVendorId = VIRTIO_VENDOR_ID;
  readonly subsystemId = VIRTIO_NET_SUBSYSTEM_DEVICE_ID;
  readonly classCode = VIRTIO_NET_CLASS_CODE;
  readonly revisionId = VIRTIO_CONTRACT_REVISION_ID;
  readonly irqLine = VIRTIO_NET_IRQ_LINE;
  readonly bdf = { bus: 0, device: 8, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, null, null, null, null];

  readonly #bridge: VirtioNetPciBridgeLike;
  readonly #irqSink: IrqSink;

  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: VirtioNetPciBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
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
      offset: 0x0000,
      length: 0x0100,
    });
    writeVirtioPciCap(config, 0x60, {
      next: 0x74,
      capLen: 20,
      cfgType: 2,
      bar: 0,
      offset: 0x1000,
      length: 0x0100,
      notifyOffMultiplier: 4,
    });
    writeVirtioPciCap(config, 0x74, {
      next: 0x84,
      capLen: 16,
      cfgType: 3,
      bar: 0,
      offset: 0x2000,
      length: 0x0020,
    });
    writeVirtioPciCap(config, 0x84, {
      next: 0x00,
      capLen: 16,
      cfgType: 4,
      bar: 0,
      offset: 0x3000,
      length: 0x0100,
    });
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    let value: number;
    try {
      // BAR0 is 0x4000 bytes, so offset fits in a JS number.
      value = this.#bridge.mmio_read(Number(offset), size) >>> 0;
    } catch {
      // Virtio contract v1 expects undefined MMIO reads within BAR0 to return 0.
      // Treat device-side errors the same way to avoid spurious all-ones reads
      // confusing drivers.
      value = 0;
    }
    this.#syncIrq();
    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    try {
      this.#bridge.mmio_write(Number(offset), size, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
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

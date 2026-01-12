import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciCapability, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioNetPciBridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  /**
   * Advance device-side state (process virtqueues / ring backend).
   *
   * Newer WASM builds may expose `poll()` rather than `tick()`. The wrapper supports both.
   */
  poll?: () => void;
  tick?: (nowMs?: number) => void;
  irq_level?(): boolean;
  irq_asserted?(): boolean;
  free(): void;
};

const VIRTIO_PCI_VENDOR_ID = 0x1af4;
// virtio-net device type is 1, so modern virtio-pci device ID is 0x1040 + 1.
const VIRTIO_NET_DEVICE_ID = 0x1041;
// Contract v1: PCI Revision ID matches contract major version.
const VIRTIO_NET_REVISION_ID = 0x01;
const VIRTIO_NET_CLASS_CODE = 0x02_00_00;

// Keep in sync with `crates/aero-virtio/src/pci.rs` (`bar0_size`) and
// `docs/windows7-virtio-driver-contract.md` (BAR0 layout contract).
export const VIRTIO_PCI_BAR0_MMIO_SIZE = 0x4000;

// IRQ10 is traditionally used by PCI NICs on legacy x86 machines and is already
// used by the E1000 fallback model. Sharing is supported by the IO worker's
// refcounted IRQ sink.
const VIRTIO_NET_IRQ_LINE = 0x0a;

// PCI capability IDs/types for virtio-pci modern transport (contract v1).
const PCI_CAP_ID_VENDOR_SPECIFIC = 0x09;
const VIRTIO_PCI_CAP_COMMON_CFG = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG = 2;
const VIRTIO_PCI_CAP_ISR_CFG = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG = 4;

// Fixed BAR0 layout.
const VIRTIO_MMIO_COMMON_OFFSET = 0x0000;
const VIRTIO_MMIO_NOTIFY_OFFSET = 0x1000;
const VIRTIO_MMIO_ISR_OFFSET = 0x2000;
const VIRTIO_MMIO_DEVICE_OFFSET = 0x3000;
const VIRTIO_MMIO_COMMON_LEN = 0x0100;
const VIRTIO_MMIO_NOTIFY_LEN = 0x0100;
const VIRTIO_MMIO_ISR_LEN = 0x0020;
const VIRTIO_MMIO_DEVICE_LEN = 0x0100;
const VIRTIO_NOTIFY_OFF_MULTIPLIER = 4;

function writeU32LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
  buf[off + 2] = (value >>> 16) & 0xff;
  buf[off + 3] = (value >>> 24) & 0xff;
}

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

function makeVirtioPciCap(opts: { cfgType: number; bar: number; offset: number; length: number }): PciCapability {
  const bytes = new Uint8Array(16);
  bytes[0] = PCI_CAP_ID_VENDOR_SPECIFIC;
  bytes[1] = 0; // next (patched by PciBus)
  bytes[2] = 0; // cap_len (patched by PciBus)
  bytes[3] = opts.cfgType & 0xff;
  bytes[4] = opts.bar & 0xff;
  bytes[5] = 0; // id
  bytes[6] = 0;
  bytes[7] = 0;
  writeU32LE(bytes, 8, opts.offset >>> 0);
  writeU32LE(bytes, 12, opts.length >>> 0);
  return { bytes };
}

function makeVirtioPciNotifyCap(opts: {
  bar: number;
  offset: number;
  length: number;
  notifyOffMultiplier: number;
}): PciCapability {
  const bytes = new Uint8Array(20);
  bytes[0] = PCI_CAP_ID_VENDOR_SPECIFIC;
  bytes[1] = 0; // next (patched by PciBus)
  bytes[2] = 0; // cap_len (patched by PciBus)
  bytes[3] = VIRTIO_PCI_CAP_NOTIFY_CFG;
  bytes[4] = opts.bar & 0xff;
  bytes[5] = 0; // id
  bytes[6] = 0;
  bytes[7] = 0;
  writeU32LE(bytes, 8, opts.offset >>> 0);
  writeU32LE(bytes, 12, opts.length >>> 0);
  writeU32LE(bytes, 16, opts.notifyOffMultiplier >>> 0);
  return { bytes };
}

/**
 * virtio-net PCI function backed by the WASM `VirtioNetPciBridge`.
 *
 * Exposes a single virtio-pci modern BAR0 + virtio vendor-specific capabilities
 * per `AERO-W7-VIRTIO` contract v1.
 */
export class VirtioNetPciDevice implements PciDevice, TickableDevice {
  readonly name = "virtio-net";
  readonly vendorId = VIRTIO_PCI_VENDOR_ID;
  readonly deviceId = VIRTIO_NET_DEVICE_ID;
  readonly subsystemVendorId = VIRTIO_PCI_VENDOR_ID;
  readonly subsystemId = 0x0001;
  readonly classCode = VIRTIO_NET_CLASS_CODE;
  readonly revisionId = VIRTIO_NET_REVISION_ID;
  readonly irqLine = VIRTIO_NET_IRQ_LINE;

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio64", size: VIRTIO_PCI_BAR0_MMIO_SIZE }, null, null, null, null, null];

  readonly capabilities: ReadonlyArray<PciCapability> = [
    makeVirtioPciCap({ cfgType: VIRTIO_PCI_CAP_COMMON_CFG, bar: 0, offset: VIRTIO_MMIO_COMMON_OFFSET, length: VIRTIO_MMIO_COMMON_LEN }),
    makeVirtioPciNotifyCap({ bar: 0, offset: VIRTIO_MMIO_NOTIFY_OFFSET, length: VIRTIO_MMIO_NOTIFY_LEN, notifyOffMultiplier: VIRTIO_NOTIFY_OFF_MULTIPLIER }),
    makeVirtioPciCap({ cfgType: VIRTIO_PCI_CAP_ISR_CFG, bar: 0, offset: VIRTIO_MMIO_ISR_OFFSET, length: VIRTIO_MMIO_ISR_LEN }),
    makeVirtioPciCap({ cfgType: VIRTIO_PCI_CAP_DEVICE_CFG, bar: 0, offset: VIRTIO_MMIO_DEVICE_OFFSET, length: VIRTIO_MMIO_DEVICE_LEN }),
  ];

  readonly #bridge: VirtioNetPciBridgeLike;
  readonly #irqSink: IrqSink;

  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: VirtioNetPciBridgeLike; irqSink: IrqSink }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_PCI_BAR0_MMIO_SIZE) return defaultReadValue(size);

    let value = 0;
    try {
      value = this.#bridge.mmio_read(off >>> 0, size >>> 0) >>> 0;
    } catch {
      value = 0;
    }

    // Reads from the ISR register are read-to-ack and may deassert the IRQ.
    if (off >= VIRTIO_MMIO_ISR_OFFSET && off < VIRTIO_MMIO_ISR_OFFSET + VIRTIO_MMIO_ISR_LEN) {
      this.#syncIrq();
    }

    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > VIRTIO_PCI_BAR0_MMIO_SIZE) return;

    try {
      this.#bridge.mmio_write(off >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest MMIO
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
      if (typeof bridge.irq_level === "function") {
        asserted = Boolean(bridge.irq_level.call(this.#bridge));
      } else if (typeof bridge.irq_asserted === "function") {
        asserted = Boolean(bridge.irq_asserted.call(this.#bridge));
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

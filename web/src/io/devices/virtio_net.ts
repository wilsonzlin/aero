import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioNetPciBridgeLike = {
  mmio_read?: (offset: number, size: number) => number;
  mmio_write?: (offset: number, size: number, value: number) => void;
  io_read?: (offset: number, size: number) => number;
  io_write?: (offset: number, size: number, value: number) => void;
  tick?: (nowMs?: number) => void;
  irq_level?: () => boolean;
  irq_asserted?: () => boolean;
  free: () => void;
};

const VIRTIO_PCI_VENDOR_ID = 0x1af4;
// virtio-net device type is 1, so transitional device ID is 0x1000 + (1 - 1).
const VIRTIO_NET_TRANSITIONAL_DEVICE_ID = 0x1000;
const VIRTIO_NET_CLASS_CODE = 0x02_00_00;

// Keep in sync with `crates/aero-virtio/src/pci.rs` (`bar0_size`).
export const VIRTIO_PCI_BAR0_MMIO_SIZE = 0x4000;
// Keep in sync with `crates/aero-virtio/src/pci.rs` (`bar2_size` when legacy IO is enabled).
const VIRTIO_PCI_LEGACY_IO_SIZE = 0x100;

const VIRTIO_NET_IRQ_LINE = 0x0b;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal virtio-net PCI function backed by the WASM `VirtioNetPciBridge`.
 *
 * Exposes:
 * - BAR0: 0x4000 MMIO window containing virtio PCI capabilities (modern transport)
 * - BAR2: 0x100 I/O port window for the legacy virtio 0.9 transport (transitional device)
 */
export class VirtioNetPciDevice implements PciDevice, TickableDevice {
  readonly name = "virtio-net";
  readonly vendorId = VIRTIO_PCI_VENDOR_ID;
  readonly deviceId = VIRTIO_NET_TRANSITIONAL_DEVICE_ID;
  readonly classCode = VIRTIO_NET_CLASS_CODE;
  readonly revisionId = 0x00;
  readonly irqLine = VIRTIO_NET_IRQ_LINE;

  readonly bars: ReadonlyArray<PciBar | null> = [
    { kind: "mmio32", size: VIRTIO_PCI_BAR0_MMIO_SIZE },
    null,
    { kind: "io", size: VIRTIO_PCI_LEGACY_IO_SIZE },
    null,
    null,
    null,
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

    const bridge = this.#bridge;
    const fn = bridge.mmio_read;
    if (typeof fn !== "function") return defaultReadValue(size);
    try {
      const value = fn.call(bridge, Number(offset), size >>> 0) >>> 0;
      return maskToSize(value, size);
    } catch {
      return defaultReadValue(size);
    }
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const bridge = this.#bridge;
    const fn = bridge.mmio_write;
    if (typeof fn === "function") {
      try {
        fn.call(bridge, Number(offset), size >>> 0, maskToSize(value >>> 0, size));
      } catch {
        // ignore device errors during guest IO
      }
    }

    this.#syncIrq();
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 2) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const bridge = this.#bridge;
    const fn = bridge.io_read;
    if (typeof fn !== "function") return defaultReadValue(size);
    try {
      const value = fn.call(bridge, offset >>> 0, size >>> 0) >>> 0;
      return maskToSize(value, size);
    } catch {
      return defaultReadValue(size);
    }
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 2) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const bridge = this.#bridge;
    const fn = bridge.io_write;
    if (typeof fn === "function") {
      try {
        fn.call(bridge, offset >>> 0, size >>> 0, maskToSize(value >>> 0, size));
      } catch {
        // ignore device errors during guest IO
      }
    }

    this.#syncIrq();
  }

  tick(nowMs: number): void {
    if (this.#destroyed) return;

    const bridge = this.#bridge;
    const tick = (bridge as unknown as { tick?: unknown }).tick;
    if (typeof tick === "function") {
      try {
        // Some wasm-bindgen builds enforce method arity; pass `nowMs` only when accepted.
        if (tick.length >= 1) {
          (tick as (nowMs: number) => void).call(bridge, nowMs);
        } else {
          (tick as () => void).call(bridge);
        }
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

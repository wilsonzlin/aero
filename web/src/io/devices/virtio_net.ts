import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type VirtioNetPciBridgeLike = {
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
  /**
   * Optional poll hook (some WASM builds expose `poll()` rather than `tick()`).
   */
  poll?: () => void;
  /**
   * Optional per-tick hook (canonical in current WASM builds).
   */
  tick?: (nowMs?: number) => void;
  /**
   * Optional hook for mirroring PCI command register writes into the WASM bridge.
   *
   * When present, this is used to enforce DMA gating based on Bus Master Enable.
   */
  set_pci_command?(command: number): void;
  irq_level?(): boolean;
  irq_asserted?(): boolean;
  /**
   * Best-effort `NET_TX`/`NET_RX` ring backend stats (when exposed by the WASM bridge).
   */
  virtio_net_stats?: () =>
    | {
        tx_pushed_frames: bigint;
        tx_pushed_bytes?: bigint;
        tx_dropped_oversize: bigint;
        tx_dropped_oversize_bytes?: bigint;
        tx_dropped_full: bigint;
        tx_dropped_full_bytes?: bigint;
        rx_popped_frames: bigint;
        rx_popped_bytes?: bigint;
        rx_dropped_oversize: bigint;
        rx_dropped_oversize_bytes?: bigint;
        rx_corrupt: bigint;
        rx_broken?: boolean;
      }
    | null;
  free(): void;
};

export type VirtioNetPciMode = "modern" | "transitional" | "legacy";

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
 * Virtio-net PCI function (virtio-pci modern/transitional/legacy transport) backed by the WASM `VirtioNetPciBridge`.
 *
 * Exposes:
 * - BAR0: 64-bit MMIO BAR, size 0x4000 (Aero Windows 7 virtio contract v1 modern layout).
 * - BAR2 (transitional/legacy modes): legacy I/O port register block, size 0x100.
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
  readonly #mmioReadFn: (offset: number, size: number) => number;
  readonly #mmioWriteFn: (offset: number, size: number, value: number) => void;
  readonly #freeFn: () => void;
  readonly #setPciCommandFn: ((command: number) => void) | null;
  readonly #irqSink: IrqSink;
  readonly #mode: VirtioNetPciMode;

  #pciCommand = 0;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: VirtioNetPciBridgeLike; irqSink: IrqSink; mode?: VirtioNetPciMode }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
    this.#mode = opts.mode ?? "modern";

    // Backwards compatibility: accept both snake_case and camelCase WASM bridge exports and
    // invoke extracted method references via `.call(bridge, ...)` to avoid wasm-bindgen `this`
    // binding pitfalls.
    const bridgeAny = opts.bridge as unknown as Record<string, unknown>;
    const mmioRead = bridgeAny.mmio_read ?? bridgeAny.mmioRead;
    const mmioWrite = bridgeAny.mmio_write ?? bridgeAny.mmioWrite;
    const free = bridgeAny.free;
    if (typeof mmioRead !== "function" || typeof mmioWrite !== "function") {
      throw new Error("virtio-net bridge missing mmio_read/mmioRead or mmio_write/mmioWrite exports.");
    }
    if (typeof free !== "function") {
      throw new Error("virtio-net bridge missing free() export.");
    }
    this.#mmioReadFn = mmioRead as (offset: number, size: number) => number;
    this.#mmioWriteFn = mmioWrite as (offset: number, size: number, value: number) => void;
    this.#freeFn = free as () => void;

    const setCmd = bridgeAny.set_pci_command ?? bridgeAny.setPciCommand;
    this.#setPciCommandFn = typeof setCmd === "function" ? (setCmd as (command: number) => void) : null;

    this.deviceId = this.#mode === "modern" ? VIRTIO_NET_MODERN_DEVICE_ID : VIRTIO_NET_TRANSITIONAL_DEVICE_ID;
    this.bars =
      this.#mode === "modern"
        ? [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, null, null, null, null]
        : [{ kind: "mmio64", size: VIRTIO_MMIO_BAR0_SIZE }, null, { kind: "io", size: VIRTIO_LEGACY_IO_BAR2_SIZE }, null, null, null];
  }

  initPciConfig(config: Uint8Array): void {
    // Subsystem IDs (Aero Windows 7 virtio contract v1).
    writeU16LE(config, 0x2c, VIRTIO_VENDOR_ID);
    writeU16LE(config, 0x2e, VIRTIO_NET_SUBSYSTEM_DEVICE_ID);

    // Legacy-only mode intentionally disables modern virtio-pci capabilities so guests take the
    // virtio 0.9 I/O-port transport path.
    if (this.#mode === "legacy") {
      return;
    }

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
      value = this.#mmioReadFn.call(this.#bridge, off >>> 0, size) >>> 0;
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
      this.#mmioWriteFn.call(this.#bridge, off >>> 0, size, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
    }
    this.#syncIrq();
  }

  onPciCommandWrite(command: number): void {
    if (this.#destroyed) return;
    this.#pciCommand = command & 0xffff;

    // Mirror into the WASM bridge so it can enforce PCI Bus Master Enable gating for DMA.
    const setCmd = this.#setPciCommandFn;
    if (typeof setCmd === "function") {
      try {
        setCmd.call(this.#bridge, this.#pciCommand >>> 0);
      } catch {
        // ignore device errors during PCI config writes
      }
    }

    // Interrupt Disable bit can immediately drop INTx level.
    this.#syncIrq();
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 2) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);
    if (this.#mode === "modern") return defaultReadValue(size);

    const off = offset >>> 0;
    if (off + size > VIRTIO_LEGACY_IO_BAR2_SIZE) return defaultReadValue(size);

    const bridge = this.#bridge as unknown as Record<string, unknown>;
    const fn =
      (typeof bridge.legacy_io_read === "function"
        ? (bridge.legacy_io_read as (offset: number, size: number) => number)
        : typeof bridge.legacyIoRead === "function"
          ? (bridge.legacyIoRead as (offset: number, size: number) => number)
          : typeof bridge.io_read === "function"
            ? (bridge.io_read as (offset: number, size: number) => number)
            : typeof bridge.ioRead === "function"
              ? (bridge.ioRead as (offset: number, size: number) => number)
              : undefined) ?? undefined;
    if (typeof fn !== "function") return defaultReadValue(size);

    let value: number;
    try {
      value = fn.call(this.#bridge, off, size) >>> 0;
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

    const bridge = this.#bridge as unknown as Record<string, unknown>;
    const fn =
      (typeof bridge.legacy_io_write === "function"
        ? (bridge.legacy_io_write as (offset: number, size: number, value: number) => void)
        : typeof bridge.legacyIoWrite === "function"
          ? (bridge.legacyIoWrite as (offset: number, size: number, value: number) => void)
          : typeof bridge.io_write === "function"
            ? (bridge.io_write as (offset: number, size: number, value: number) => void)
            : typeof bridge.ioWrite === "function"
              ? (bridge.ioWrite as (offset: number, size: number, value: number) => void)
              : undefined) ?? undefined;
    if (typeof fn === "function") {
      try {
        fn.call(this.#bridge, off, size, maskToSize(value >>> 0, size));
      } catch {
        // ignore device errors during guest IO
      }
    }
    this.#syncIrq();
  }

  tick(nowMs: number): void {
    if (this.#destroyed) return;

    // PCI Bus Master Enable (command bit 2) gates whether the device is allowed to DMA into guest
    // memory (virtqueue descriptor reads / used-ring writes / RX buffer fills).
    //
    // Mirror/gating note:
    // - Newer WASM builds can also enforce this via `set_pci_command`, but keep a wrapper-side gate
    //   so older builds remain correct and we avoid invoking poll/tick unnecessarily.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;

    const bridge = this.#bridge;
    if (busMasterEnabled) {
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
      this.#freeFn.call(this.#bridge);
    } catch {
      // ignore
    }
  }

  #syncIrq(): void {
    const bridge = this.#bridge as unknown as Record<string, unknown>;

    let asserted = false;
    try {
      const irqAsserted = bridge.irq_asserted ?? bridge.irqAsserted;
      const irqLevel = bridge.irq_level ?? bridge.irqLevel;
      if (typeof irqAsserted === "function") {
        asserted = Boolean((irqAsserted as () => unknown).call(this.#bridge));
      } else if (typeof irqLevel === "function") {
        asserted = Boolean((irqLevel as () => unknown).call(this.#bridge));
      }
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

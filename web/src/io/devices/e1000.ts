import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";
import type { RingBuffer } from "../../ipc/ring_buffer.ts";

export type E1000BridgeLike = {
  pci_config_write?: (offset: number, size: number, value: number) => void;
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  io_read(offset: number, size: number): number;
  io_write(offset: number, size: number, value: number): void;
  poll(): void;
  receive_frame(frame: Uint8Array): void;
  // wasm-bindgen represents `Option<Uint8Array>` as `undefined` in most builds,
  // but older bindings or manual shims may use `null`. Accept both.
  pop_tx_frame(): Uint8Array | null | undefined;
  irq_level(): boolean;
  mac_addr?: () => Uint8Array;

  /**
   * Optional PCI command register mirror (offset 0x04, 16-bit).
   *
   * Newer E1000 device models gate DMA on Bus Master Enable, so the JS PCI bus must forward
   * command register writes into the WASM device model.
   */
  set_pci_command?: (command: number) => void;

  /**
   * Deterministic snapshot/restore helpers (aero-io-snapshot TLV bytes).
   *
   * Optional for older WASM builds.
   */
  save_state?: () => Uint8Array;
  load_state?: (bytes: Uint8Array) => void;
  snapshot_state?: () => Uint8Array;
  restore_state?: (bytes: Uint8Array) => void;
  free(): void;
};

const E1000_CLASS_CODE = 0x02_00_00;
const E1000_MMIO_BAR_SIZE = 0x20_000;
const E1000_IO_BAR_SIZE = 0x40;

// IRQ10 is traditionally used by PCI NICs on legacy x86 machines and is currently
// unused by the other built-in devices (i8042=IRQ1, UART=IRQ4, UHCI=IRQ11).
const E1000_IRQ_LINE = 0x0a;

// Avoid spending unbounded time draining rings if the tab was backgrounded.
const MAX_FRAMES_PER_TICK = 128;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal E1000 PCI function backed by the WASM `E1000Bridge`.
 *
 * Exposes:
 * - BAR0 (MMIO32): E1000 register window
 * - BAR1 (IO): IOADDR/IODATA window
 *
 * The tick loop wires the device's host-facing TX/RX queues to the NET_TX/NET_RX
 * shared rings used by `net.worker.ts`.
 */
export class E1000PciDevice implements PciDevice, TickableDevice {
  readonly name = "e1000";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x100e;
  readonly classCode = E1000_CLASS_CODE;
  readonly revisionId = 0x00;
  readonly irqLine = E1000_IRQ_LINE;

  // QEMU places the E1000 at 00:05.0 by default; keep a stable address so guest driver
  // installation and test snapshots are deterministic.
  readonly bdf = { bus: 0, device: 5, function: 0 };

  readonly bars: ReadonlyArray<PciBar | null> = [
    { kind: "mmio32", size: E1000_MMIO_BAR_SIZE },
    { kind: "io", size: E1000_IO_BAR_SIZE },
    null,
    null,
    null,
    null,
  ];

  readonly #bridge: E1000BridgeLike;
  readonly #irqSink: IrqSink;
  readonly #netTxRing: RingBuffer;
  readonly #netRxRing: RingBuffer;

  // If the NET_TX ring is full, hold exactly one pending frame so we don't drop it.
  #pendingTxFrame: Uint8Array | null = null;
  #irqLevel = false;
  #pciCommand = 0;
  #destroyed = false;

  constructor(opts: { bridge: E1000BridgeLike; irqSink: IrqSink; netTxRing: RingBuffer; netRxRing: RingBuffer }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
    this.#netTxRing = opts.netTxRing;
    this.#netRxRing = opts.netRxRing;
  }

  pciConfigWrite(offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    const fn = this.#bridge.pci_config_write;
    if (!fn) return;
    if (size !== 1 && size !== 2 && size !== 4) return;
    try {
      fn.call(this.#bridge, offset >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest PCI config writes
    }
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > E1000_MMIO_BAR_SIZE) return defaultReadValue(size);

    let value = defaultReadValue(size);
    try {
      value = this.#bridge.mmio_read(off >>> 0, size >>> 0) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }

    // Reads of ICR can clear the IRQ; keep the line level accurate.
    this.#syncIrq();
    return maskToSize(value, size);
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 0) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > E1000_MMIO_BAR_SIZE) return;

    try {
      this.#bridge.mmio_write(off >>> 0, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest MMIO
    }
    this.#syncIrq();
  }

  ioRead(barIndex: number, offset: number, size: number): number {
    if (this.#destroyed) return defaultReadValue(size);
    if (barIndex !== 1) return defaultReadValue(size);
    if (size !== 1 && size !== 2 && size !== 4) return defaultReadValue(size);

    const off = offset >>> 0;
    if (off + size > E1000_IO_BAR_SIZE) return defaultReadValue(size);

    let value = defaultReadValue(size);
    try {
      value = this.#bridge.io_read(off, size >>> 0) >>> 0;
    } catch {
      value = defaultReadValue(size);
    }

    // Reads via IODATA can touch ICR.
    this.#syncIrq();
    return maskToSize(value, size);
  }

  ioWrite(barIndex: number, offset: number, size: number, value: number): void {
    if (this.#destroyed) return;
    if (barIndex !== 1) return;
    if (size !== 1 && size !== 2 && size !== 4) return;

    const off = offset >>> 0;
    if (off + size > E1000_IO_BAR_SIZE) return;

    try {
      this.#bridge.io_write(off, size >>> 0, maskToSize(value >>> 0, size));
    } catch {
      // ignore device errors during guest IO
    }
    this.#syncIrq();
  }

  tick(_nowMs: number): void {
    if (this.#destroyed) return;

    this.#pumpRxRing();

    // PCI Bus Master Enable (command bit 2) gates whether the device is allowed to DMA into guest
    // memory (descriptor reads/writes and RX buffer fills).
    //
    // Newer WASM builds expose `set_pci_command` so the Rust device model can enforce this gate
    // internally. However, older builds may default BME to enabled; enforce the gate here so we
    // don't DMA unless the guest explicitly enables bus mastering.
    const busMasterEnabled = (this.#pciCommand & (1 << 2)) !== 0;
    if (busMasterEnabled) {
      try {
        this.#bridge.poll();
      } catch {
        // ignore device errors during tick
      }
    }

    this.#pumpTxRing();
    this.#syncIrq();
  }

  destroy(): void {
    if (this.#destroyed) return;
    this.#destroyed = true;

    if (this.#irqLevel) {
      this.#irqSink.lowerIrq(this.irqLine);
      this.#irqLevel = false;
    }

    this.#pendingTxFrame = null;

    try {
      this.#bridge.free();
    } catch {
      // ignore
    }
  }

  /**
   * Restore JS wrapper state after a VM snapshot restore.
   *
   * The WASM `E1000Bridge` snapshot blob does not include transient JS-side state
   * such as the pending TX frame (already popped from WASM but not yet pushed
   * into NET_TX) or the cached IRQ line level. Without resetting these fields a
   * snapshot restore in the same IO worker instance can:
   * - replay a "future" TX frame after restoring an older device state, and/or
   * - leave the IRQ sink refcount in an inconsistent asserted/deasserted state.
   */
  onSnapshotRestore(): void {
    if (this.#destroyed) return;

    // Drop any frame that was popped from the WASM device before restore but not
    // yet emitted to the host network worker.
    this.#pendingTxFrame = null;

    // Force the IRQ sink back to a clean base level before resyncing. This
    // avoids leaving the refcount elevated when restore rewinds the device state.
    if (this.#irqLevel) {
      this.#irqSink.lowerIrq(this.irqLine);
      this.#irqLevel = false;
    }

    // Re-evaluate the restored bridge IRQ level and forward any transition.
    this.#syncIrq();
  }

  onPciCommandWrite(command: number): void {
    if (this.#destroyed) return;

    const cmd = command & 0xffff;
    this.#pciCommand = cmd;

    // Mirror into the WASM device model so DMA gating (Bus Master Enable) is coherent with the
    // JS PCI config space.
    const setCmd = this.#bridge.set_pci_command;
    if (typeof setCmd === "function") {
      try {
        setCmd.call(this.#bridge, cmd >>> 0);
      } catch {
        // ignore device errors during PCI config writes
      }
    }

    // INTx disable bit can immediately drop the line; keep the sink coherent.
    this.#syncIrq();
  }

  #pumpRxRing(): void {
    const ring = this.#netRxRing;
    const bridge = this.#bridge;

    for (let i = 0; i < MAX_FRAMES_PER_TICK; i++) {
      const didConsume = ring.consumeNext((frame) => {
        try {
          bridge.receive_frame(frame);
        } catch {
          // ignore malformed frames
        }
      });
      if (!didConsume) break;
    }
  }

  #pumpTxRing(): void {
    const ring = this.#netTxRing;
    const bridge = this.#bridge;

    // If the NET_TX ring was full, retry the pending frame first and avoid
    // popping more frames from WASM until we can flush it.
    if (this.#pendingTxFrame) {
      if (!ring.tryPush(this.#pendingTxFrame)) return;
      this.#pendingTxFrame = null;
    }

    for (let i = 0; i < MAX_FRAMES_PER_TICK; i++) {
      let frame: Uint8Array | null | undefined;
      try {
        frame = bridge.pop_tx_frame();
      } catch {
        frame = undefined;
      }
      if (!frame) return;

      if (!ring.tryPush(frame)) {
        this.#pendingTxFrame = frame;
        return;
      }
    }
  }

  #syncIrq(): void {
    // E1000 uses PCI INTx, which is level-triggered. The WASM bridge exposes the current INTx
    // level; forward only level transitions to the runtime (`raiseIrq` on 0→1, `lowerIrq` on 1→0).
    //
    // See `docs/irq-semantics.md`.
    let asserted = false;
    try {
      asserted = Boolean(this.#bridge.irq_level());
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

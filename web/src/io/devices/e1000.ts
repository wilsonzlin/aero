import { RingBuffer } from "../../ipc/ring_buffer.ts";
import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";

export type E1000BridgeLike = {
  mmio_read(offset: number, size: number): number;
  mmio_write(offset: number, size: number, value: number): void;
  io_read(offset: number, size: number): number;
  io_write(offset: number, size: number, value: number): void;
  poll(): void;
  receive_frame(frame: Uint8Array): void;
  pop_tx_frame(): Uint8Array | null | undefined;
  irq_level(): boolean;
  mac_addr?: () => Uint8Array;
  free(): void;
};

const E1000_CLASS_CODE = 0x02_00_00;
const E1000_MMIO_BAR_SIZE = 0x20_000;
const E1000_IO_BAR_SIZE = 0x40;

// IRQ10 is traditionally used by PCI NICs on legacy x86 machines and is currently
// unused by the other built-in devices (i8042=IRQ1, UART=IRQ4, UHCI=IRQ11).
const E1000_IRQ_LINE = 0x0a;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

/**
 * Minimal Intel E1000 PCI function backed by the WASM `E1000Bridge`.
 *
 * The guest driver programs RX/TX descriptor rings in guest RAM; the Rust device
 * model DMAs directly via the wasm linear memory guest mapping.
 *
 * Raw Ethernet frames are forwarded between the device model and the net worker
 * through the IO_IPC_NET_{TX,RX} rings.
 */
export class E1000PciDevice implements PciDevice, TickableDevice {
  readonly name = "e1000";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x100e;
  readonly classCode = E1000_CLASS_CODE;
  readonly irqLine = E1000_IRQ_LINE;

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
  readonly #netTx: RingBuffer;
  readonly #netRx: RingBuffer;

  #irqLevel = false;
  #destroyed = false;
  #txDrops = 0;

  constructor(opts: { bridge: E1000BridgeLike; irqSink: IrqSink; netTxRing: RingBuffer; netRxRing: RingBuffer }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
    this.#netTx = opts.netTxRing;
    this.#netRx = opts.netRxRing;
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

    try {
      this.#bridge.poll();
    } catch {
      // ignore poll errors
    }

    // Drain guest->host frames (E1000 TX queue -> NET_TX ring).
    // eslint-disable-next-line no-constant-condition
    while (true) {
      let frame: Uint8Array | null | undefined;
      try {
        frame = this.#bridge.pop_tx_frame();
      } catch {
        frame = null;
      }
      if (!frame) break;
      if (!this.#netTx.tryPush(frame)) {
        this.#txDrops++;
      }
    }

    // Drain host->guest frames (NET_RX ring -> E1000 RX path).
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const frame = this.#netRx.tryPop();
      if (!frame) break;
      try {
        this.#bridge.receive_frame(frame);
      } catch {
        // ignore malformed frames
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

  get txDrops(): number {
    return this.#txDrops;
  }

  #syncIrq(): void {
    let asserted = false;
    try {
      asserted = Boolean(this.#bridge.irq_level());
    } catch {
      asserted = false;
    }
    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}

import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { IrqSink, TickableDevice } from "../device_manager.ts";
import type { RingBuffer } from "../../ipc/ring_buffer.ts";

export type E1000BridgeLike = {
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
  free(): void;
};

const E1000_CLASS_CODE = 0x02_00_00;
const E1000_MMIO_BAR_SIZE = 0x20_000;
const E1000_IO_BAR_SIZE = 0x40;

// IRQ10 is traditionally used by PCI NICs on legacy x86 machines and is currently
// unused by the other built-in devices (i8042=IRQ1, UART=IRQ4, UHCI=IRQ11).
const E1000_IRQ_LINE = 0x0a;

const MAX_FRAMES_PER_TICK = 128;

function maskToSize(value: number, size: number): number {
  if (size === 1) return value & 0xff;
  if (size === 2) return value & 0xffff;
  return value >>> 0;
}

export class E1000PciDevice implements PciDevice, TickableDevice {
  readonly name = "e1000";
  readonly vendorId = 0x8086;
  readonly deviceId = 0x100e;
  readonly classCode = E1000_CLASS_CODE;
  readonly revisionId = 0x00;
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
  readonly #netTxRing: RingBuffer;
  readonly #netRxRing: RingBuffer;

  #pendingTxFrame: Uint8Array | null = null;
  #irqLevel = false;
  #destroyed = false;

  constructor(opts: { bridge: E1000BridgeLike; irqSink: IrqSink; netTxRing: RingBuffer; netRxRing: RingBuffer }) {
    this.#bridge = opts.bridge;
    this.#irqSink = opts.irqSink;
    this.#netTxRing = opts.netTxRing;
    this.#netRxRing = opts.netRxRing;
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

    try {
      this.#bridge.poll();
    } catch {
      // ignore device errors during tick
    }

    this.#pumpRxRing();
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

    try {
      this.#bridge.free();
    } catch {
      // ignore
    }
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

    if (asserted === this.#irqLevel) return;
    this.#irqLevel = asserted;
    if (asserted) this.#irqSink.raiseIrq(this.irqLine);
    else this.#irqSink.lowerIrq(this.irqLine);
  }
}

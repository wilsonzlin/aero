import { SharedRingBuffer } from "./ring_buffer.ts";
import {
  IO_MESSAGE_STRIDE_U32,
  IO_OP_IRQ_LOWER,
  IO_OP_IRQ_RAISE,
  IO_OP_MMIO_READ,
  IO_OP_MMIO_WRITE,
  IO_OP_PORT_READ,
  IO_OP_PORT_WRITE,
  IO_OP_RESP,
  decodeU64,
  writeIoMessage,
} from "./io_protocol.ts";
import {
  IRQ_REFCOUNT_ASSERT,
  IRQ_REFCOUNT_DEASSERT,
  IRQ_REFCOUNT_SATURATED,
  IRQ_REFCOUNT_UNDERFLOW,
  applyIrqRefCountChange,
} from "../irq_refcount.ts";
import type { IrqSink } from "../device_manager.ts";

const IS_DEV = (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true;

export interface IoDispatchTarget {
  portRead(port: number, size: number): number;
  portWrite(port: number, size: number, value: number): void;
  mmioRead(addr: bigint, size: number): number;
  mmioWrite(addr: bigint, size: number, value: number): void;
  tick(nowMs: number): void;
}

export interface IoServerOptions {
  tickIntervalMs?: number;
}

export class IoServer implements IrqSink {
  readonly #req: SharedRingBuffer;
  readonly #resp: SharedRingBuffer;
  readonly #target: IoDispatchTarget;
  readonly #tickIntervalMs: number;

  readonly #rx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  readonly #tx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  readonly #irqRefCounts = new Uint16Array(256);
  readonly #irqWarnedUnderflow = new Uint8Array(256);
  readonly #irqWarnedSaturated = new Uint8Array(256);

  constructor(reqRing: SharedRingBuffer, respRing: SharedRingBuffer, target: IoDispatchTarget, opts: IoServerOptions = {}) {
    if (reqRing.stride !== IO_MESSAGE_STRIDE_U32) {
      throw new Error(`reqRing stride ${reqRing.stride} != ${IO_MESSAGE_STRIDE_U32}`);
    }
    if (respRing.stride !== IO_MESSAGE_STRIDE_U32) {
      throw new Error(`respRing stride ${respRing.stride} != ${IO_MESSAGE_STRIDE_U32}`);
    }
    this.#req = reqRing;
    this.#resp = respRing;
    this.#target = target;
    this.#tickIntervalMs = opts.tickIntervalMs ?? 5;
  }

  raiseIrq(irq: number): void {
    // IRQs are transported as line level transitions (assert/deassert). Edge-triggered sources
    // are represented as explicit pulses (raise then lower). See `docs/irq-semantics.md`.
    const idx = irq & 0xff;
    const flags = applyIrqRefCountChange(this.#irqRefCounts, idx, true);
    if (flags & IRQ_REFCOUNT_ASSERT) this.#sendIrq(IO_OP_IRQ_RAISE, idx);
    if (IS_DEV && (flags & IRQ_REFCOUNT_SATURATED) && this.#irqWarnedSaturated[idx] === 0) {
      this.#irqWarnedSaturated[idx] = 1;
      console.warn(`[io_server] IRQ${idx} refcount saturated at 0xffff (raiseIrq without matching lowerIrq?)`);
    }
  }

  lowerIrq(irq: number): void {
    const idx = irq & 0xff;
    const flags = applyIrqRefCountChange(this.#irqRefCounts, idx, false);
    if (flags & IRQ_REFCOUNT_DEASSERT) this.#sendIrq(IO_OP_IRQ_LOWER, idx);
    if (IS_DEV && (flags & IRQ_REFCOUNT_UNDERFLOW) && this.#irqWarnedUnderflow[idx] === 0) {
      this.#irqWarnedUnderflow[idx] = 1;
      console.warn(`[io_server] IRQ${idx} refcount underflow (lowerIrq while already deasserted)`);
    }
  }

  #sendIrq(type: number, irq: number): void {
    writeIoMessage(this.#tx, {
      type,
      id: 0,
      addrLo: irq & 0xff,
      addrHi: 0,
      size: 0,
      value: 0,
    });
    const ok = this.#resp.pushBlocking(this.#tx);
    if (!ok) throw new Error("pushBlocking unexpectedly timed out sending IRQ");
  }

  /**
   * Main worker loop. This is intended to run inside the dedicated I/O worker.
   * It uses Atomics.wait() for efficient blocking when idle, while still
   * calling `tick()` periodically for time-based device progress.
   */
  run(stopSignal?: Int32Array): void {
    let nextTickAt = (typeof performance !== "undefined" ? performance.now() : Date.now()) + this.#tickIntervalMs;

    while (true) {
      if (stopSignal && Atomics.load(stopSignal, 0) !== 0) return;
      // Fast path: drain requests without waiting.
      const got = this.#req.popInto(this.#rx);
      if (got) {
        this.#handleRequest();
      } else {
        const now = typeof performance !== "undefined" ? performance.now() : Date.now();
        const timeout = Math.max(0, nextTickAt - now);
        if (timeout === 0) {
          this.#target.tick(now);
          nextTickAt = now + this.#tickIntervalMs;
          continue;
        }

        const wokeWithReq = this.#req.popBlockingInto(this.#rx, timeout);
        if (wokeWithReq) {
          this.#handleRequest();
        } else {
          const tickNow = typeof performance !== "undefined" ? performance.now() : Date.now();
          this.#target.tick(tickNow);
          nextTickAt = tickNow + this.#tickIntervalMs;
          continue;
        }
      }

      const after = typeof performance !== "undefined" ? performance.now() : Date.now();
      if (after >= nextTickAt) {
        this.#target.tick(after);
        nextTickAt = after + this.#tickIntervalMs;
      }
    }
  }

  #handleRequest(): void {
    const type = this.#rx[0]!;
    const id = this.#rx[1]!;
    const addrLo = this.#rx[2]!;
    const addrHi = this.#rx[3]!;
    const size = this.#rx[4]!;
    const value = this.#rx[5]!;

    let respValue = 0;

    switch (type) {
      case IO_OP_PORT_READ:
        respValue = this.#target.portRead(addrLo & 0xffff, size);
        break;
      case IO_OP_PORT_WRITE:
        this.#target.portWrite(addrLo & 0xffff, size, value);
        respValue = 0;
        break;
      case IO_OP_MMIO_READ:
        respValue = this.#target.mmioRead(decodeU64(addrLo, addrHi), size);
        break;
      case IO_OP_MMIO_WRITE:
        this.#target.mmioWrite(decodeU64(addrLo, addrHi), size, value);
        respValue = 0;
        break;
      default:
        // Unknown request; respond with 0 to avoid deadlocking the CPU.
        respValue = 0;
        break;
    }

    writeIoMessage(this.#tx, {
      type: IO_OP_RESP,
      id,
      addrLo: type,
      addrHi: 0,
      size: 0,
      value: respValue >>> 0,
    });

    const ok = this.#resp.pushBlocking(this.#tx);
    if (!ok) throw new Error("pushBlocking unexpectedly timed out sending response");
  }
}

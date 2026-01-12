import { SharedRingBuffer } from "./ring_buffer.ts";
import {
  IO_MESSAGE_STRIDE_U32,
  IO_OP_A20_SET,
  IO_OP_IRQ_LOWER,
  IO_OP_IRQ_RAISE,
  IO_OP_MMIO_READ,
  IO_OP_MMIO_WRITE,
  IO_OP_PORT_READ,
  IO_OP_PORT_WRITE,
  IO_OP_RESP,
  IO_OP_RESET_REQUEST,
  IO_OP_SERIAL_OUT,
  decodeU64,
  encodeU64,
  writeIoMessage,
} from "./io_protocol.ts";

/**
 * IRQ callback invoked when the IO worker emits an IRQ message on the response ring.
 *
 * `level=true` corresponds to {@link IO_OP_IRQ_RAISE} (line asserted) and `level=false`
 * corresponds to {@link IO_OP_IRQ_LOWER} (line deasserted).
 *
 * These events model IRQ *line levels*. Edge-triggered interrupts are represented as explicit
 * pulses (0→1→0).
 *
 * See `docs/irq-semantics.md`.
 */
export type IrqCallback = (irq: number, level: boolean) => void;
export type A20Callback = (enabled: boolean) => void;
export type ResetCallback = () => void;
export type SerialOutputCallback = (port: number, data: Uint8Array) => void;

export interface IoClientOptions {
  onIrq?: IrqCallback;
  onA20?: A20Callback;
  onReset?: ResetCallback;
  onSerialOutput?: SerialOutputCallback;
}

/**
 * Synchronous (blocking) CPU-side client for I/O requests. Intended to be used
 * inside the dedicated CPU worker thread, where Atomics.wait() is permitted.
 */
export class IoClient {
  readonly #req: SharedRingBuffer;
  readonly #resp: SharedRingBuffer;
  readonly #onIrq?: IrqCallback;
  readonly #onA20?: A20Callback;
  readonly #onReset?: ResetCallback;
  readonly #onSerialOutput?: SerialOutputCallback;

  #nextId = 1;

  readonly #tx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  readonly #rx = new Uint32Array(IO_MESSAGE_STRIDE_U32);

  constructor(reqRing: SharedRingBuffer, respRing: SharedRingBuffer, opts: IoClientOptions = {}) {
    if (reqRing.stride !== IO_MESSAGE_STRIDE_U32) {
      throw new Error(`reqRing stride ${reqRing.stride} != ${IO_MESSAGE_STRIDE_U32}`);
    }
    if (respRing.stride !== IO_MESSAGE_STRIDE_U32) {
      throw new Error(`respRing stride ${respRing.stride} != ${IO_MESSAGE_STRIDE_U32}`);
    }
    this.#req = reqRing;
    this.#resp = respRing;
    this.#onIrq = opts.onIrq;
    this.#onA20 = opts.onA20;
    this.#onReset = opts.onReset;
    this.#onSerialOutput = opts.onSerialOutput;
  }

  portRead(port: number, size: number): number {
    return this.#rpcRead(IO_OP_PORT_READ, BigInt(port & 0xffff), size);
  }

  portWrite(port: number, size: number, value: number): void {
    this.#rpcWrite(IO_OP_PORT_WRITE, BigInt(port & 0xffff), size, value);
  }

  mmioRead(addr: bigint, size: number): number {
    return this.#rpcRead(IO_OP_MMIO_READ, addr, size);
  }

  mmioWrite(addr: bigint, size: number, value: number): void {
    this.#rpcWrite(IO_OP_MMIO_WRITE, addr, size, value);
  }

  #rpcRead(type: number, addr: bigint, size: number): number {
    const id = this.#send(type, addr, size, 0);
    return this.#waitForResponse(id);
  }

  #rpcWrite(type: number, addr: bigint, size: number, value: number): void {
    const id = this.#send(type, addr, size, value);
    void this.#waitForResponse(id);
  }

  #send(type: number, addr: bigint, size: number, value: number): number {
    const id = this.#nextId++;
    const { addrLo, addrHi } = encodeU64(addr);
    writeIoMessage(this.#tx, {
      type,
      id,
      addrLo,
      addrHi,
      size,
      value,
    });

    // Block until there's space (should be fast for 1:1 sync requests).
    const ok = this.#req.pushBlocking(this.#tx);
    if (!ok) throw new Error("pushBlocking unexpectedly timed out");
    return id;
  }

  #waitForResponse(requestId: number): number {
    while (true) {
      const ok = this.#resp.popBlockingInto(this.#rx);
      if (!ok) continue;

      const type = this.#rx[0]!;
      if (type === IO_OP_RESP) {
        const id = this.#rx[1]!;
        if (id !== (requestId >>> 0)) {
          throw new Error(`unexpected response id ${id}, expected ${requestId}`);
        }
        return this.#rx[5]!;
      }

      if (type === IO_OP_IRQ_RAISE || type === IO_OP_IRQ_LOWER) {
        const irq = this.#rx[2]! & 0xff;
        this.#onIrq?.(irq, type === IO_OP_IRQ_RAISE);
        continue;
      }

      if (type === IO_OP_A20_SET) {
        this.#onA20?.(this.#rx[5]! !== 0);
        continue;
      }

      if (type === IO_OP_RESET_REQUEST) {
        this.#onReset?.();
        continue;
      }

      if (type === IO_OP_SERIAL_OUT) {
        const port = this.#rx[2]! & 0xffff;
        const len = this.#rx[4]! & 0xff;
        const value = this.#rx[5]!;
        const bytes = new Uint8Array(len);
        for (let i = 0; i < len; i++) {
          bytes[i] = (value >>> (i * 8)) & 0xff;
        }
        this.#onSerialOutput?.(port, bytes);
        continue;
      }

      // Debug fallback: allow future message types without hanging.
      if (type === IO_OP_PORT_READ || type === IO_OP_PORT_WRITE || type === IO_OP_MMIO_READ || type === IO_OP_MMIO_WRITE) {
        const addr = decodeU64(this.#rx[2]!, this.#rx[3]!);
        throw new Error(`unexpected request op ${type} on response ring (addr=${addr})`);
      }
      throw new Error(`unknown message type ${type} on response ring`);
    }
  }
}

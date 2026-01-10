import { RingBuffer } from "../../ipc/ring_buffer.ts";
import { decodeCommand, decodeEvent, encodeCommand, encodeEvent, type Command, type Event } from "../../ipc/protocol.ts";
import type { IrqSink } from "../device_manager.ts";

export type IrqCallback = (irq: number, level: boolean) => void;
export type A20Callback = (enabled: boolean) => void;
export type ResetCallback = () => void;
export type SerialOutputCallback = (port: number, data: Uint8Array) => void;

export interface AeroIpcIoDispatchTarget {
  portRead(port: number, size: number): number;
  portWrite(port: number, size: number, value: number): void;
  mmioRead(addr: bigint, size: number): number;
  mmioWrite(addr: bigint, size: number, value: number): void;
  tick(nowMs: number): void;
}

export interface AeroIpcIoClientOptions {
  onIrq?: IrqCallback;
  onA20?: A20Callback;
  onReset?: ResetCallback;
  onSerialOutput?: SerialOutputCallback;
}

function valueToLeBytes(value: number, size: number): Uint8Array {
  const out = new Uint8Array(size);
  const v = value >>> 0;
  if (size >= 1) out[0] = v & 0xff;
  if (size >= 2) out[1] = (v >>> 8) & 0xff;
  if (size >= 3) out[2] = (v >>> 16) & 0xff;
  if (size >= 4) out[3] = (v >>> 24) & 0xff;
  return out;
}

function leBytesToU32(bytes: Uint8Array): number {
  const b0 = bytes[0] ?? 0;
  const b1 = bytes[1] ?? 0;
  const b2 = bytes[2] ?? 0;
  const b3 = bytes[3] ?? 0;
  return (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) >>> 0;
}

/**
 * CPU-side synchronous I/O client using the project's variable-length AIPC ring
 * buffer (`web/src/ipc`) and protocol (`web/src/ipc/protocol.ts`).
 *
 * Intended to be used inside a worker thread (CPU worker) where `Atomics.wait`
 * is permitted.
 */
export class AeroIpcIoClient {
  readonly #cmdQ: RingBuffer;
  readonly #evtQ: RingBuffer;
  readonly #onIrq?: IrqCallback;
  readonly #onA20?: A20Callback;
  readonly #onReset?: ResetCallback;
  readonly #onSerialOutput?: SerialOutputCallback;
  #nextId = 1;

  constructor(cmdQ: RingBuffer, evtQ: RingBuffer, opts: AeroIpcIoClientOptions = {}) {
    this.#cmdQ = cmdQ;
    this.#evtQ = evtQ;
    this.#onIrq = opts.onIrq;
    this.#onA20 = opts.onA20;
    this.#onReset = opts.onReset;
    this.#onSerialOutput = opts.onSerialOutput;
  }

  portRead(port: number, size: number): number {
    const id = this.#send({ kind: "portRead", id: this.#allocId(), port: port & 0xffff, size });
    return this.#waitForResponse(id, "portReadResp").value >>> 0;
  }

  portWrite(port: number, size: number, value: number): void {
    const id = this.#send({ kind: "portWrite", id: this.#allocId(), port: port & 0xffff, size, value: value >>> 0 });
    void this.#waitForResponse(id, "portWriteResp");
  }

  mmioRead(addr: bigint, size: number): number {
    const id = this.#send({ kind: "mmioRead", id: this.#allocId(), addr, size });
    const evt = this.#waitForResponse(id, "mmioReadResp");
    return leBytesToU32(evt.data);
  }

  mmioWrite(addr: bigint, size: number, value: number): void {
    const id = this.#send({
      kind: "mmioWrite",
      id: this.#allocId(),
      addr,
      data: valueToLeBytes(value, size),
    });
    void this.#waitForResponse(id, "mmioWriteResp");
  }

  #allocId(): number {
    // IDs are u32 in the wire format; keep them non-zero.
    const id = this.#nextId >>> 0;
    this.#nextId = (this.#nextId + 1) >>> 0;
    return id === 0 ? this.#allocId() : id;
  }

  #send(cmd: Command & { id?: number }): number {
    const encoded = encodeCommand(cmd as Command);
    this.#cmdQ.pushBlocking(encoded);
    return (cmd as { id: number }).id >>> 0;
  }

  #waitForResponse<TKind extends Event["kind"]>(
    requestId: number,
    kind: TKind,
  ): Extract<Event, { kind: TKind }> {
    for (;;) {
      const bytes = this.#evtQ.popBlocking();
      const evt = decodeEvent(bytes);

      if (evt.kind === "irqRaise" || evt.kind === "irqLower") {
        this.#onIrq?.(evt.irq, evt.kind === "irqRaise");
        continue;
      }

      if (evt.kind === "a20Set") {
        this.#onA20?.(evt.enabled);
        continue;
      }

      if (evt.kind === "resetRequest") {
        this.#onReset?.();
        continue;
      }

      if (evt.kind === "serialOutput") {
        this.#onSerialOutput?.(evt.port, evt.data);
        continue;
      }

      if (evt.kind !== kind) {
        // Ignore unrelated events (logs, frames, etc.) while waiting.
        continue;
      }

      const id = (evt as { id?: number }).id;
      if (id !== (requestId >>> 0)) {
        throw new Error(`unexpected ${kind} id ${id}, expected ${requestId}`);
      }
      return evt as Extract<Event, { kind: TKind }>;
    }
  }
}

export interface AeroIpcIoServerOptions {
  tickIntervalMs?: number;
}

/**
 * I/O-side server loop implementing the AIPC Command/Event contract and routing
 * to an I/O `DeviceManager` (or any `AeroIpcIoDispatchTarget`).
 */
export class AeroIpcIoServer implements IrqSink {
  readonly #cmdQ: RingBuffer;
  readonly #evtQ: RingBuffer;
  readonly #target: AeroIpcIoDispatchTarget;
  readonly #tickIntervalMs: number;

  constructor(cmdQ: RingBuffer, evtQ: RingBuffer, target: AeroIpcIoDispatchTarget, opts: AeroIpcIoServerOptions = {}) {
    this.#cmdQ = cmdQ;
    this.#evtQ = evtQ;
    this.#target = target;
    this.#tickIntervalMs = opts.tickIntervalMs ?? 5;
  }

  raiseIrq(irq: number): void {
    this.#evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: irq & 0xff }));
  }

  lowerIrq(irq: number): void {
    this.#evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: irq & 0xff }));
  }

  run(): void {
    let nextTickAt = this.#nowMs() + this.#tickIntervalMs;

    for (;;) {
      // Drain all queued commands.
      while (true) {
        const bytes = this.#cmdQ.tryPop();
        if (!bytes) break;
        const cmd = this.#safeDecodeCommand(bytes);
        if (!cmd) continue;
        if (cmd.kind === "shutdown") return;
        this.#handleCommand(cmd);
      }

      const now = this.#nowMs();
      if (now >= nextTickAt) {
        this.#target.tick(now);
        nextTickAt = now + this.#tickIntervalMs;
        continue;
      }

      const timeout = Math.max(0, nextTickAt - now);
      const res = this.#cmdQ.waitForData(timeout);
      if (res === "timed-out") {
        const tickNow = this.#nowMs();
        this.#target.tick(tickNow);
        nextTickAt = tickNow + this.#tickIntervalMs;
      }
    }
  }

  #handleCommand(cmd: Command): void {
    switch (cmd.kind) {
      case "nop":
        // NOP is often used for benchmarking / wakeups; reply so the sender can
        // measure latency.
        this.#evtQ.pushBlocking(encodeEvent({ kind: "ack", seq: cmd.seq }));
        return;
      case "mmioRead": {
        const value = this.#target.mmioRead(cmd.addr, cmd.size);
        const data = valueToLeBytes(value, cmd.size);
        this.#evtQ.pushBlocking(encodeEvent({ kind: "mmioReadResp", id: cmd.id, data }));
        return;
      }
      case "mmioWrite": {
        const value = leBytesToU32(cmd.data);
        this.#target.mmioWrite(cmd.addr, cmd.data.byteLength, value);
        this.#evtQ.pushBlocking(encodeEvent({ kind: "mmioWriteResp", id: cmd.id }));
        return;
      }
      case "portRead": {
        const value = this.#target.portRead(cmd.port, cmd.size);
        this.#evtQ.pushBlocking(encodeEvent({ kind: "portReadResp", id: cmd.id, value }));
        return;
      }
      case "portWrite": {
        this.#target.portWrite(cmd.port, cmd.size, cmd.value);
        this.#evtQ.pushBlocking(encodeEvent({ kind: "portWriteResp", id: cmd.id }));
        return;
      }
      case "shutdown":
        return;
    }
  }

  #safeDecodeCommand(bytes: Uint8Array): Command | null {
    try {
      // `decodeCommand` throws on unknown tags/trailing bytes. Treat that as a
      // malformed payload and ignore so we don't deadlock the sender forever.
      return decodeCommand(bytes);
    } catch {
      return null;
    }
  }

  #nowMs(): number {
    return typeof performance !== "undefined" ? performance.now() : Date.now();
  }
}

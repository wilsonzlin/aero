import { RingBuffer } from "../../ipc/ring_buffer.ts";
import { decodeCommand, decodeEvent, encodeCommand, encodeEvent, type Command, type Event } from "../../ipc/protocol.ts";
import type { IrqSink } from "../device_manager.ts";
import { formatOneLineError } from "../../text.ts";

/**
 * IRQ callback invoked by {@link AeroIpcIoClient.poll}.
 *
 * `level=true` corresponds to an `irqRaise` event (line asserted) and
 * `level=false` corresponds to an `irqLower` event (line deasserted).
 *
 * These events model IRQ *line levels* (not \"deliver an interrupt now\"). Shared
 * lines should be treated as refcounted wire-OR levels (effective level is high
 * while the refcount is > 0).
 *
 * Edge-triggered interrupts are represented as explicit pulses (0→1→0).
 *
 * See `docs/irq-semantics.md`.
 */
export type IrqCallback = (irq: number, level: boolean) => void;
export type A20Callback = (enabled: boolean) => void;
export type ResetCallback = () => void;
export type SerialOutputCallback = (port: number, data: Uint8Array) => void;

/**
 * AIPC I/O server dispatch interface.
 *
 * This interface belongs to the **inter-worker I/O RPC layer** (AIPC ring buffers) and is not the
 * repo's canonical disk abstraction. Disk images and controller/device models should converge on
 * the canonical traits described in:
 *
 * - `docs/20-storage-trait-consolidation.md`
 * - `docs/19-indexeddb-storage-story.md` (Option C discussion: using a separate worker + sync RPC)
 */
export interface AeroIpcIoDispatchTarget {
  portRead(port: number, size: number): number;
  portWrite(port: number, size: number, value: number): void;
  mmioRead(addr: bigint, size: number): number;
  mmioWrite(addr: bigint, size: number, value: number): void;
  /**
   * Optional disk read handler.
   *
   * If implemented, the server will invoke this for `diskRead` commands and emit
   * a corresponding `diskReadResp` event when the returned value (or promise)
   * resolves.
   *
   * The implementation is responsible for copying bytes into the shared guest
   * memory at `guestOffset` (a guest physical address).
   */
  diskRead?(diskOffset: bigint, len: number, guestOffset: bigint): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult>;
  /**
   * Optional disk write handler.
   *
   * The implementation is responsible for copying bytes out of the shared guest
   * memory at `guestOffset` (a guest physical address).
   */
  diskWrite?(diskOffset: bigint, len: number, guestOffset: bigint): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult>;
  tick(nowMs: number): void;
}

export interface AeroIpcIoDiskResult {
  ok: boolean;
  bytes: number;
  errorCode?: number;
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
  readonly #responsesById = new Map<number, Event>();
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

  diskRead(
    diskOffset: bigint,
    len: number,
    guestOffset: bigint,
    timeoutMs?: number,
  ): Extract<Event, { kind: "diskReadResp" }> {
    const id = this.#send({ kind: "diskRead", id: this.#allocId(), diskOffset, len: len >>> 0, guestOffset });
    return this.#waitForResponse(id, "diskReadResp", timeoutMs);
  }

  diskWrite(
    diskOffset: bigint,
    len: number,
    guestOffset: bigint,
    timeoutMs?: number,
  ): Extract<Event, { kind: "diskWriteResp" }> {
    const id = this.#send({ kind: "diskWrite", id: this.#allocId(), diskOffset, len: len >>> 0, guestOffset });
    return this.#waitForResponse(id, "diskWriteResp", timeoutMs);
  }

  /**
   * Drain pending events from the ioIpc event queue without blocking.
   *
   * This is useful for handling asynchronous device events (IRQs, A20 changes,
   * reset requests) even when the CPU is not currently performing a port/mmio
   * request.
   *
   * Any response events (`*Resp`) encountered are buffered internally so
   * subsequent synchronous calls can still retrieve them by id.
   */
  poll(maxEvents?: number): number {
    let drained = 0;
    while (maxEvents == null || drained < (maxEvents >>> 0)) {
      const bytes = this.#evtQ.tryPop();
      if (!bytes) break;
      let evt: Event;
      try {
        evt = decodeEvent(bytes);
      } catch {
        continue;
      }
      this.#handleIncomingEvent(evt);
      drained++;
    }
    return drained;
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
    timeoutMs?: number,
  ): Extract<Event, { kind: TKind }> {
    const take = (): Extract<Event, { kind: TKind }> | null => {
      const existing = this.#responsesById.get(requestId);
      if (!existing) return null;
      if (existing.kind !== kind) {
        throw new Error(`unexpected response kind ${existing.kind} for id ${requestId}, expected ${kind}`);
      }
      this.#responsesById.delete(requestId);
      return existing as Extract<Event, { kind: TKind }>;
    };

    const cached = take();
    if (cached) return cached;

    const startMs = timeoutMs == null ? 0 : this.#nowMs();
    const deadlineMs = timeoutMs == null ? 0 : startMs + timeoutMs;

    for (;;) {
      const maybeReady = take();
      if (maybeReady) return maybeReady;

      const remaining = timeoutMs == null ? undefined : Math.max(0, deadlineMs - this.#nowMs());
      let bytes: Uint8Array;
      try {
        bytes = this.#evtQ.popBlocking(remaining);
      } catch (err) {
        // Include a bit more context than RingBuffer's generic message.
        const base = formatOneLineError(err, 256);
        throw new Error(`${base} while waiting for ${kind} (id=${requestId})`);
      }
      let evt: Event;
      try {
        evt = decodeEvent(bytes);
      } catch {
        continue;
      }

      this.#handleIncomingEvent(evt);
    }
  }

  #handleIncomingEvent(evt: Event): void {
    if (evt.kind === "irqRaise" || evt.kind === "irqLower") {
      // IRQ events are level transitions: `irqRaise` asserts the line and
      // `irqLower` deasserts it. See `IrqCallback` for the wire-OR contract.
      this.#onIrq?.(evt.irq, evt.kind === "irqRaise");
      return;
    }

    if (evt.kind === "a20Set") {
      this.#onA20?.(evt.enabled);
      return;
    }

    if (evt.kind === "resetRequest") {
      this.#onReset?.();
      return;
    }

    if (evt.kind === "serialOutput") {
      this.#onSerialOutput?.(evt.port, evt.data);
      return;
    }

    switch (evt.kind) {
      case "mmioReadResp":
      case "mmioWriteResp":
      case "portReadResp":
      case "portWriteResp":
      case "diskReadResp":
      case "diskWriteResp":
        this.#responsesById.set(evt.id >>> 0, evt);
        return;
      default:
        return;
    }
  }

  #nowMs(): number {
    return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
  }
}

export interface AeroIpcIoServerOptions {
  tickIntervalMs?: number;
  /**
   * Optional event sink used for server-emitted AIPC events.
   *
   * When omitted, events are pushed using `evtQ.pushBlocking`, which can stall
   * the worker if the ring is full. Browser workers that must remain responsive
   * to `postMessage` handlers should provide a non-blocking sink (e.g. tryPush
   * + in-memory queue flushed from a timer/tick).
   */
  emitEvent?: (bytes: Uint8Array) => void;
}

export interface AeroIpcIoServerRunAsyncOptions {
  /**
   * Optional abort signal used to stop the server loop without requiring a
   * `shutdown` command.
   */
  signal?: AbortSignal;
  /**
   * Optional fairness knob: after draining this many commands, yield back to the
   * event loop so `postMessage` handlers (e.g. input batches) can run.
   */
  yieldEveryNCommands?: number;
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
  readonly #emitEvent: (bytes: Uint8Array) => void;

  constructor(cmdQ: RingBuffer, evtQ: RingBuffer, target: AeroIpcIoDispatchTarget, opts: AeroIpcIoServerOptions = {}) {
    this.#cmdQ = cmdQ;
    this.#evtQ = evtQ;
    this.#target = target;
    this.#tickIntervalMs = opts.tickIntervalMs ?? 5;
    this.#emitEvent = opts.emitEvent ?? ((bytes) => this.#evtQ.pushBlocking(bytes));
  }

  raiseIrq(irq: number): void {
    // Emit a level assertion event. Receivers are responsible for wire-OR /
    // refcount behaviour when multiple devices share a line. Edge-triggered
    // interrupts are represented as explicit pulses (raise then lower).
    // See `docs/irq-semantics.md`.
    this.#emitEvent(encodeEvent({ kind: "irqRaise", irq: irq & 0xff }));
  }

  lowerIrq(irq: number): void {
    // Emit a level deassertion event. See `raiseIrq()`.
    this.#emitEvent(encodeEvent({ kind: "irqLower", irq: irq & 0xff }));
  }

  run(): void {
    let nextTickAt = this.#nowMs() + this.#tickIntervalMs;

    for (;;) {
      // Drain all queued commands.
      while (true) {
        const bytes = this.#cmdQ.tryPop();
        if (!bytes) break;
        const cmd = this.#safeDecodeCommand(bytes);
        if (cmd) {
          if (cmd.kind === "shutdown") return;
          this.#handleCommand(cmd);
        }

        const now = this.#nowMs();
        if (now >= nextTickAt) {
          this.#target.tick(now);
          nextTickAt = now + this.#tickIntervalMs;
        }
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

  /**
   * Async/non-blocking variant of `run()`.
   *
   * This is intended for browser workers that must stay responsive to
   * `postMessage` events (e.g. input batches) and therefore cannot park the
   * entire thread in `Atomics.wait()`.
   */
  async runAsync(opts: AeroIpcIoServerRunAsyncOptions = {}): Promise<void> {
    let nextTickAt = this.#nowMs() + this.#tickIntervalMs;

    for (;;) {
      if (opts.signal?.aborted) return;

      // Drain all queued commands.
      let drained = 0;
      while (true) {
        const bytes = this.#cmdQ.tryPop();
        if (!bytes) break;
        const cmd = this.#safeDecodeCommand(bytes);
        if (cmd) {
          if (cmd.kind === "shutdown") return;
          this.#handleCommand(cmd);
        }

        const now = this.#nowMs();
        if (now >= nextTickAt) {
          this.#target.tick(now);
          nextTickAt = now + this.#tickIntervalMs;
        }

        drained++;
        if (opts.yieldEveryNCommands && drained >= opts.yieldEveryNCommands) {
          drained = 0;
          // Yield to allow other tasks (e.g. worker `onmessage`) to run.
          await new Promise((resolve) => {
            const timer = setTimeout(resolve, 0);
            (timer as unknown as { unref?: () => void }).unref?.();
          });
          if (opts.signal?.aborted) return;
        }
      }

      const now = this.#nowMs();
      if (now >= nextTickAt) {
        this.#target.tick(now);
        nextTickAt = now + this.#tickIntervalMs;
        continue;
      }

      const timeout = Math.max(0, nextTickAt - now);
      const res = await this.#cmdQ.waitForDataAsync(timeout);
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
        this.#emitEvent(encodeEvent({ kind: "ack", seq: cmd.seq }));
        return;
      case "mmioRead": {
        const value = this.#target.mmioRead(cmd.addr, cmd.size);
        const data = valueToLeBytes(value, cmd.size);
        this.#emitEvent(encodeEvent({ kind: "mmioReadResp", id: cmd.id, data }));
        return;
      }
      case "mmioWrite": {
        const value = leBytesToU32(cmd.data);
        this.#target.mmioWrite(cmd.addr, cmd.data.byteLength, value);
        this.#emitEvent(encodeEvent({ kind: "mmioWriteResp", id: cmd.id }));
        return;
      }
      case "portRead": {
        const value = this.#target.portRead(cmd.port, cmd.size);
        this.#emitEvent(encodeEvent({ kind: "portReadResp", id: cmd.id, value }));
        return;
      }
      case "portWrite": {
        this.#target.portWrite(cmd.port, cmd.size, cmd.value);
        this.#emitEvent(encodeEvent({ kind: "portWriteResp", id: cmd.id }));
        return;
      }
      case "diskRead": {
        const handler = this.#target.diskRead;
        if (typeof handler !== "function") {
          // No disk backend; reply with a generic failure so the client does not deadlock.
          this.#emitEvent(encodeEvent({ kind: "diskReadResp", id: cmd.id, ok: false, bytes: 0, errorCode: 0 }));
          return;
        }

        const emit = (res: AeroIpcIoDiskResult | null | undefined): void => {
          const ok = Boolean(res?.ok);
          const bytes = typeof res?.bytes === "number" && Number.isFinite(res.bytes) ? res.bytes >>> 0 : 0;
          const errorCode =
            typeof res?.errorCode === "number" && Number.isFinite(res.errorCode) ? (res.errorCode >>> 0) : undefined;
          this.#emitEvent(encodeEvent({ kind: "diskReadResp", id: cmd.id, ok, bytes, errorCode }));
        };

        try {
          const result = handler.call(this.#target, cmd.diskOffset, cmd.len, cmd.guestOffset);
          const maybeThenable = result as unknown as { then?: unknown };
          if (maybeThenable && typeof maybeThenable.then === "function") {
            void (result as Promise<AeroIpcIoDiskResult>).then(emit, () => emit({ ok: false, bytes: 0, errorCode: 0 }));
          } else {
            emit(result as AeroIpcIoDiskResult);
          }
        } catch {
          emit({ ok: false, bytes: 0, errorCode: 0 });
        }
        return;
      }
      case "diskWrite": {
        const handler = this.#target.diskWrite;
        if (typeof handler !== "function") {
          this.#emitEvent(encodeEvent({ kind: "diskWriteResp", id: cmd.id, ok: false, bytes: 0, errorCode: 0 }));
          return;
        }

        const emit = (res: AeroIpcIoDiskResult | null | undefined): void => {
          const ok = Boolean(res?.ok);
          const bytes = typeof res?.bytes === "number" && Number.isFinite(res.bytes) ? res.bytes >>> 0 : 0;
          const errorCode =
            typeof res?.errorCode === "number" && Number.isFinite(res.errorCode) ? (res.errorCode >>> 0) : undefined;
          this.#emitEvent(encodeEvent({ kind: "diskWriteResp", id: cmd.id, ok, bytes, errorCode }));
        };

        try {
          const result = handler.call(this.#target, cmd.diskOffset, cmd.len, cmd.guestOffset);
          const maybeThenable = result as unknown as { then?: unknown };
          if (maybeThenable && typeof maybeThenable.then === "function") {
            void (result as Promise<AeroIpcIoDiskResult>).then(emit, () => emit({ ok: false, bytes: 0, errorCode: 0 }));
          } else {
            emit(result as AeroIpcIoDiskResult);
          }
        } catch {
          emit({ ok: false, bytes: 0, errorCode: 0 });
        }
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

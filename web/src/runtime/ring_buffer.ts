import { notify, waitUntilNotEqual } from "./atomics_wait.ts";

/**
 * Lock-free single-producer/single-consumer (SPSC) ring buffer implemented over a
 * SharedArrayBuffer using Atomics for cross-thread visibility.
 *
 * Message framing
 * ---------------
 * This ring buffer stores discrete messages (byte payloads) with a u32 little-endian
 * length prefix:
 *
 *   [len: u32 LE][payload bytes...]
 *
 * `len` is the payload byte length. Zero-length payloads are treated as invalid
 * (reserved for corruption detection).
 *
 * Concurrency model
 * -----------------
 * The ring is designed for one writer thread and one reader thread operating
 * concurrently (e.g. Coordinator -> Worker command queue, Worker -> Coordinator
 * event queue). Head/tail indices are stored in an Int32Array and updated with
 * Atomics to ensure ordering/visibility across workers.
 */

export type AtomicsWaitResult = "ok" | "not-equal" | "timed-out";

export interface RingBufferRegion {
  sab: SharedArrayBuffer;
  byteOffset: number;
  byteLength: number;
}

const META_INTS = 2;
const META_BYTES = META_INTS * 4;

const HEAD_INDEX = 0;
const TAIL_INDEX = 1;

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

export class RingBuffer {
  static readonly META_BYTES = META_BYTES;

  readonly meta: Int32Array;
  readonly data: Uint8Array;
  readonly capacityBytes: number;

  constructor(sab: SharedArrayBuffer, byteOffset: number, byteLength: number) {
    if ((byteOffset & 3) !== 0) {
      throw new Error(`RingBuffer byteOffset must be 4-byte aligned (got ${byteOffset})`);
    }
    if (byteLength <= META_BYTES) {
      throw new Error(`RingBuffer byteLength must be > ${META_BYTES} (got ${byteLength})`);
    }

    this.meta = new Int32Array(sab, byteOffset, META_INTS);
    const dataOffset = byteOffset + META_BYTES;
    const dataLength = byteLength - META_BYTES;
    if (dataLength < 16) {
      throw new Error(`RingBuffer capacity too small (got ${dataLength} bytes)`);
    }
    this.data = new Uint8Array(sab, dataOffset, dataLength);
    this.capacityBytes = dataLength;
  }

  static byteLengthForCapacity(capacityBytes: number): number {
    if (capacityBytes < 16) {
      throw new Error("RingBuffer capacityBytes must be >= 16");
    }
    return META_BYTES + capacityBytes;
  }

  reset(): void {
    Atomics.store(this.meta, HEAD_INDEX, 0);
    Atomics.store(this.meta, TAIL_INDEX, 0);
  }

  /**
   * Maximum payload size allowed by this ring.
   *
   * We intentionally keep one byte unused so that head==tail means "empty" and we
   * can detect "full" without an extra flag.
   */
  maxMessageBytes(): number {
    return Math.max(0, this.capacityBytes - 5);
  }

  usedBytes(): number {
    const head = Atomics.load(this.meta, HEAD_INDEX);
    const tail = Atomics.load(this.meta, TAIL_INDEX);
    return this.usedBytesFor(head, tail);
  }

  freeBytes(): number {
    const head = Atomics.load(this.meta, HEAD_INDEX);
    const tail = Atomics.load(this.meta, TAIL_INDEX);
    return this.freeBytesFor(head, tail);
  }

  push(payload: Uint8Array): boolean {
    if (payload.byteLength === 0) return false;
    if (payload.byteLength > this.maxMessageBytes()) return false;

    const head = Atomics.load(this.meta, HEAD_INDEX);
    const tail = Atomics.load(this.meta, TAIL_INDEX);

    const totalBytes = 4 + payload.byteLength;
    if (this.freeBytesFor(head, tail) < totalBytes) return false;

    this.writeU32LE(head, payload.byteLength);
    this.writeBytes(this.advance(head, 4), payload);

    Atomics.store(this.meta, HEAD_INDEX, this.advance(head, totalBytes));
    notify(this.meta, HEAD_INDEX, 1);
    return true;
  }

  pop(): Uint8Array | null {
    const head = Atomics.load(this.meta, HEAD_INDEX);
    const tail = Atomics.load(this.meta, TAIL_INDEX);

    const used = this.usedBytesFor(head, tail);
    if (used < 4) return null;

    const len = this.readU32LE(tail);
    if (len === 0 || len > this.maxMessageBytes()) {
      // Corruption or writer/reader disagreement. Drop everything to avoid
      // getting stuck in a bad state.
      Atomics.store(this.meta, TAIL_INDEX, head);
      return null;
    }

    const totalBytes = 4 + len;
    if (used < totalBytes) {
      // Should never happen for a correct single-producer implementation (writer
      // updates head after fully writing the message). Treat as corruption and
      // drop everything so the consumer doesn't get stuck forever.
      Atomics.store(this.meta, TAIL_INDEX, head);
      return null;
    }

    const payloadStart = this.advance(tail, 4);
    const payload = new Uint8Array(len);
    this.readBytes(payloadStart, payload);

    Atomics.store(this.meta, TAIL_INDEX, this.advance(tail, totalBytes));
    return payload;
  }

  /**
   * Wait until new data is available.
   *
   * - In Workers: uses blocking `Atomics.wait()` for efficiency.
   * - On the browser main thread: uses `Atomics.waitAsync()` when available, or
   *   a polling fallback, so the UI thread never blocks.
   */
  async waitForData(timeoutMs?: number): Promise<AtomicsWaitResult> {
    const start = timeoutMs === undefined ? 0 : nowMs();

    // Important: never wait if the ring is already non-empty. Otherwise we can
    // miss messages that arrive between "drain" and "wait", leading to deadlocks
    // (e.g. waiting for head to change when head already includes unread data).
    while (true) {
      const head = Atomics.load(this.meta, HEAD_INDEX);
      const tail = Atomics.load(this.meta, TAIL_INDEX);
      if (head !== tail) return "not-equal";

      const remaining = timeoutMs === undefined ? undefined : Math.max(0, timeoutMs - (nowMs() - start));
      const result = await waitUntilNotEqual(this.meta, HEAD_INDEX, head, { timeoutMs: remaining });
      if (result === "timed-out") return "timed-out";
      // Head changed between load and wait; loop and re-check emptiness.
    }
  }

  /**
   * Async-friendly version of `waitForData()`, intended for the main thread
   * where `Atomics.wait` is not permitted.
   */
  async waitForDataAsync(timeoutMs?: number): Promise<AtomicsWaitResult> {
    // `waitForData()` already routes through `waitUntilNotEqual`, which is
    // environment-aware (workers can block; Window thread uses waitAsync/polling).
    return this.waitForData(timeoutMs);
  }

  notifyData(count = 1): number {
    return notify(this.meta, HEAD_INDEX, count);
  }

  private advance(pos: number, delta: number): number {
    const next = pos + delta;
    return next >= this.capacityBytes ? next - this.capacityBytes : next;
  }

  private usedBytesFor(head: number, tail: number): number {
    if (head >= tail) return head - tail;
    return this.capacityBytes - (tail - head);
  }

  private freeBytesFor(head: number, tail: number): number {
    // Keep one byte open so full/empty are distinguishable.
    return this.capacityBytes - this.usedBytesFor(head, tail) - 1;
  }

  private writeU32LE(pos: number, value: number): void {
    const cap = this.capacityBytes;
    this.data[pos] = value & 0xff;
    this.data[(pos + 1) % cap] = (value >>> 8) & 0xff;
    this.data[(pos + 2) % cap] = (value >>> 16) & 0xff;
    this.data[(pos + 3) % cap] = (value >>> 24) & 0xff;
  }

  private readU32LE(pos: number): number {
    const cap = this.capacityBytes;
    return (
      this.data[pos] |
      (this.data[(pos + 1) % cap] << 8) |
      (this.data[(pos + 2) % cap] << 16) |
      (this.data[(pos + 3) % cap] << 24)
    ) >>> 0;
  }

  private writeBytes(pos: number, bytes: Uint8Array): void {
    const cap = this.capacityBytes;
    const first = Math.min(bytes.byteLength, cap - pos);
    this.data.set(bytes.subarray(0, first), pos);
    if (first < bytes.byteLength) {
      this.data.set(bytes.subarray(first), 0);
    }
  }

  private readBytes(pos: number, out: Uint8Array): void {
    const cap = this.capacityBytes;
    const first = Math.min(out.byteLength, cap - pos);
    out.set(this.data.subarray(pos, pos + first), 0);
    if (first < out.byteLength) {
      out.set(this.data.subarray(0, out.byteLength - first), first);
    }
  }
}

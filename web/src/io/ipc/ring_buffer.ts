export type AtomicsWaitResult = "ok" | "not-equal" | "timed-out";

const HEADER_I32_COUNT = 4;
const HEADER_HEAD_I32 = 0;
const HEADER_TAIL_I32 = 1;
const HEADER_CAPACITY_I32 = 2;
const HEADER_STRIDE_I32 = 3;

// When callers request an "infinite" blocking push/pop, do not use an unbounded Atomics.wait().
// A finite slice prevents hangs if a notify is missed and makes shutdown/termination more reliable
// across runtimes (Node/browsers) and versions.
const DEFAULT_BLOCKING_WAIT_SLICE_MS = 1000;

function canBlockAtomicsWait(): boolean {
  // Browser main thread disallows Atomics.wait; workers and Node allow it.
  // There's no perfect feature test; this is conservative and matches the
  // approach used by the other shared-memory IPC utilities.
  return typeof Atomics.wait === "function" && typeof document === "undefined";
}

function assertCanBlockAtomicsWait(): void {
  if (canBlockAtomicsWait()) return;
  throw new Error(
    "Blocking Atomics.wait() is not allowed in this context (likely the browser main thread). " +
      "Use non-blocking ring operations (push/popInto) and poll, or add an async wait based on Atomics.waitAsync().",
  );
}

export interface SharedRingBufferCreateOptions {
  capacity: number;
  stride: number;
}

/**
 * A simple single-producer single-consumer (SPSC) ring buffer for fixed-size
 * messages backed by a SharedArrayBuffer.
 *
 * Layout (little-endian):
 *   Int32[0] head (write index)
 *   Int32[1] tail (read index)
 *   Int32[2] capacity (slots)
 *   Int32[3] stride (u32s per slot)
 *   Uint32[] data (capacity * stride)
 *
 * Wait/notify strategy:
 * - Consumer waits on `head` when empty.
 * - Producer waits on `tail` when full.
 */
export class SharedRingBuffer {
  readonly sab: SharedArrayBuffer;
  readonly header: Int32Array;
  readonly data: Uint32Array;
  readonly capacity: number;
  readonly stride: number;

  constructor(sab: SharedArrayBuffer, byteOffset = 0) {
    this.sab = sab;
    this.header = new Int32Array(sab, byteOffset, HEADER_I32_COUNT);
    this.capacity = this.header[HEADER_CAPACITY_I32];
    this.stride = this.header[HEADER_STRIDE_I32];
    if (this.capacity <= 1) {
      throw new Error(`SharedRingBuffer capacity must be > 1, got ${this.capacity}`);
    }
    if (this.stride <= 0) {
      throw new Error(`SharedRingBuffer stride must be > 0, got ${this.stride}`);
    }
    const dataByteOffset = byteOffset + HEADER_I32_COUNT * 4;
    this.data = new Uint32Array(sab, dataByteOffset, this.capacity * this.stride);
  }

  static byteLength(capacity: number, stride: number): number {
    if (capacity <= 1) throw new Error(`capacity must be > 1, got ${capacity}`);
    if (stride <= 0) throw new Error(`stride must be > 0, got ${stride}`);
    return HEADER_I32_COUNT * 4 + capacity * stride * 4;
  }

  static create({ capacity, stride }: SharedRingBufferCreateOptions): SharedRingBuffer {
    const sab = new SharedArrayBuffer(SharedRingBuffer.byteLength(capacity, stride));
    const header = new Int32Array(sab, 0, HEADER_I32_COUNT);
    header[HEADER_HEAD_I32] = 0;
    header[HEADER_TAIL_I32] = 0;
    header[HEADER_CAPACITY_I32] = capacity;
    header[HEADER_STRIDE_I32] = stride;
    return new SharedRingBuffer(sab);
  }

  static from(sab: SharedArrayBuffer, byteOffset = 0): SharedRingBuffer {
    return new SharedRingBuffer(sab, byteOffset);
  }

  /**
   * Non-blocking push. Returns false if the ring is full.
   */
  push(slot: ArrayLike<number>): boolean {
    if (slot.length !== this.stride) {
      throw new Error(`push slot length ${slot.length} != stride ${this.stride}`);
    }

    const head = Atomics.load(this.header, HEADER_HEAD_I32);
    const tail = Atomics.load(this.header, HEADER_TAIL_I32);
    const nextHead = head + 1 === this.capacity ? 0 : head + 1;
    if (nextHead === tail) return false;

    const base = head * this.stride;
    for (let i = 0; i < this.stride; i++) {
      // Ensure uint32 encoding (Atomics-safe element size).
      this.data[base + i] = slot[i]! >>> 0;
    }

    Atomics.store(this.header, HEADER_HEAD_I32, nextHead);
    Atomics.notify(this.header, HEADER_HEAD_I32, 1);
    return true;
  }

  /**
   * Blocking push. Waits until space is available (or timeout) and pushes.
   * Returns true if pushed, false if timed out.
   */
  pushBlocking(slot: ArrayLike<number>, timeoutMs?: number): boolean {
    assertCanBlockAtomicsWait();
    if (slot.length !== this.stride) {
      throw new Error(`push slot length ${slot.length} != stride ${this.stride}`);
    }
    const deadline =
      timeoutMs === undefined ? undefined : (typeof performance !== "undefined" ? performance.now() : Date.now()) + timeoutMs;

    while (true) {
      const head = Atomics.load(this.header, HEADER_HEAD_I32);
      const tail = Atomics.load(this.header, HEADER_TAIL_I32);
      const nextHead = head + 1 === this.capacity ? 0 : head + 1;
      if (nextHead !== tail) {
        const base = head * this.stride;
        for (let i = 0; i < this.stride; i++) {
          this.data[base + i] = slot[i]! >>> 0;
        }
        Atomics.store(this.header, HEADER_HEAD_I32, nextHead);
        Atomics.notify(this.header, HEADER_HEAD_I32, 1);
        return true;
      }

      const now = typeof performance !== "undefined" ? performance.now() : Date.now();
      if (deadline !== undefined && now >= deadline) return false;

      const remaining = deadline === undefined ? DEFAULT_BLOCKING_WAIT_SLICE_MS : Math.max(0, deadline - now);
      Atomics.wait(this.header, HEADER_TAIL_I32, tail, remaining);
    }
  }

  /**
   * Non-blocking pop. Writes the slot into `out`. Returns false if empty.
   */
  popInto(out: Uint32Array): boolean {
    if (out.length !== this.stride) {
      throw new Error(`popInto out length ${out.length} != stride ${this.stride}`);
    }

    const tail = Atomics.load(this.header, HEADER_TAIL_I32);
    const head = Atomics.load(this.header, HEADER_HEAD_I32);
    if (tail === head) return false;

    const base = tail * this.stride;
    for (let i = 0; i < this.stride; i++) {
      out[i] = this.data[base + i]!;
    }

    const nextTail = tail + 1 === this.capacity ? 0 : tail + 1;
    Atomics.store(this.header, HEADER_TAIL_I32, nextTail);
    Atomics.notify(this.header, HEADER_TAIL_I32, 1);
    return true;
  }

  /**
   * Blocking pop. Waits until data is available (or timeout). Returns true if a
   * slot was read, false if timed out.
   */
  popBlockingInto(out: Uint32Array, timeoutMs?: number): boolean {
    assertCanBlockAtomicsWait();
    if (out.length !== this.stride) {
      throw new Error(`popBlockingInto out length ${out.length} != stride ${this.stride}`);
    }

    const deadline =
      timeoutMs === undefined ? undefined : (typeof performance !== "undefined" ? performance.now() : Date.now()) + timeoutMs;

    while (true) {
      const tail = Atomics.load(this.header, HEADER_TAIL_I32);
      const head = Atomics.load(this.header, HEADER_HEAD_I32);
      if (tail !== head) {
        const base = tail * this.stride;
        for (let i = 0; i < this.stride; i++) {
          out[i] = this.data[base + i]!;
        }
        const nextTail = tail + 1 === this.capacity ? 0 : tail + 1;
        Atomics.store(this.header, HEADER_TAIL_I32, nextTail);
        Atomics.notify(this.header, HEADER_TAIL_I32, 1);
        return true;
      }

      const now = typeof performance !== "undefined" ? performance.now() : Date.now();
      if (deadline !== undefined && now >= deadline) return false;
      const remaining = deadline === undefined ? DEFAULT_BLOCKING_WAIT_SLICE_MS : Math.max(0, deadline - now);
      Atomics.wait(this.header, HEADER_HEAD_I32, head, remaining);
    }
  }

  isEmpty(): boolean {
    return Atomics.load(this.header, HEADER_HEAD_I32) === Atomics.load(this.header, HEADER_TAIL_I32);
  }
}

// SharedArrayBuffer-backed SPSC (single-producer, single-consumer) ring buffer.
//
// Design goals:
// - Works in both Window and Worker contexts.
// - Fixed-size records (binary) stored in a circular buffer.
// - Atomic head/tail counters (monotonic u32 counters, not modulo indices).
// - Explicit overflow strategy with dropped record accounting.
//
// NOTE: The head/tail counters are stored as signed i32 for Atomics compatibility
// (Atomics operates on Int32Array). We treat them as unsigned u32 via `>>> 0`.

export const RING_BUFFER_MAGIC = 0x46525041; // 'APRF' little-endian
export const RING_BUFFER_VERSION = 1;

export const OverflowStrategy = Object.freeze({
  DropNewest: 0,
  // DropOldest intentionally not implemented: it would require producer writes to
  // the consumer-owned head counter or per-slot sequence numbers.
});

const HEADER_MAGIC = 0;
const HEADER_VERSION = 1;
const HEADER_RECORD_SIZE = 2;
const HEADER_CAPACITY = 3;
const HEADER_OVERFLOW_STRATEGY = 4;
const HEADER_HEAD = 5;
const HEADER_TAIL = 6;
const HEADER_DROPPED = 7;

export const RING_BUFFER_HEADER_I32 = 8;
export const RING_BUFFER_HEADER_BYTES = RING_BUFFER_HEADER_I32 * 4;

export function createSpscRingBufferSharedArrayBuffer({
  capacity,
  recordSize,
  overflowStrategy = OverflowStrategy.DropNewest,
}) {
  if (!Number.isInteger(capacity) || capacity <= 0) {
    throw new Error(`capacity must be a positive integer (got ${capacity})`);
  }
  if (!Number.isInteger(recordSize) || recordSize <= 0) {
    throw new Error(`recordSize must be a positive integer (got ${recordSize})`);
  }
  if (overflowStrategy !== OverflowStrategy.DropNewest) {
    throw new Error(`unsupported overflowStrategy ${overflowStrategy}`);
  }

  const sab = new SharedArrayBuffer(RING_BUFFER_HEADER_BYTES + capacity * recordSize);
  const header = new Int32Array(sab, 0, RING_BUFFER_HEADER_I32);
  header[HEADER_MAGIC] = RING_BUFFER_MAGIC | 0;
  header[HEADER_VERSION] = RING_BUFFER_VERSION | 0;
  header[HEADER_RECORD_SIZE] = recordSize | 0;
  header[HEADER_CAPACITY] = capacity | 0;
  header[HEADER_OVERFLOW_STRATEGY] = overflowStrategy | 0;
  Atomics.store(header, HEADER_HEAD, 0);
  Atomics.store(header, HEADER_TAIL, 0);
  Atomics.store(header, HEADER_DROPPED, 0);
  return sab;
}

export class SpscRingBuffer {
  constructor(sharedArrayBuffer, { expectedRecordSize } = {}) {
    if (!(sharedArrayBuffer instanceof SharedArrayBuffer)) {
      throw new Error(`sharedArrayBuffer must be a SharedArrayBuffer`);
    }
    this.sharedArrayBuffer = sharedArrayBuffer;
    this.header = new Int32Array(sharedArrayBuffer, 0, RING_BUFFER_HEADER_I32);
    this.view = new DataView(sharedArrayBuffer);

    const magic = this.header[HEADER_MAGIC] >>> 0;
    if (magic !== RING_BUFFER_MAGIC) {
      throw new Error(
        `ring buffer magic mismatch: expected 0x${RING_BUFFER_MAGIC.toString(16)}, got 0x${magic.toString(16)}`,
      );
    }
    const version = this.header[HEADER_VERSION] >>> 0;
    if (version !== RING_BUFFER_VERSION) {
      throw new Error(`ring buffer version mismatch: expected ${RING_BUFFER_VERSION}, got ${version}`);
    }

    this.recordSize = this.header[HEADER_RECORD_SIZE] >>> 0;
    this.capacity = this.header[HEADER_CAPACITY] >>> 0;
    this.overflowStrategy = this.header[HEADER_OVERFLOW_STRATEGY] >>> 0;

    if (expectedRecordSize != null && this.recordSize !== expectedRecordSize) {
      throw new Error(`record size mismatch: expected ${expectedRecordSize}, got ${this.recordSize}`);
    }
    if (this.overflowStrategy !== OverflowStrategy.DropNewest) {
      throw new Error(`unsupported overflowStrategy ${this.overflowStrategy}`);
    }

    this.recordsByteOffset = RING_BUFFER_HEADER_BYTES;
  }

  getDroppedCount() {
    return Atomics.load(this.header, HEADER_DROPPED) >>> 0;
  }

  getCapacity() {
    return this.capacity;
  }

  getRecordSize() {
    return this.recordSize;
  }

  reset() {
    Atomics.store(this.header, HEADER_HEAD, 0);
    Atomics.store(this.header, HEADER_TAIL, 0);
    Atomics.store(this.header, HEADER_DROPPED, 0);
  }

  /**
   * Returns the approximate number of available records.
   * (May be stale by the time you act on it.)
   */
  availableRead() {
    const head = Atomics.load(this.header, HEADER_HEAD) >>> 0;
    const tail = Atomics.load(this.header, HEADER_TAIL) >>> 0;
    return (tail - head) >>> 0;
  }

  /**
   * @param {(view: DataView, byteOffset: number) => void} encodeFn
   * @returns {boolean} true if written, false if dropped due to overflow.
   */
  tryWriteRecord(encodeFn) {
    const head = Atomics.load(this.header, HEADER_HEAD) >>> 0;
    const tail = Atomics.load(this.header, HEADER_TAIL) >>> 0;
    const used = (tail - head) >>> 0;

    if (used >= this.capacity) {
      Atomics.add(this.header, HEADER_DROPPED, 1);
      return false;
    }

    const slot = tail % this.capacity;
    const byteOffset = this.recordsByteOffset + slot * this.recordSize;
    encodeFn(this.view, byteOffset);

    // Publish record after all non-atomic writes are complete.
    Atomics.store(this.header, HEADER_TAIL, (tail + 1) | 0);
    return true;
  }

  /**
   * @template T
   * @param {(view: DataView, byteOffset: number) => T} decodeFn
   * @returns {T | null}
   */
  tryReadRecord(decodeFn) {
    const head = Atomics.load(this.header, HEADER_HEAD) >>> 0;
    const tail = Atomics.load(this.header, HEADER_TAIL) >>> 0;
    const available = (tail - head) >>> 0;
    if (available === 0) {
      return null;
    }

    const slot = head % this.capacity;
    const byteOffset = this.recordsByteOffset + slot * this.recordSize;
    const out = decodeFn(this.view, byteOffset);
    Atomics.store(this.header, HEADER_HEAD, (head + 1) | 0);
    return out;
  }

  /**
   * Drain up to `maxRecords` records.
   * @param {number} maxRecords
   * @param {(view: DataView, byteOffset: number) => void} onRecord
   * @returns {number} records drained
   */
  drain(maxRecords, onRecord) {
    let drained = 0;
    while (drained < maxRecords) {
      const ok = this.tryReadRecord((view, byteOffset) => {
        onRecord(view, byteOffset);
      });
      if (ok === null) {
        break;
      }
      drained++;
    }
    return drained;
  }
}


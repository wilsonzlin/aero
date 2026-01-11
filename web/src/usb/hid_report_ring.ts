// SharedArrayBuffer-backed SPSC ring buffer for variable-length HID reports.
//
// Layout:
//   ctrl: Int32Array[3] (head, tail, dropped)
//   data: Uint8Array[dataCapacityBytes]
//
// `head`/`tail` are monotonic u32 byte counters (wrapping at 2^32). The actual
// byte index into `data` is `counter % dataCapacityBytes`.
//
// Records are stored as:
//   deviceId:    u32 (LE)
//   reportType:  u8
//   reportId:    u8
//   len:         u16 (LE)
//   payload:     u8[len]
//   padding:     to 4-byte alignment
//
// When a record would straddle the end of the buffer, the producer writes a
// wrap marker (reportType=0xff, len=0) and pads to the end; the consumer skips
// to the next wrap boundary.

export const HID_REPORT_RING_CTRL_WORDS = 3;
export const HID_REPORT_RING_CTRL_BYTES = HID_REPORT_RING_CTRL_WORDS * 4;

export const HID_REPORT_RECORD_HEADER_BYTES = 8;
export const HID_REPORT_RECORD_ALIGN = 4;

const enum CtrlIndex {
  Head = 0,
  Tail = 1,
  Dropped = 2,
}

export const enum HidReportType {
  Input = 0,
  Output = 1,
  Feature = 2,
  WrapMarker = 0xff,
}

export type HidReportRingRecord = {
  deviceId: number;
  reportType: HidReportType;
  reportId: number;
  payload: Uint8Array;
};

function u32(n: number): number {
  return n >>> 0;
}

function alignUp(value: number, align: number): number {
  if ((align & (align - 1)) !== 0) throw new Error("align must be power of two");
  return (value + (align - 1)) & ~(align - 1);
}

export function createHidReportRingBuffer(dataCapacityBytes: number): SharedArrayBuffer {
  const cap = dataCapacityBytes >>> 0;
  if (cap === 0) throw new Error("dataCapacityBytes must be > 0");
  return new SharedArrayBuffer(HID_REPORT_RING_CTRL_BYTES + cap);
}

export class HidReportRing {
  readonly #ctrl: Int32Array;
  readonly #data: Uint8Array;
  readonly #view: DataView;
  readonly #cap: number;

  constructor(buffer: SharedArrayBuffer) {
    this.#ctrl = new Int32Array(buffer, 0, HID_REPORT_RING_CTRL_WORDS);
    this.#data = new Uint8Array(buffer, HID_REPORT_RING_CTRL_BYTES);
    this.#cap = this.#data.byteLength >>> 0;
    this.#view = new DataView(this.#data.buffer, this.#data.byteOffset, this.#data.byteLength);
    if (this.#cap === 0) {
      throw new Error("HID report ring buffer is too small (missing data region).");
    }
  }

  buffer(): SharedArrayBuffer {
    return this.#ctrl.buffer as SharedArrayBuffer;
  }

  dataCapacityBytes(): number {
    return this.#cap;
  }

  dropped(): number {
    return u32(Atomics.load(this.#ctrl, CtrlIndex.Dropped));
  }

  push(deviceId: number, reportType: HidReportType, reportId: number, payload: Uint8Array): boolean {
    const payloadLen = payload.byteLength >>> 0;
    const recordSize = alignUp(HID_REPORT_RECORD_HEADER_BYTES + payloadLen, HID_REPORT_RECORD_ALIGN);
    if (recordSize > this.#cap) {
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return false;
    }

    const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
    const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
    const used = u32(tail - head);
    if (used > this.#cap) {
      // Corruption or raced with a manual reset; treat as full.
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return false;
    }
    const free = this.#cap - used;

    const tailIndex = tail % this.#cap;
    const remaining = this.#cap - tailIndex;
    const needsWrap = remaining >= HID_REPORT_RECORD_HEADER_BYTES && remaining < recordSize;
    const padding = remaining < recordSize ? remaining : 0;
    const reserve = padding + recordSize;
    if (reserve > free) {
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return false;
    }

    if (needsWrap) {
      // Write the wrap marker at the current tail position; the consumer will
      // advance to the next wrap boundary (`tail + remaining`).
      this.#view.setUint32(tailIndex + 0, 0, true);
      this.#view.setUint8(tailIndex + 4, HidReportType.WrapMarker);
      this.#view.setUint8(tailIndex + 5, 0);
      this.#view.setUint16(tailIndex + 6, 0, true);
    }

    const start = u32(tail + padding);
    const startIndex = start % this.#cap;
    this.#view.setUint32(startIndex + 0, u32(deviceId), true);
    this.#view.setUint8(startIndex + 4, reportType & 0xff);
    this.#view.setUint8(startIndex + 5, reportId & 0xff);
    this.#view.setUint16(startIndex + 6, payloadLen & 0xffff, true);
    this.#data.set(payload, startIndex + HID_REPORT_RECORD_HEADER_BYTES);

    const newTail = u32(tail + reserve);
    Atomics.store(this.#ctrl, CtrlIndex.Tail, newTail | 0);
    return true;
  }

  pop(): HidReportRingRecord | null {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
      const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
      if (head === tail) return null;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;

      // Not enough contiguous bytes for a header; skip to the wrap boundary.
      if (remaining < HID_REPORT_RECORD_HEADER_BYTES) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const reportType = this.#view.getUint8(headIndex + 4) as HidReportType;
      const reportId = this.#view.getUint8(headIndex + 5) >>> 0;
      const payloadLen = this.#view.getUint16(headIndex + 6, true) >>> 0;

      if (reportType === HidReportType.WrapMarker) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const total = alignUp(HID_REPORT_RECORD_HEADER_BYTES + payloadLen, HID_REPORT_RECORD_ALIGN);
      if (total > remaining) {
        // Corruption; do not advance head.
        return null;
      }

      const deviceId = this.#view.getUint32(headIndex + 0, true) >>> 0;
      const payloadStart = headIndex + HID_REPORT_RECORD_HEADER_BYTES;
      const payloadEnd = payloadStart + payloadLen;
      const payload = this.#data.slice(payloadStart, payloadEnd);

      Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
      return { deviceId, reportType, reportId, payload };
    }
  }

  /**
   * Consume (and commit) the next record without allocating a new payload buffer.
   *
   * Returns `false` when the ring is empty. The `payload` passed to `consumer` is
   * only valid until the next write to the ring (copy it if you need to retain
   * it asynchronously).
   */
  consumeNext(consumer: (record: HidReportRingRecord) => void): boolean {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
      const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
      if (head === tail) return false;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;

      if (remaining < HID_REPORT_RECORD_HEADER_BYTES) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const reportType = this.#view.getUint8(headIndex + 4) as HidReportType;
      const reportId = this.#view.getUint8(headIndex + 5) >>> 0;
      const payloadLen = this.#view.getUint16(headIndex + 6, true) >>> 0;

      if (reportType === HidReportType.WrapMarker) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const total = alignUp(HID_REPORT_RECORD_HEADER_BYTES + payloadLen, HID_REPORT_RECORD_ALIGN);
      if (total > remaining) return false;

      const deviceId = this.#view.getUint32(headIndex + 0, true) >>> 0;
      const payloadStart = headIndex + HID_REPORT_RECORD_HEADER_BYTES;
      const payloadEnd = payloadStart + payloadLen;
      const payload = this.#data.subarray(payloadStart, payloadEnd);

      consumer({ deviceId, reportType, reportId, payload });
      Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
      return true;
    }
  }
}

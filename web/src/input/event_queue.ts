export enum InputEventType {
  /**
   * A PS/2 set-2 scancode sequence.
   *
   * Payload:
   *   a = packed bytes (little-endian, b0 in bits 0..7)
   *   b = byte length (1..4)
   */
  KeyScancode = 1,
  /**
   * Relative mouse movement in PS/2 coordinate space (dx right, dy up).
   *
   * Payload:
   *   a = dx (signed 32-bit)
   *   b = dy (signed 32-bit)
   */
  MouseMove = 2,
  /**
   * Mouse button state bitmask.
   *
   * Payload:
   *   a = buttons (bit0=left, bit1=right, bit2=middle)
   *   b = unused
   */
  MouseButtons = 3,
  /**
   * Mouse wheel movement.
   *
   * Payload:
   *   a = dz (signed 32-bit, positive = wheel up)
   *   b = unused
   */
  MouseWheel = 4
}

export interface InputBatchMessage {
  type: 'in:input-batch';
  buffer: ArrayBuffer;
  /**
   * If set, the receiver should transfer `buffer` back to the sender once it is
   * done processing the batch. This allows the sender to reuse buffers and
   * avoid per-flush allocations.
   */
  recycle?: true;
}

export interface InputBatchRecycleMessage {
  type: "in:input-batch-recycle";
  buffer: ArrayBuffer;
}

export type InputBatchTarget = {
  postMessage: (msg: InputBatchMessage, transfer: Transferable[]) => void;
};

const HEADER_WORDS = 2;
const WORDS_PER_EVENT = 4;

let nextInputBatchId = 1;
type BufferFactory = (byteLength: number) => ArrayBuffer;

/**
  * High-throughput queue for input events. Pushes are allocation-free until the
  * backing buffer grows (rare) or the batch is flushed (by design).
 *
 * Wire format (Int32 little-endian):
 *   [0] count
 *   [1] batchSendTimestampUs (u32, wraps)
 *   for each event:
 *     [type, eventTimestampUs, a, b]
 */
export class InputEventQueue {
  private buf: ArrayBuffer;
  private words: Int32Array;
  private capacityEvents: number;
  private count = 0;
  private minTimestampUs = 0;
  private readonly bufferFactory: BufferFactory;

  constructor(capacityEvents = 128, bufferFactory: BufferFactory = (byteLength) => new ArrayBuffer(byteLength)) {
    this.capacityEvents = capacityEvents;
    this.bufferFactory = bufferFactory;
    this.buf = this.allocateBuffer((HEADER_WORDS + capacityEvents * WORDS_PER_EVENT) * 4);
    this.words = new Int32Array(this.buf);
  }

  get size(): number {
    return this.count;
  }

  pushKeyScancode(timestampUs: number, packedBytes: number, byteLen: number): void {
    this.push(InputEventType.KeyScancode, timestampUs, packedBytes | 0, byteLen | 0);
  }

  pushMouseMove(timestampUs: number, dx: number, dy: number): void {
    // Merge with previous mouse move to reduce event count without changing ordering.
    if (this.count > 0) {
      const base = HEADER_WORDS + (this.count - 1) * WORDS_PER_EVENT;
      if (this.words[base] === InputEventType.MouseMove) {
        this.words[base + 1] = timestampUs | 0;
        this.words[base + 2] = (this.words[base + 2] + (dx | 0)) | 0;
        this.words[base + 3] = (this.words[base + 3] + (dy | 0)) | 0;
        return;
      }
    }
    this.push(InputEventType.MouseMove, timestampUs, dx | 0, dy | 0);
  }

  pushMouseButtons(timestampUs: number, buttons: number): void {
    this.push(InputEventType.MouseButtons, timestampUs, buttons | 0, 0);
  }

  pushMouseWheel(timestampUs: number, dz: number): void {
    // Merge with previous wheel event.
    if (this.count > 0) {
      const base = HEADER_WORDS + (this.count - 1) * WORDS_PER_EVENT;
      if (this.words[base] === InputEventType.MouseWheel) {
        this.words[base + 1] = timestampUs | 0;
        this.words[base + 2] = (this.words[base + 2] + (dz | 0)) | 0;
        return;
      }
    }
    this.push(InputEventType.MouseWheel, timestampUs, dz | 0, 0);
  }

  /**
   * Transfers the internal ArrayBuffer to `target` and resets the queue. The
   * buffer is always transferred whole (small, fixed-size) to avoid extra copies.
   *
   * Returns the host-side latency in microseconds from the first event in the
   * batch to when the batch is sent, or `null` if the queue was empty.
   */
  flush(target: InputBatchTarget, opts: { recycle?: boolean } = {}): number | null {
    if (this.count === 0) {
      return null;
    }

    this.words[0] = this.count | 0;
    const sendTimestampUs = Math.round(performance.now() * 1000) >>> 0;
    this.words[1] = sendTimestampUs;
    const minTimestampUs = this.minTimestampUs;

    // Best-effort responsiveness telemetry hook. This avoids adding a hard perf
    // dependency to the input pipeline; the perf API is optional and may be
    // absent in minimal builds.
    const maybePerf = (globalThis as any).aero?.perf as
      | {
          noteInputCaptured?: (id: number, tCaptureMs?: number) => void;
          noteInputInjected?: (
            id: number,
            tInjectedMs?: number,
            queueDepth?: number,
            queueOldestCaptureMs?: number | null,
          ) => void;
        }
      | undefined;
    if (maybePerf?.noteInputCaptured || maybePerf?.noteInputInjected) {
      const id = nextInputBatchId++;
      const tCaptureMs = (minTimestampUs >>> 0) / 1000;
      const tInjectedMs = (sendTimestampUs >>> 0) / 1000;
      maybePerf.noteInputCaptured?.(id, tCaptureMs);
      maybePerf.noteInputInjected?.(id, tInjectedMs, this.count, tCaptureMs);
    }

    const byteLength = this.buf.byteLength;
    const buffer = this.buf;
    if (opts.recycle) {
      target.postMessage({ type: "in:input-batch", buffer, recycle: true }, [buffer]);
    } else {
      target.postMessage({ type: "in:input-batch", buffer }, [buffer]);
    }

    // The transferred buffer is now detached; allocate a fresh one.
    this.buf = this.allocateBuffer(byteLength);
    this.words = new Int32Array(this.buf);
    this.count = 0;
    this.minTimestampUs = 0;

    // Unsigned delta handles u32 wraparound.
    return (sendTimestampUs - minTimestampUs) >>> 0;
  }

  private push(type: InputEventType, timestampUs: number, a: number, b: number): void {
    if (this.count >= this.capacityEvents) {
      this.grow();
    }

    if (this.count === 0) {
      this.minTimestampUs = timestampUs >>> 0;
    }

    const base = HEADER_WORDS + this.count * WORDS_PER_EVENT;
    this.words[base] = type | 0;
    this.words[base + 1] = timestampUs | 0;
    this.words[base + 2] = a | 0;
    this.words[base + 3] = b | 0;
    this.count++;
  }

  private grow(): void {
    const nextCapacity = this.capacityEvents * 2;
    const nextBuf = this.allocateBuffer((HEADER_WORDS + nextCapacity * WORDS_PER_EVENT) * 4);
    new Int32Array(nextBuf).set(this.words);
    this.capacityEvents = nextCapacity;
    this.buf = nextBuf;
    this.words = new Int32Array(this.buf);
  }

  private allocateBuffer(byteLength: number): ArrayBuffer {
    const buf = this.bufferFactory(byteLength);
    if (!(buf instanceof ArrayBuffer)) {
      throw new Error("InputEventQueue bufferFactory must return an ArrayBuffer");
    }
    if (buf.byteLength !== byteLength) {
      throw new Error(`InputEventQueue bufferFactory returned ${buf.byteLength} bytes, expected ${byteLength}`);
    }
    return buf;
  }
}

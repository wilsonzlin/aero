import { alignUp, RECORD_ALIGN, ringCtrl, WRAP_MARKER } from "./layout";

export type AtomicsWaitResult = "ok" | "not-equal" | "timed-out";

function canAtomicsWait(): boolean {
  // Browser main thread disallows Atomics.wait; workers and Node allow it.
  // There's no perfect feature test; this is conservative.
  return typeof Atomics.wait === "function" && (globalThis as any).document === undefined;
}

function u32(n: number): number {
  return n >>> 0;
}

// MPSC/SPSC ring buffer backed by SharedArrayBuffer.
//
// Layout:
//   ctrl: Int32Array[4] (head, tail_reserve, tail_commit, capacity)
//   data: Uint8Array[capacity]
//
// Head/tail values are byte offsets, stored as wrapping u32 (but accessed via
// signed Int32Array for Atomics).
export class RingBuffer {
  private readonly ctrl: Int32Array;
  private readonly data: Uint8Array;
  private readonly view: DataView;
  private readonly cap: number;

  constructor(buffer: SharedArrayBuffer, offsetBytes: number) {
    this.ctrl = new Int32Array(buffer, offsetBytes, ringCtrl.WORDS);
    this.cap = u32(Atomics.load(this.ctrl, ringCtrl.CAPACITY));
    this.data = new Uint8Array(buffer, offsetBytes + ringCtrl.BYTES, this.cap);
    this.view = new DataView(this.data.buffer, this.data.byteOffset, this.data.byteLength);
  }

  capacityBytes(): number {
    return this.cap;
  }

  tryPush(payload: Uint8Array): boolean {
    const payloadLen = payload.byteLength;
    const recordSize = alignUp(4 + payloadLen, RECORD_ALIGN);
    if (recordSize > this.cap) return false;

    for (;;) {
      const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
      const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_RESERVE));

      const used = u32(tail - head);
      if (used > this.cap) continue; // raced with consumer
      const free = this.cap - used;

      const tailIndex = tail % this.cap;
      const remaining = this.cap - tailIndex;
      const needsWrap = remaining >= 4 && remaining < recordSize;
      const padding = remaining < recordSize ? remaining : 0;
      const reserve = padding + recordSize;
      if (reserve > free) return false;

      const newTail = u32(tail + reserve);
      const prev = Atomics.compareExchange(this.ctrl, ringCtrl.TAIL_RESERVE, tail | 0, newTail | 0);
      if (u32(prev) !== tail) continue;

      if (needsWrap) {
        this.view.setUint32(tailIndex, WRAP_MARKER, true);
      }

      const start = u32(tail + padding);
      const startIndex = start % this.cap;
      this.view.setUint32(startIndex, payloadLen, true);
      this.data.set(payload, startIndex + 4);

      // In-order commit.
      for (;;) {
        const committed = Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT);
        if (u32(committed) === tail) break;
        if (canAtomicsWait()) {
          Atomics.wait(this.ctrl, ringCtrl.TAIL_COMMIT, committed);
        }
      }

      Atomics.store(this.ctrl, ringCtrl.TAIL_COMMIT, newTail | 0);
      Atomics.notify(this.ctrl, ringCtrl.TAIL_COMMIT, 1);
      return true;
    }
  }

  tryPop(): Uint8Array | null {
    for (;;) {
      const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
      const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT));
      if (head === tail) return null;

      const headIndex = head % this.cap;
      const remaining = this.cap - headIndex;
      if (remaining < 4) {
        Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + remaining) | 0);
        Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
        continue;
      }

      const len = this.view.getUint32(headIndex, true);
      if (len === WRAP_MARKER) {
        Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + remaining) | 0);
        Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
        continue;
      }

      const total = alignUp(4 + len, RECORD_ALIGN);
      if (total > remaining) return null; // corruption

      const start = headIndex + 4;
      const end = start + len;
      const out = this.data.slice(start, end);

      Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + total) | 0);
      Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
      return out;
    }
  }

  waitForData(timeoutMs?: number): AtomicsWaitResult {
    if (!canAtomicsWait()) throw new Error("Atomics.wait not available in this context");
    for (;;) {
      const head = Atomics.load(this.ctrl, ringCtrl.HEAD);
      const tail = Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT);
      if (head !== tail) return "ok";
      // Atomics.wait returns one of: "ok" | "not-equal" | "timed-out".
      return Atomics.wait(this.ctrl, ringCtrl.TAIL_COMMIT, tail, timeoutMs) as AtomicsWaitResult;
    }
  }

  // Blocking helpers (workers / Node only).

  popBlocking(timeoutMs?: number): Uint8Array {
    for (;;) {
      const msg = this.tryPop();
      if (msg) return msg;
      const res = this.waitForData(timeoutMs);
      if (res === "timed-out") throw new Error("popBlocking timed out");
    }
  }

  pushBlocking(payload: Uint8Array, timeoutMs?: number): void {
    const recordSize = alignUp(4 + payload.byteLength, RECORD_ALIGN);
    if (recordSize > this.cap) throw new Error("payload too large for ring buffer");

    if (!canAtomicsWait()) throw new Error("Atomics.wait not available in this context");
    for (;;) {
      if (this.tryPush(payload)) return;
      const head = Atomics.load(this.ctrl, ringCtrl.HEAD);
      const res = Atomics.wait(this.ctrl, ringCtrl.HEAD, head, timeoutMs) as AtomicsWaitResult;
      if (res === "timed-out") throw new Error("pushBlocking timed out");
    }
  }
}

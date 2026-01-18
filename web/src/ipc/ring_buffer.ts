import { alignUp, RECORD_ALIGN, ringCtrl, WRAP_MARKER } from "./layout.ts";
import { unrefBestEffort } from "../unrefSafe";

export type AtomicsWaitResult = "ok" | "not-equal" | "timed-out";

function canAtomicsWait(): boolean {
  // Browser main thread disallows Atomics.wait; workers and Node allow it.
  // There's no perfect feature test; this is conservative.
  return typeof Atomics.wait === "function" && typeof document === "undefined";
}

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

function sleepAsync(timeoutMs: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, timeoutMs);
    unrefBestEffort(timer);
  });
}

function u32(n: number): number {
  return n >>> 0;
}

// Payload length is stored as a `u32`, and `WRAP_MARKER` reserves `u32::MAX` as a sentinel.
// Mirror `crates/aero-ipc/src/ring.rs`.
const MAX_PAYLOAD_LEN = 0xffff_fffe;

function ringBufferCorruption(message: string): Error {
  return new Error(`RingBuffer corrupted (${message}).`);
}

type AtomicsWaitAsyncResult =
  | { async: false; value: AtomicsWaitResult }
  | { async: true; value: Promise<AtomicsWaitResult> };

function atomicsWaitAsync(
  arr: Int32Array,
  index: number,
  value: number,
  timeoutMs?: number,
): AtomicsWaitAsyncResult | null {
  // TS lib definitions don't consistently include Atomics.waitAsync yet, so use an
  // untyped access with a narrow wrapper.
  const fn = (Atomics as unknown as { waitAsync?: unknown }).waitAsync;
  if (typeof fn !== "function") return null;
  return (fn as (arr: Int32Array, index: number, value: number, timeout?: number) => AtomicsWaitAsyncResult)(
    arr,
    index,
    value,
    timeoutMs,
  );
}

async function waitForStateChangeAsync(
  arr: Int32Array,
  index: number,
  expected: number,
  timeoutMs?: number,
): Promise<AtomicsWaitResult> {
  const res = atomicsWaitAsync(arr, index, expected, timeoutMs);
  if (res) {
    return res.async ? await res.value : res.value;
  }

  // Polling fallback (e.g. browsers without Atomics.waitAsync).
  //
  // The legacy implementation used `setTimeout(0)` in a tight loop which can
  // consume substantial CPU (Node) and introduce jitter (browsers with timer
  // clamping). Prefer sleeping in small slices with a backoff when no deadline
  // is provided.
  const start = nowMs();
  const pollSliceMs = 4;
  const backoffCapMs = 8;
  let backoffMs = 1;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const cur = Atomics.load(arr, index);
    if (cur !== expected) return "not-equal";
    if (timeoutMs != null) {
      const elapsed = nowMs() - start;
      if (elapsed >= timeoutMs) return "timed-out";
      const remaining = timeoutMs - elapsed;
      // Avoid pathological busy polling; never schedule a 0ms timeout in Node,
      // and limit jitter by polling in short slices.
      const sleepMs = Math.max(1, Math.min(remaining, pollSliceMs));
      await sleepAsync(sleepMs);
      continue;
    }

    await sleepAsync(backoffMs);
    backoffMs = Math.min(backoffMs * 2, backoffCapMs);
  }
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

  reset(): void {
    // Resets the ring to the empty state.
    //
    // WARNING: This must only be called when there are no concurrent producers or
    // consumers operating on the ring (e.g. during worker/session startup).
    Atomics.store(this.ctrl, ringCtrl.HEAD, 0);
    Atomics.store(this.ctrl, ringCtrl.TAIL_RESERVE, 0);
    Atomics.store(this.ctrl, ringCtrl.TAIL_COMMIT, 0);
    Atomics.notify(this.ctrl, ringCtrl.HEAD);
    Atomics.notify(this.ctrl, ringCtrl.TAIL_COMMIT);
  }

  tryPush(payload: Uint8Array): boolean {
    const payloadLen = payload.byteLength;
    if (payloadLen > MAX_PAYLOAD_LEN) return false;
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

  /**
   * Reserve space for a payload and let the caller write it directly into the ring.
   *
   * This avoids allocating an intermediate payload buffer (useful for hot paths like
   * HID input report forwarding).
   *
   * The writer is given a `Uint8Array` view backed by the ring's underlying
   * SharedArrayBuffer. The view is only valid until the next successful push that
   * overwrites the same region; callers must copy if they need to retain it.
   */
  tryPushWithWriter(payloadLen: number, writer: (dest: Uint8Array) => void): boolean {
    if (!Number.isFinite(payloadLen)) return false;
    const lenRaw = Math.floor(payloadLen);
    if (lenRaw < 0) return false;
    if (lenRaw > MAX_PAYLOAD_LEN) return false;
    const len = lenRaw >>> 0;
    const recordSize = alignUp(4 + len, RECORD_ALIGN);
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
      this.view.setUint32(startIndex, len, true);
      const dest = this.data.subarray(startIndex + 4, startIndex + 4 + len);
      try {
        writer(dest);
      } catch {
        dest.fill(0);
      }

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

  /**
   * Single-producer variant of {@link RingBuffer.tryPushWithWriter} that throws on corruption.
   *
   * The generic `tryPushWithWriter` implementation supports multiple concurrent producers and
   * enforces in-order commits via a `TAIL_COMMIT` spin/wait loop. On the browser main thread,
   * `Atomics.wait` is unavailable, so a corrupted commit state can otherwise spin indefinitely.
   *
   * Use this helper when the ring is known to have exactly one producer (common for main-thread
   * producers pushing to a worker consumer).
   */
  tryPushWithWriterSpsc(payloadLen: number, writer: (dest: Uint8Array) => void): boolean {
    if (!Number.isFinite(payloadLen)) return false;
    const lenRaw = Math.floor(payloadLen);
    if (lenRaw < 0) return false;
    if (lenRaw > MAX_PAYLOAD_LEN) return false;
    const len = lenRaw >>> 0;
    const recordSize = alignUp(4 + len, RECORD_ALIGN);
    if (recordSize > this.cap) return false;

    const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
    const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT));
    const used = u32(tail - head);
    if (used > this.cap) throw ringBufferCorruption("tail/head out of range");
    const free = this.cap - used;

    const tailIndex = tail % this.cap;
    const remaining = this.cap - tailIndex;
    const needsWrap = remaining >= 4 && remaining < recordSize;
    const padding = remaining < recordSize ? remaining : 0;
    const reserve = padding + recordSize;
    if (reserve > free) return false;

    if (needsWrap) {
      this.view.setUint32(tailIndex, WRAP_MARKER, true);
    }

    const start = u32(tail + padding);
    const startIndex = start % this.cap;
    this.view.setUint32(startIndex, len, true);
    const dest = this.data.subarray(startIndex + 4, startIndex + 4 + len);
    try {
      writer(dest);
    } catch {
      dest.fill(0);
    }

    const newTail = u32(tail + reserve);
    Atomics.store(this.ctrl, ringCtrl.TAIL_RESERVE, newTail | 0);
    Atomics.store(this.ctrl, ringCtrl.TAIL_COMMIT, newTail | 0);
    Atomics.notify(this.ctrl, ringCtrl.TAIL_COMMIT, 1);
    return true;
  }

  /**
   * Consume (and commit) the next record without allocating a new payload buffer.
   *
   * Returns `false` when the ring is empty. The `payload` passed to `consumer` is
   * only valid until the next write to the ring (copy it if you need to retain
   * it asynchronously).
   */
  consumeNext(consumer: (payload: Uint8Array) => void): boolean {
    for (;;) {
      const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
      const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT));
      if (head === tail) return false;

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
      if (total > remaining) return false; // corruption

      const payloadStart = headIndex + 4;
      const payloadEnd = payloadStart + len;
      const payload = this.data.subarray(payloadStart, payloadEnd);
      consumer(payload);

      Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + total) | 0);
      Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
      return true;
    }
  }

  /**
   * Like {@link RingBuffer.consumeNext}, but throws when corruption is detected.
   *
   * This is useful for background drain loops that must be able to fall back to an
   * alternate transport when the ring becomes unreadable (e.g. due to memory
   * corruption).
   */
  consumeNextOrThrow(consumer: (payload: Uint8Array) => void): boolean {
    for (;;) {
      const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
      const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT));
      if (head === tail) return false;

      const used = u32(tail - head);
      if (used > this.cap) throw ringBufferCorruption("tail/head out of range");

      const headIndex = head % this.cap;
      const remaining = this.cap - headIndex;
      if (remaining < 4) {
        if (used < remaining) throw ringBufferCorruption("tail inside wrap padding");
        Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + remaining) | 0);
        Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
        continue;
      }

      const len = this.view.getUint32(headIndex, true);
      if (len === WRAP_MARKER) {
        if (used < remaining) throw ringBufferCorruption("wrap marker exceeds available bytes");
        Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + remaining) | 0);
        Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
        continue;
      }

      const total = alignUp(4 + len, RECORD_ALIGN);
      if (total > remaining) throw ringBufferCorruption("record straddles wrap boundary");
      if (total > used) throw ringBufferCorruption("record exceeds available bytes");

      const payloadStart = headIndex + 4;
      const payloadEnd = payloadStart + len;
      const payload = this.data.subarray(payloadStart, payloadEnd);
      consumer(payload);

      Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + total) | 0);
      Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
      return true;
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

  // Non-blocking wait usable on the main thread via Atomics.waitAsync (if
  // available), with a polling fallback.
  async waitForDataAsync(timeoutMs?: number): Promise<AtomicsWaitResult> {
    const start = typeof performance !== "undefined" ? performance.now() : Date.now();
    for (;;) {
      const head = Atomics.load(this.ctrl, ringCtrl.HEAD);
      const tail = Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT);
      if (head !== tail) return "ok";

      const remaining =
        timeoutMs == null
          ? undefined
          : Math.max(
              0,
              timeoutMs -
                ((typeof performance !== "undefined" ? performance.now() : Date.now()) - start),
            );

      const status = await waitForStateChangeAsync(this.ctrl, ringCtrl.TAIL_COMMIT, tail, remaining);
      if (status === "timed-out") return "timed-out";
    }
  }

  // Wait until the consumer advances the head pointer (i.e. some data was
  // consumed / space may have been freed).
  //
  // This is useful for producer-side scheduling loops that want to flush pending
  // work (e.g. `L2TunnelForwarder` pushing into NET_RX after it was previously
  // full) without polling.
  async waitForConsumeAsync(timeoutMs?: number): Promise<AtomicsWaitResult> {
    const head = Atomics.load(this.ctrl, ringCtrl.HEAD);
    const status = await waitForStateChangeAsync(this.ctrl, ringCtrl.HEAD, head, timeoutMs);
    return status === "timed-out" ? "timed-out" : "ok";
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
    if (payload.byteLength > MAX_PAYLOAD_LEN) {
      throw new Error("payload too large for ring buffer");
    }
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

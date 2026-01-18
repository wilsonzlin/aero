import { wsSendSafe } from "./ws_safe.js";
import { unrefBestEffort } from "./unref_safe.js";

/**
 * @typedef {object} WsSendQueueOptions
 * @property {any=} ws
 * @property {number=} highWatermarkBytes
 * @property {number=} lowWatermarkBytes
 * @property {number=} pollMs
 * @property {(() => void)=} onPauseSources
 * @property {(() => void)=} onResumeSources
 * @property {(err: unknown) => void=} onSendError
 */

/**
 * A tiny helper for bounding attacker-controlled buffering when writing to a `ws` WebSocket.
 * It combines a controlled send queue with `ws.bufferedAmount` and exposes a backpressure
 * signal (`isBackpressured()`) suitable for pausing upstream TCP reads.
 *
 * This is intentionally generic and does not assume a specific framing protocol.
 *
 * @param {WsSendQueueOptions} opts
 */
export function createWsSendQueue(opts) {
  const ws = opts?.ws;
  const highWatermarkBytes = Number.isFinite(opts?.highWatermarkBytes) ? opts.highWatermarkBytes : 8 * 1024 * 1024;
  const lowWatermarkBytes = Number.isFinite(opts?.lowWatermarkBytes) ? opts.lowWatermarkBytes : Math.max(1, Math.floor(highWatermarkBytes / 2));
  const pollMs = Number.isFinite(opts?.pollMs) ? opts.pollMs : 10;
  const onPauseSources = typeof opts?.onPauseSources === "function" ? opts.onPauseSources : null;
  const onResumeSources = typeof opts?.onResumeSources === "function" ? opts.onResumeSources : null;
  const onSendError = typeof opts?.onSendError === "function" ? opts.onSendError : null;

  /** @type {Buffer[]} */
  const queue = [];
  let queueBytes = 0;
  let flushScheduled = false;
  let backpressureActive = false;
  /** @type {ReturnType<typeof setTimeout> | null} */
  let backpressurePollTimer = null;

  function isOpen() {
    if (ws == null || (typeof ws !== "object" && typeof ws !== "function")) return false;
    try {
      const readyState = ws.readyState;
      if (typeof readyState !== "number") return true;
      const openState = typeof ws.OPEN === "number" ? ws.OPEN : 1;
      return readyState === openState;
    } catch {
      return false;
    }
  }

  function bufferedAmount() {
    try {
      const value = ws?.bufferedAmount;
      return Number.isFinite(value) && value >= 0 ? value : 0;
    } catch {
      return 0;
    }
  }

  function backlogBytes() {
    return queueBytes + bufferedAmount();
  }

  function clearBackpressurePoll() {
    const t = backpressurePollTimer;
    if (!t) return;
    backpressurePollTimer = null;
    try {
      clearTimeout(t);
    } catch {
      // ignore
    }
  }

  function scheduleBackpressurePoll() {
    if (backpressurePollTimer) return;
    backpressurePollTimer = setTimeout(() => {
      backpressurePollTimer = null;
      if (!isOpen()) return;
      maybeResume();
      if (backpressureActive) scheduleBackpressurePoll();
    }, pollMs);
    unrefBestEffort(backpressurePollTimer);
  }

  function maybePause() {
    if (!isOpen()) return;
    if (backpressureActive) return;
    if (backlogBytes() <= highWatermarkBytes) return;
    backpressureActive = true;
    try {
      onPauseSources?.();
    } catch {
      // ignore
    }
    scheduleBackpressurePoll();
  }

  function maybeResume() {
    if (!isOpen()) return;
    if (!backpressureActive) return;
    if (backlogBytes() > lowWatermarkBytes) return;
    backpressureActive = false;
    try {
      onResumeSources?.();
    } catch {
      // ignore
    }
  }

  function flush() {
    flushScheduled = false;
    if (!ws) return;
    try {
      if (typeof ws.send !== "function") return;
    } catch {
      return;
    }
    if (!isOpen()) return;

    while (queue.length > 0) {
      const frame = queue.shift();
      queueBytes -= frame.byteLength;
      const ok = wsSendSafe(ws, frame, (err) => {
        if (!err) return;
        try {
          onSendError?.(err);
        } catch {
          // ignore
        }
      });
      if (!ok) return;
      if (bufferedAmount() > highWatermarkBytes) break;
    }

    maybeResume();
    if (queue.length > 0) {
      const delayMs = bufferedAmount() > highWatermarkBytes ? pollMs : 0;
      scheduleFlush(delayMs);
    }
  }

  function scheduleFlush(delayMs = 0) {
    if (flushScheduled) return;
    flushScheduled = true;
    if (delayMs > 0) {
      const t = setTimeout(flush, delayMs);
      unrefBestEffort(t);
      return;
    }
    setImmediate(flush);
  }

  return Object.freeze({
    enqueue(frame) {
      if (!Buffer.isBuffer(frame)) return;
      queue.push(frame);
      queueBytes += frame.byteLength;
      maybePause();
      scheduleFlush();
    },

    isBackpressured() {
      return backpressureActive;
    },

    backlogBytes,

    close() {
      clearBackpressurePoll();
      queue.length = 0;
      queueBytes = 0;
      backpressureActive = false;
      flushScheduled = false;
    },
  });
}


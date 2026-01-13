import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailableClamped,
} from "./audio_worklet_ring_layout.js";

function nowMs() {
  // AudioWorkletGlobalScope exposes `currentTime` (seconds) / `currentFrame` (frames). When
  // running under Node-based tests, fall back to `performance.now()` / `Date.now()`.
  // eslint-disable-next-line no-undef
  const ct = typeof currentTime === "number" && Number.isFinite(currentTime) ? currentTime : null;
  if (ct !== null) return ct * 1000;
  if (typeof globalThis.performance?.now === "function") return globalThis.performance.now();
  return Date.now();
}

function wallNowMs() {
  // AudioWorkletGlobalScope exposes `currentTime`, but that clock is *paused* when the
  // AudioContext is suspended. For suspend/resume detection we need a wall clock.
  if (typeof globalThis.performance?.now === "function") return globalThis.performance.now();
  return Date.now();
}

// Atomically add missing frames to the underrun counter. The counter is a u32
// that wraps naturally at 2^32.
export function addUnderrunFrames(header, missingFrames) {
  const missing = missingFrames >>> 0;
  // Atomics.add returns the previous value.
  return (Atomics.add(header, UNDERRUN_COUNT_INDEX, missing) + missing) >>> 0;
}

const WorkletProcessorBase =
  typeof AudioWorkletProcessor === "undefined"
    ? class {
        constructor() {
          this.port = { postMessage() {} };
        }
      }
    : AudioWorkletProcessor;

export class AeroAudioProcessor extends WorkletProcessorBase {
  constructor(options) {
    super();

    const ringBuffer = options?.processorOptions?.ringBuffer;
    const channelCount = options?.processorOptions?.channelCount;
    const capacityFrames = options?.processorOptions?.capacityFrames;
    const sendUnderrunMessages = options?.processorOptions?.sendUnderrunMessages;
    const underrunMessageIntervalMs = options?.processorOptions?.underrunMessageIntervalMs;
    const discardOnResume = options?.processorOptions?.discardOnResume;

    // Underrun messages are optional diagnostics. Persistent underruns can happen at render-quantum
    // rate (~375 msg/sec at 48kHz), which can add overhead and worsen glitches. Default to *not*
    // posting per-underrun messages; the shared-memory counter is always updated.
    this._sendUnderrunMessages = sendUnderrunMessages === true;
    // Rate-limit underrun messages when enabled (default: 250ms). Treat invalid/zero/negative
    // values defensively to avoid accidental MessagePort spam.
    this._underrunMessageIntervalMs =
      typeof underrunMessageIntervalMs === "number" &&
      Number.isFinite(underrunMessageIntervalMs) &&
      underrunMessageIntervalMs > 0
        ? underrunMessageIntervalMs
        : 250;
    this._lastUnderrunMessageTimeMs = null;
    this._pendingUnderrunFrames = 0;

    // If enabled, discard any buffered backlog after a suspend/resume or other long pause in
    // processing. The processor has no direct access to AudioContext state; instead, detect resume
    // by observing a large wall-clock gap between `process()` callbacks.
    //
    // This is intentionally opt-in because it can also trigger after extreme scheduling stalls
    // (e.g. tab backgrounding, long GC pauses). In those cases, lowering latency by dropping stale
    // buffered audio is still desirable.
    this._discardOnResume = discardOnResume === true;
    this._lastWallTimeMs = null;

    if (typeof SharedArrayBuffer !== "undefined" && ringBuffer instanceof SharedArrayBuffer) {
      // Layout is described in:
      // - `web/src/platform/audio_worklet_ring_layout.js` (layout-only, AudioWorklet-safe)
      // - `web/src/audio/audio_worklet_ring.ts` (producer-side helpers; re-exports the same constants)
      //
      // `SharedArrayBuffer` contents/size may be untrusted (e.g. corrupted snapshot state or a
      // misbehaving host). Creating typed-array views can throw a RangeError if the buffer is too
      // small or misaligned; treat that as "no ring attached" and output silence rather than
      // crashing the AudioWorklet.
      let header = null;
      let samples = null;
      try {
        header = new Uint32Array(ringBuffer, 0, HEADER_U32_LEN);
        samples = new Float32Array(ringBuffer, HEADER_BYTES);
      } catch (_e) {
        header = null;
        samples = null;
      }
      this._header = header;
      this._samples = samples;
      this._channelCount = header && samples && typeof channelCount === "number" ? channelCount : null;
      this._capacityFrames = header && samples && typeof capacityFrames === "number" ? capacityFrames : null;
    } else {
      this._header = null;
      this._samples = null;
      this._channelCount = null;
      this._capacityFrames = null;
    }

    // Control messages from the main thread.
    //
    // The AudioWorklet may be resumed after long suspends (tab background, iOS interruption, etc.)
    // while the shared ring still contains buffered audio. A reset message allows the worklet to
    // atomically discard any backlog and resume "live".
    //
    // Be defensive: messages and shared state may be untrusted (corrupted snapshots / misbehaving
    // hosts). Never throw from the worklet thread.
    const port = this.port;
    if (port && typeof port === "object") {
      port.onmessage = (event) => {
        try {
          const data = event?.data;
          if (!data || typeof data !== "object") return;
          const msg = data;
          if (msg.type !== "ring.reset") return;
          if (!this._header) return;

          const writeFrameIndex = Atomics.load(this._header, WRITE_FRAME_INDEX) >>> 0;
          Atomics.store(this._header, READ_FRAME_INDEX, writeFrameIndex);
        } catch (_e) {
          // Ignore.
        }
      };
    }
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output) return true;

    // Always zero outputs first so we never leak stale samples if the ring is absent or
    // misconfigured.
    for (let c = 0; c < output.length; c++) output[c].fill(0);

    if (!this._header || !this._samples) {
      return true;
    }

    if (this._discardOnResume) {
      const now = wallNowMs();
      const last = this._lastWallTimeMs;
      this._lastWallTimeMs = now;

      // If there is a large wall-time gap between callbacks, treat this as a suspend/resume (or a
      // similarly disruptive stall) and discard any buffered frames so playback stays "live".
      //
      // Keep the threshold well above the nominal render quantum interval (~3ms @ 48kHz) so normal
      // scheduling jitter does not trigger it. 100ms is still comfortably below Playwright's
      // suspend/resume discard budget (the corresponding e2e spec expects recovery within ~350ms).
      const GAP_THRESHOLD_MS = 100;
      if (last !== null && Number.isFinite(last) && Number.isFinite(now) && now - last >= GAP_THRESHOLD_MS) {
        try {
          const writeFrameIndex = Atomics.load(this._header, WRITE_FRAME_INDEX) >>> 0;
          Atomics.store(this._header, READ_FRAME_INDEX, writeFrameIndex);
        } catch {
          // Ignore.
        }
      }
    }

    const framesNeeded = output[0]?.length ?? 0;
    if (framesNeeded === 0) return true;

    function parsePositiveSafeU32(value) {
      if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0 || value > 0xffff_ffff) return null;
      return value >>> 0;
    }

    // Defensive validation: callers can pass bogus values for `channelCount`/`capacityFrames` via
    // `processorOptions`, and the AudioWorklet must never index out of bounds into the shared ring.
    //
    // Clamp channelCount to the actual output channel count and derive an upper bound on the ring
    // capacity from the SharedArrayBuffer length.
    // Note: avoid `>>> 0` on non-safe integers; it wraps modulo 2^32, which can turn absurd values
    // into small-but-wrong capacities.
    let channelCount = parsePositiveSafeU32(this._channelCount);
    if (!channelCount) channelCount = output.length;
    channelCount = Math.min(channelCount, output.length);

    const maxCapacityFromBuffer = Math.floor(this._samples.length / channelCount);
    if (!Number.isFinite(maxCapacityFromBuffer) || maxCapacityFromBuffer <= 0) {
      return true;
    }

    // Cap to match other layers of the stack (Rust `WorkletBridge` / TS helpers) so untrusted
    // inputs cannot make the worklet do multi-second per-callback work.
    const MAX_CAPACITY_FRAMES = 1_048_576; // 2^20 frames (~21s @ 48kHz)

    let capacityFrames = parsePositiveSafeU32(this._capacityFrames);
    capacityFrames = capacityFrames ? Math.min(capacityFrames, maxCapacityFromBuffer) : maxCapacityFromBuffer;
    capacityFrames = Math.min(capacityFrames, MAX_CAPACITY_FRAMES);
    if (capacityFrames <= 0) return true;

    const readFrameIndex = Atomics.load(this._header, READ_FRAME_INDEX) >>> 0;
    const writeFrameIndex = Atomics.load(this._header, WRITE_FRAME_INDEX) >>> 0;
    const available = framesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames);
    const framesToRead = Math.min(framesNeeded, available);

    const samples = this._samples;
    const cc = channelCount;

    const readPos = readFrameIndex % capacityFrames;
    const firstFrames = Math.min(framesToRead, capacityFrames - readPos);
    const secondFrames = framesToRead - firstFrames;

    // Copy first contiguous chunk.
    for (let i = 0; i < firstFrames; i++) {
      const base = (readPos + i) * cc;
      for (let c = 0; c < cc; c++) {
        output[c][i] = samples[base + c];
      }
    }

    // Copy wrapped chunk.
    for (let i = 0; i < secondFrames; i++) {
      const base = i * cc;
      for (let c = 0; c < cc; c++) {
        output[c][firstFrames + i] = samples[base + c];
      }
    }

    // Any missing frames are already zeroed above.
    if (framesToRead < framesNeeded) {
      const missing = framesNeeded - framesToRead;
      const newTotal = addUnderrunFrames(this._header, missing);
      if (this._sendUnderrunMessages) {
        this._pendingUnderrunFrames = (this._pendingUnderrunFrames + (missing >>> 0)) >>> 0;

        const now = nowMs();
        const last = this._lastUnderrunMessageTimeMs;
        const intervalMs = this._underrunMessageIntervalMs;
        const canSend = last === null || !Number.isFinite(last) || now - last >= intervalMs || now < last;

        if (canSend) {
          this._lastUnderrunMessageTimeMs = now;
          const added = this._pendingUnderrunFrames >>> 0;
          this._pendingUnderrunFrames = 0;
          try {
            this.port.postMessage({
              type: "underrun",
              underrunFramesAdded: added,
              underrunFramesTotal: newTotal,
              // Backwards-compatible field name; this is a frame counter (not events).
              underrunCount: newTotal,
            });
          } catch (_e) {
            // Avoid crashing the AudioWorklet if the host's MessagePort is misbehaving.
          }
        }
      }
    }

    // If underruns were rate-limited and then recovered before the next message interval elapses,
    // flush a final message once allowed so consumers that rely on messages (instead of shared
    // memory) can observe the latest total.
    //
    // Note: this check intentionally only runs on non-underrun callbacks (framesToRead ==
    // framesNeeded) to avoid redundant time computations in persistent underrun scenarios.
    if (this._sendUnderrunMessages && this._pendingUnderrunFrames !== 0 && framesToRead === framesNeeded) {
      const now = nowMs();
      const last = this._lastUnderrunMessageTimeMs;
      const intervalMs = this._underrunMessageIntervalMs;
      const canSend = last === null || !Number.isFinite(last) || now - last >= intervalMs || now < last;

      if (canSend) {
        this._lastUnderrunMessageTimeMs = now;
        const added = this._pendingUnderrunFrames >>> 0;
        this._pendingUnderrunFrames = 0;
        const total = Atomics.load(this._header, UNDERRUN_COUNT_INDEX) >>> 0;
        try {
          this.port.postMessage({
            type: "underrun",
            underrunFramesAdded: added,
            underrunFramesTotal: total,
            // Backwards-compatible field name; this is a frame counter (not events).
            underrunCount: total,
          });
        } catch (_e) {
          // Ignore.
        }
      }
    }

    if (framesToRead > 0) {
      Atomics.store(this._header, READ_FRAME_INDEX, readFrameIndex + framesToRead);
    }

    return true;
  }
}

if (typeof registerProcessor === "function") {
  registerProcessor("aero-audio-processor", AeroAudioProcessor);
}

// When this module is imported directly (e.g. by Node-based tests), provide a
// default export so `import ... from "./audio-worklet-processor.js?worker&url"`
// can resolve without Vite's `?worker&url` transform.
export default import.meta.url;

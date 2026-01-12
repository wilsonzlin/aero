import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailableClamped,
} from "./audio_worklet_ring_layout.js";

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
      this.port.postMessage({
        type: "underrun",
        underrunFramesAdded: missing,
        underrunFramesTotal: newTotal,
        // Backwards-compatible field name; this is a frame counter (not events).
        underrunCount: newTotal,
      });
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

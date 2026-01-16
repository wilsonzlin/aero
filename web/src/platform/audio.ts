import {
  HEADER_BYTES as AUDIO_WORKLET_RING_HEADER_BYTES,
  framesFree,
  getRingBufferLevelFrames as getAudioWorkletRingLevelFrames,
  requiredBytes as audioWorkletRingRequiredBytes,
  type AudioWorkletRingBufferViews,
  wrapRingBuffer as wrapAudioWorkletRingBuffer,
} from "../audio/audio_worklet_ring";
import { formatOneLineError, formatOneLineUtf8 } from "../text";

// The audio worklet processor is loaded at runtime via `AudioWorklet.addModule()`.
//
// Use `new URL(..., import.meta.url)` instead of Vite-only `?worker&url` imports so that:
// - Node unit tests can import this module without a bundler transform.
// - Vite still emits the worklet module as a separate asset and rewrites the URL in builds.
const audioWorkletProcessorUrl = new URL("./audio-worklet-processor.js", import.meta.url).toString();

export type CreateAudioOutputOptions = {
  sampleRate?: number;
  latencyHint?: AudioContextLatencyCategory | number;
  channelCount?: number;
  /**
   * If true, the AudioWorklet posts `type: "underrun"` messages when it renders missing frames.
   *
   * This is intended for debugging/diagnostics. In persistent underrun situations, posting a
   * message every render quantum can be expensive, so messages are disabled by default and (when
   * enabled) rate-limited via `underrunMessageIntervalMs`.
   */
  sendUnderrunMessages?: boolean;
  /**
   * Minimum interval between `type: "underrun"` messages posted by the AudioWorklet, in
   * milliseconds (default handled by the worklet).
   */
  underrunMessageIntervalMs?: number;
  /**
   * Prefill this many frames of silence into the playback ring buffer at startup
   * if the ring is empty.
   *
   * This avoids counting an initial underrun while the producer starts writing
   * into the ring buffer (and can mask slow-starting producers), at the cost of
   * delaying the first audible sample by roughly `startupPrefillFrames /
   * AudioContext.sampleRate` seconds.
   *
   * Defaults to `512` frames.
   */
  startupPrefillFrames?: number;
  /**
   * When an AudioContext is resumed after being suspended/interrupted, the shared ring buffer may
   * contain a backlog of buffered audio. Discarding the backlog keeps playback "live" instead of
   * playing stale buffered audio with high latency.
   *
   * This is enabled by default, but it is intentionally *not* applied to the first-ever transition
   * to `"running"` so that the initial silence prefill can still prevent startup underrun spam.
   *
   * Defaults to `true`.
   */
  discardOnResume?: boolean;
  /**
   * Size of the ring buffer in frames (per channel).
   *
   * The actual sample capacity is `ringBufferFrames * channelCount`.
   *
   * If omitted (and `ringBuffer` is not provided), this defaults to ~200ms of
   * audio at the *actual* `AudioContext.sampleRate` (`sampleRate / 5`, clamped to
   * `[2048, sampleRate / 2]`). Browsers may ignore a requested `sampleRate`, so
   * the default is derived from the constructed AudioContext's rate.
   */
  ringBufferFrames?: number;
  /**
   * Optional pre-allocated SharedArrayBuffer to use as the ring buffer.
   *
   * This is useful when the emulator/WASM side owns the SharedArrayBuffer and
   * wants to hand it to the AudioWorklet output pipeline.
   */
  ringBuffer?: SharedArrayBuffer;
};

/**
 * Shared ring buffer used for inter-thread audio sample transport.
 *
 * Layout (little-endian):
 * - u32 readFrameIndex (bytes 0..4)
 * - u32 writeFrameIndex (bytes 4..8)
 * - u32 underrunCount (bytes 8..12): total missing output frames rendered as silence due to underruns (wraps at 2^32)
 * - u32 overrunCount (bytes 12..16): frames dropped by the producer due to buffer full (wraps at 2^32)
 * - f32 samples[] (bytes 16..)
 *
 * Indices are monotonically-increasing frame counters (wrapping naturally at
 * 2^32). The producer writes samples first, then atomically advances
 * `writeFrameIndex`. The consumer reads available frames (clamped to the
 * configured capacity) and then atomically advances `readFrameIndex`.
 */
export type AudioRingBufferLayout = AudioWorkletRingBufferViews & {
  buffer: SharedArrayBuffer;
  channelCount: number;
  capacityFrames: number;
};

export type AudioOutputState = AudioContextState | "disabled";

export type AudioOutputMetrics = {
  bufferLevelFrames: number;
  capacityFrames: number;
  /**
   * Total missing output frames rendered as silence due to underruns (wraps at 2^32).
   */
  underrunCount: number;
  /**
   * Total frames dropped by the producer due to buffer full (wraps at 2^32).
   */
  overrunCount: number;
  sampleRate: number;
  state: AudioOutputState;
  /**
   * The `AudioContext.baseLatency` value (seconds) when available.
   *
   * This is a best-effort introspection field; some browsers do not expose it.
   */
  baseLatencySeconds?: number;
  /**
   * The `AudioContext.outputLatency` value (seconds) when available.
   *
   * This is a best-effort introspection field; some browsers do not expose it.
   */
  outputLatencySeconds?: number;
};

export type EnabledAudioOutput = {
  enabled: true;
  message?: string;
  context: AudioContext;
  node: AudioWorkletNode;
  ringBuffer: AudioRingBufferLayout;
  resume(): Promise<void>;
  close(): Promise<void>;
  /**
   * Instruct the AudioWorklet to discard any buffered audio frames (consumer-side reset).
   *
   * Useful after `AudioContext` suspend/resume cycles to avoid stale latency.
   */
  discardBufferedFrames(): void;
  /**
   * Write interleaved `f32` samples into the ring buffer.
   *
   * `srcSampleRate` is used for naive linear resampling if it differs from the
   * AudioContext's sample rate.
   */
  writeInterleaved(samples: Float32Array, srcSampleRate: number): number;
  getBufferLevelFrames(): number;
  /**
   * Total missing output frames rendered as silence due to underruns (wraps at 2^32).
   */
  getUnderrunCount(): number;
  getOverrunCount(): number;
  getMetrics(): AudioOutputMetrics;
};

export type DisabledAudioOutput = {
  enabled: false;
  message: string;
  ringBuffer?: AudioRingBufferLayout;
  resume(): Promise<void>;
  close(): Promise<void>;
  writeInterleaved(_samples: Float32Array, _srcSampleRate: number): number;
  getBufferLevelFrames(): number;
  /**
   * Total missing output frames rendered as silence due to underruns (wraps at 2^32).
   *
   * (Always 0 when audio is disabled.)
   */
  getUnderrunCount(): number;
  getOverrunCount(): number;
  getMetrics(): AudioOutputMetrics;
};

export type AudioOutput = EnabledAudioOutput | DisabledAudioOutput;

export { restoreAudioWorkletRing, type AudioWorkletRingStateLike } from "./audio_ring_restore";

function clampFrames(value: number, min: number, max: number): number {
  const v = Math.floor(Number.isFinite(value) ? value : 0);
  const lo = Math.floor(Number.isFinite(min) ? min : 0);
  const hi = Math.floor(Number.isFinite(max) ? max : 0);
  const clampedHi = Math.max(lo, hi);
  return Math.min(Math.max(v, lo), clampedHi);
}

function finiteNonNegative(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : undefined;
}

export function getDefaultRingBufferFrames(sampleRate: number): number {
  // Default to ~200ms of capacity to keep interactive audio latency reasonable,
  // while still allowing producers to buffer enough samples to avoid underruns.
  // Producers should generally target a smaller steady-state fill level (see
  // `createAdaptiveRingBufferTarget`) so latency stays in the tens of ms.
  const minFrames = 2048;
  const maxFrames = Math.max(minFrames, Math.floor(sampleRate / 2));
  return clampFrames(sampleRate / 5, minFrames, maxFrames);
}

export type AdaptiveRingBufferTargetOptions = {
  minTargetFrames?: number;
  maxTargetFrames?: number;
  initialTargetFrames?: number;
  /**
   * AudioWorklet render quantum size, in frames per channel.
   *
   * The underrun counter (`underrunCount`) tracks missing output frames rendered
   * as silence. To decide how aggressively to increase the buffering target we
   * scale underrun frames relative to the render quantum size.
   *
   * Web Audio currently uses a fixed render quantum of 128 frames.
   */
  renderQuantumFrames?: number;
  /**
   * How many frames to add to the target per *full* render quantum worth of
   * underrun (scaled proportionally for partial underruns).
   */
  increaseFrames?: number;
  /**
   * If there are no underruns for this many seconds, start slowly decreasing the
   * target.
   */
  stableSeconds?: number;
  /**
   * How many frames to subtract from the target when stable.
   */
  decreaseFrames?: number;
  /**
   * How often to apply `decreaseFrames` while stable.
   */
  decreaseIntervalSeconds?: number;
  /**
   * If the observed buffer level falls below `target * lowWaterMarkRatio`, bump
   * the target up (even if an underrun hasn't occurred yet).
   */
  lowWaterMarkRatio?: number;
};

export type AdaptiveRingBufferTarget = {
  getTargetFrames(): number;
  update(bufferLevelFrames: number, underrunCount: number, nowMs?: number): number;
};

export function createAdaptiveRingBufferTarget(
  capacityFrames: number,
  sampleRate: number,
  options: AdaptiveRingBufferTargetOptions = {},
): AdaptiveRingBufferTarget {
  const minTargetFrames = clampFrames(options.minTargetFrames ?? 2048, 0, capacityFrames);
  const maxTargetFrames = clampFrames(options.maxTargetFrames ?? capacityFrames, minTargetFrames, capacityFrames);

  const initialTargetFrames = clampFrames(
    options.initialTargetFrames ?? Math.floor(sampleRate / 20), // ~50ms
    minTargetFrames,
    maxTargetFrames,
  );

  const renderQuantumFrames = clampFrames(options.renderQuantumFrames ?? 128, 1, 4096);
  const increaseFrames = Math.max(1, Math.floor(options.increaseFrames ?? sampleRate / 50)); // ~20ms
  const stableSeconds = Number.isFinite(options.stableSeconds) ? (options.stableSeconds ?? 3) : 3;
  const decreaseFrames = Math.max(1, Math.floor(options.decreaseFrames ?? sampleRate / 200)); // ~5ms
  const decreaseIntervalMs = Math.max(0.1, options.decreaseIntervalSeconds ?? 1) * 1000;
  const lowWaterMarkRatio = Number.isFinite(options.lowWaterMarkRatio) ? (options.lowWaterMarkRatio ?? 0.25) : 0.25;

  const nowDefault = () =>
    typeof globalThis.performance?.now === "function" ? globalThis.performance.now() : Date.now();

  let targetFrames = initialTargetFrames;
  let lastUnderrunFrames: number | null = null;
  let lastUnderrunTimeMs = nowDefault();
  let lastDecreaseTimeMs = lastUnderrunTimeMs;

  return {
    getTargetFrames() {
      return targetFrames;
    },
    update(bufferLevelFrames: number, underrunCount: number, nowMs?: number) {
      const now = nowMs ?? nowDefault();
      const level = Math.max(0, Math.floor(bufferLevelFrames));
      const underrunFrames = Math.max(0, Math.floor(underrunCount));

      if (lastUnderrunFrames === null) {
        lastUnderrunFrames = underrunFrames;
        lastUnderrunTimeMs = now;
        lastDecreaseTimeMs = now;
        return targetFrames;
      }

      const deltaUnderrunFrames = (underrunFrames - lastUnderrunFrames) >>> 0;
      lastUnderrunFrames = underrunFrames;

      if (deltaUnderrunFrames > 0) {
        const scaledIncrease = Math.ceil((deltaUnderrunFrames / renderQuantumFrames) * increaseFrames);
        targetFrames = clampFrames(targetFrames + scaledIncrease, minTargetFrames, maxTargetFrames);
        lastUnderrunTimeMs = now;
        lastDecreaseTimeMs = now;
        return targetFrames;
      }

      if (lowWaterMarkRatio > 0 && level < targetFrames * lowWaterMarkRatio) {
        targetFrames = clampFrames(targetFrames + increaseFrames, minTargetFrames, maxTargetFrames);
        lastDecreaseTimeMs = now;
        return targetFrames;
      }

      if (now - lastUnderrunTimeMs >= stableSeconds * 1000 && now - lastDecreaseTimeMs >= decreaseIntervalMs) {
        targetFrames = clampFrames(targetFrames - decreaseFrames, minTargetFrames, maxTargetFrames);
        lastDecreaseTimeMs = now;
      }

      return targetFrames;
    },
  };
}

function getAudioContextCtor(): typeof AudioContext | undefined {
  return (
    (globalThis as typeof globalThis & { webkitAudioContext?: typeof AudioContext }).AudioContext ??
    (globalThis as typeof globalThis & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  );
}

function createRingBuffer(channelCount: number, ringBufferFrames: number): AudioRingBufferLayout {
  if (typeof SharedArrayBuffer === "undefined") {
    throw new Error("SharedArrayBuffer is required for Aero audio ring buffers.");
  }

  const buffer = new SharedArrayBuffer(audioWorkletRingRequiredBytes(ringBufferFrames, channelCount));
  const views = wrapAudioWorkletRingBuffer(buffer, ringBufferFrames, channelCount);

  Atomics.store(views.readIndex, 0, 0);
  Atomics.store(views.writeIndex, 0, 0);
  Atomics.store(views.underrunCount, 0, 0);
  Atomics.store(views.overrunCount, 0, 0);

  return {
    buffer,
    ...views,
    channelCount,
    capacityFrames: ringBufferFrames,
  };
}

function wrapRingBuffer(buffer: SharedArrayBuffer, channelCount: number, ringBufferFrames: number): AudioRingBufferLayout {
  const views = wrapAudioWorkletRingBuffer(buffer, ringBufferFrames, channelCount);

  return {
    buffer,
    ...views,
    channelCount,
    capacityFrames: ringBufferFrames,
  };
}

function inferRingBufferFrames(buffer: SharedArrayBuffer, channelCount: number): number {
  const payloadBytes = buffer.byteLength - AUDIO_WORKLET_RING_HEADER_BYTES;
  if (payloadBytes < 0 || payloadBytes % Float32Array.BYTES_PER_ELEMENT !== 0) {
    throw new Error("Provided ring buffer has an invalid byte length.");
  }
  const sampleCapacity = payloadBytes / Float32Array.BYTES_PER_ELEMENT;
  if (sampleCapacity % channelCount !== 0) {
    throw new Error("Provided ring buffer payload is not aligned to the requested channelCount.");
  }
  return sampleCapacity / channelCount;
}

export function getRingBufferLevelFrames(ringBuffer: AudioRingBufferLayout): number {
  return getAudioWorkletRingLevelFrames(ringBuffer.header, ringBuffer.capacityFrames);
}

export function getRingBufferUnderrunCount(ringBuffer: AudioRingBufferLayout): number {
  return Atomics.load(ringBuffer.underrunCount, 0) >>> 0;
}

export function getRingBufferOverrunCount(ringBuffer: AudioRingBufferLayout): number {
  return Atomics.load(ringBuffer.overrunCount, 0) >>> 0;
}

export function resampleLinearInterleaved(
  input: Float32Array,
  channelCount: number,
  srcRate: number,
  dstRate: number,
): Float32Array {
  if (!Number.isFinite(channelCount) || channelCount <= 0) {
    return new Float32Array();
  }
  if (!Number.isFinite(srcRate) || !Number.isFinite(dstRate) || srcRate <= 0 || dstRate <= 0) {
    return new Float32Array();
  }
  if (srcRate === dstRate) return input;

  const srcFrames = Math.floor(input.length / channelCount);
  if (srcFrames === 0) return new Float32Array();

  const ratio = dstRate / srcRate;
  if (!Number.isFinite(ratio) || ratio <= 0) return new Float32Array();
  const dstFrames = Math.floor(srcFrames * ratio);
  const out = new Float32Array(dstFrames * channelCount);

  for (let dstI = 0; dstI < dstFrames; dstI++) {
    const srcPos = dstI / ratio;
    const srcI0 = Math.floor(srcPos);
    const frac = srcPos - srcI0;
    const srcI1 = Math.min(srcI0 + 1, srcFrames - 1);

    for (let c = 0; c < channelCount; c++) {
      const v0 = input[srcI0 * channelCount + c];
      const v1 = input[srcI1 * channelCount + c];
      out[dstI * channelCount + c] = v0 + (v1 - v0) * frac;
    }
  }

  return out;
}

function resampleLinearInterleavedCapped(
  input: Float32Array,
  channelCount: number,
  srcRate: number,
  dstRate: number,
  maxDstFrames: number,
): Float32Array {
  if (!Number.isFinite(channelCount) || channelCount <= 0) {
    return new Float32Array();
  }
  if (!Number.isFinite(srcRate) || !Number.isFinite(dstRate) || srcRate <= 0 || dstRate <= 0) {
    return new Float32Array();
  }
  if (!Number.isFinite(maxDstFrames) || maxDstFrames <= 0) {
    return new Float32Array();
  }
  if (srcRate === dstRate) return input;

  const srcFrames = Math.floor(input.length / channelCount);
  if (srcFrames === 0) return new Float32Array();

  const ratio = dstRate / srcRate;
  if (!Number.isFinite(ratio) || ratio <= 0) return new Float32Array();

  const dstFrames = Math.min(Math.floor(srcFrames * ratio), Math.floor(maxDstFrames));
  if (dstFrames <= 0) return new Float32Array();

  const out = new Float32Array(dstFrames * channelCount);

  for (let dstI = 0; dstI < dstFrames; dstI++) {
    const srcPos = dstI / ratio;
    const srcI0 = Math.floor(srcPos);
    const frac = srcPos - srcI0;
    const srcI1 = Math.min(srcI0 + 1, srcFrames - 1);

    for (let c = 0; c < channelCount; c++) {
      const v0 = input[srcI0 * channelCount + c];
      const v1 = input[srcI1 * channelCount + c];
      out[dstI * channelCount + c] = v0 + (v1 - v0) * frac;
    }
  }

  return out;
}

export function writeRingBufferInterleaved(
  ringBuffer: AudioRingBufferLayout,
  input: Float32Array,
  srcSampleRate: number,
  dstSampleRate: number,
): number {
  const cc = ringBuffer.channelCount;
  if (!Number.isFinite(cc) || cc <= 0) return 0;

  let requestedFrames = 0;
  if (srcSampleRate === dstSampleRate) {
    requestedFrames = Math.floor(input.length / cc);
  } else if (
    Number.isFinite(srcSampleRate) &&
    Number.isFinite(dstSampleRate) &&
    srcSampleRate > 0 &&
    dstSampleRate > 0 &&
    srcSampleRate !== dstSampleRate
  ) {
    const srcFrames = Math.floor(input.length / cc);
    const ratio = dstSampleRate / srcSampleRate;
    if (srcFrames > 0 && Number.isFinite(ratio) && ratio > 0) {
      const dstFrames = Math.floor(srcFrames * ratio);
      if (Number.isFinite(dstFrames) && dstFrames > 0) {
        requestedFrames = dstFrames;
      }
    }
  }
  if (requestedFrames === 0) return 0;

  const read = Atomics.load(ringBuffer.readIndex, 0) >>> 0;
  const write = Atomics.load(ringBuffer.writeIndex, 0) >>> 0;

  const free = framesFree(read, write, ringBuffer.capacityFrames);
  const framesToWrite = Math.min(requestedFrames, free);
  const droppedFrames = requestedFrames - framesToWrite;
  if (droppedFrames > 0) Atomics.add(ringBuffer.overrunCount, 0, droppedFrames);
  if (framesToWrite === 0) return 0;

  const writePos = write % ringBuffer.capacityFrames;
  const firstFrames = Math.min(framesToWrite, ringBuffer.capacityFrames - writePos);
  const secondFrames = framesToWrite - firstFrames;

  const firstSamples = firstFrames * cc;
  const secondSamples = secondFrames * cc;

  const samples =
    srcSampleRate === dstSampleRate
      ? input
      : resampleLinearInterleavedCapped(input, cc, srcSampleRate, dstSampleRate, framesToWrite);

  ringBuffer.samples.set(samples.subarray(0, firstSamples), writePos * cc);
  if (secondFrames > 0) {
    ringBuffer.samples.set(samples.subarray(firstSamples, firstSamples + secondSamples), 0);
  }

  Atomics.store(ringBuffer.writeIndex, 0, write + framesToWrite);
  return framesToWrite;
}

function prefillSilenceIfEmpty(ringBuffer: AudioRingBufferLayout, frames: number): void {
  if (frames <= 0) return;

  const read = Atomics.load(ringBuffer.readIndex, 0) >>> 0;
  const write = Atomics.load(ringBuffer.writeIndex, 0) >>> 0;
  if (read !== write) return;

  const framesToWrite = Math.min(frames, ringBuffer.capacityFrames);
  const writePos = write % ringBuffer.capacityFrames;
  const cc = ringBuffer.channelCount;
  const firstFrames = Math.min(framesToWrite, ringBuffer.capacityFrames - writePos);
  const secondFrames = framesToWrite - firstFrames;

  ringBuffer.samples.fill(0, writePos * cc, (writePos + firstFrames) * cc);
  if (secondFrames > 0) {
    ringBuffer.samples.fill(0, 0, secondFrames * cc);
  }

  Atomics.store(ringBuffer.writeIndex, 0, write + framesToWrite);
}

/**
 * Initializes audio output. This is expected to be called from the main thread
 * in response to a user gesture (most browsers require user interaction to
 * start audio).
 *
 * If AudioWorklet isn't available, returns a disabled implementation and a
 * message suitable for UI display.
 */
export async function createAudioOutput(options: CreateAudioOutputOptions = {}): Promise<AudioOutput> {
  const AudioContextCtor = getAudioContextCtor();
  if (!AudioContextCtor) {
    return createDisabledAudioOutput({
      message: "Web Audio API is unavailable (AudioContext missing).",
      sampleRate: options.sampleRate ?? 48_000,
    });
  }

  // Treat all host-provided options as untrusted: validate to avoid propagating NaNs/Infs into
  // AudioContext construction or ring buffer sizing.
  const requestedSampleRate = options.sampleRate ?? 48_000;
  const sampleRate = Number.isFinite(requestedSampleRate) && requestedSampleRate > 0 ? requestedSampleRate : 48_000;
  const requestedLatencyHint: unknown = options.latencyHint;
  const latencyHint: AudioContextLatencyCategory | number =
    requestedLatencyHint === "interactive" || requestedLatencyHint === "balanced" || requestedLatencyHint === "playback"
      ? requestedLatencyHint
      : typeof requestedLatencyHint === "number" && Number.isFinite(requestedLatencyHint) && requestedLatencyHint >= 0
        ? requestedLatencyHint
        : "interactive";
  const requestedChannelCount = options.channelCount ?? 2;
  const channelCount =
    Number.isFinite(requestedChannelCount) && requestedChannelCount > 0
      ? Math.max(1, Math.min(2, Math.floor(requestedChannelCount)))
      : 2;
  const startupPrefillFrames = clampFrames(
    finiteNonNegative(options.startupPrefillFrames) ?? 512,
    0,
    Number.MAX_SAFE_INTEGER,
  );
  const discardOnResume = typeof options.discardOnResume === "boolean" ? options.discardOnResume : true;
  const sendUnderrunMessages = options.sendUnderrunMessages === true;
  const underrunMessageIntervalMsRaw = finiteNonNegative(options.underrunMessageIntervalMs);
  const underrunMessageIntervalMs =
    underrunMessageIntervalMsRaw !== undefined && underrunMessageIntervalMsRaw > 0 ? underrunMessageIntervalMsRaw : undefined;

  type AudioContextCtorArgs = [] | [AudioContextOptions];
  const contextAttempts: ReadonlyArray<Readonly<{ label: string; args: AudioContextCtorArgs }>> = [
    {
      label: "new AudioContext({ sampleRate, latencyHint })",
      args: [{ sampleRate, latencyHint }],
    },
    {
      label: "new AudioContext({ latencyHint })",
      args: [{ latencyHint }],
    },
    {
      label: "new AudioContext({ sampleRate })",
      args: [{ sampleRate }],
    },
    {
      label: "new AudioContext()",
      args: [],
    },
  ];

  let context: AudioContext | null = null;
  let lastContextError: unknown = null;
  let lastContextAttemptLabel = contextAttempts[0]?.label ?? "new AudioContext()";
  for (const attempt of contextAttempts) {
    try {
      context = new AudioContextCtor(...attempt.args);
      break;
    } catch (err) {
      lastContextError = err;
      lastContextAttemptLabel = attempt.label;
    }
  }

  if (!context) {
    const detail = formatOneLineError(lastContextError, 256, "");
    const rawMessage = detail
      ? `Failed to create AudioContext (${lastContextAttemptLabel}): ${detail}`
      : `Failed to create AudioContext (${lastContextAttemptLabel}).`;
    return createDisabledAudioOutput({
      message: formatOneLineUtf8(rawMessage, 512) || "Failed to create AudioContext.",
      sampleRate,
    });
  }
  const actualSampleRate = context.sampleRate;

  // Call resume() immediately (before any await) to satisfy autoplay policies.
  //
  // IMPORTANT: Do not permanently bind `output.resume()` to this *first* attempt.
  // Browsers may reject `AudioContext.resume()` when invoked outside a user
  // gesture. We want later calls (e.g. after a click) to retry.
  let resumeInFlight: Promise<void> | null = null;
  const startResumeAttempt = (): Promise<void> => {
    if (context.state === "running") return Promise.resolve();
    if (resumeInFlight) return resumeInFlight;

    const p = context.resume();
    resumeInFlight = p;

    // Always attach a rejection handler so we don't surface unhandled rejections
    // if callers forget to await `output.resume()` or if we early-return due to a
    // subsequent initialization failure.
    void p.catch(() => {});

    // Clear the in-flight promise once it settles so future calls can retry.
    void p.then(
      () => {
        if (resumeInFlight === p) resumeInFlight = null;
      },
      () => {
        if (resumeInFlight === p) resumeInFlight = null;
      },
    );

    return p;
  };

  // Kick off an early resume attempt (best-effort).
  void startResumeAttempt().catch(() => {});

  let ringBuffer: AudioRingBufferLayout;
  let ringBufferFrames: number;
  try {
    ringBufferFrames =
      options.ringBufferFrames ??
      (options.ringBuffer
        ? inferRingBufferFrames(options.ringBuffer, channelCount)
        : getDefaultRingBufferFrames(actualSampleRate));
    ringBuffer = options.ringBuffer
      ? wrapRingBuffer(options.ringBuffer, channelCount, ringBufferFrames)
      : createRingBuffer(channelCount, ringBufferFrames);
  } catch (err) {
    await context.close();
    return createDisabledAudioOutput({
      message: formatOneLineError(err, 512, "Failed to allocate SharedArrayBuffer for audio."),
      sampleRate: actualSampleRate,
    });
  }
  if (!context.audioWorklet || typeof context.audioWorklet.addModule !== "function") {
    await context.close();
    return createDisabledAudioOutput({
      message: "AudioWorklet is unavailable in this browser (AudioContext.audioWorklet missing).",
      ringBuffer,
      sampleRate: actualSampleRate,
    });
  }

  try {
    await context.audioWorklet.addModule(audioWorkletProcessorUrl);
  } catch (err) {
    await context.close();
    const detail = formatOneLineError(err, 512, "");
    const rawMessage = detail ? `Failed to load AudioWorklet module: ${detail}` : "Failed to load AudioWorklet module.";
    return createDisabledAudioOutput({
      message: formatOneLineUtf8(rawMessage, 512) || "Failed to load AudioWorklet module.",
      ringBuffer,
      sampleRate: actualSampleRate,
    });
  }

  let node: AudioWorkletNode | null = null;
  const processorOptions = {
    ringBuffer: ringBuffer.buffer,
    channelCount,
    capacityFrames: ringBufferFrames,
    sendUnderrunMessages,
    underrunMessageIntervalMs,
  };
  const nodeOptionsFull = {
    processorOptions,
    outputChannelCount: [channelCount],
  };
  const nodeOptionsReduced = {
    processorOptions,
  };
  try {
    node = new AudioWorkletNode(context, "aero-audio-processor", nodeOptionsFull);
  } catch (err) {
    try {
      node = new AudioWorkletNode(context, "aero-audio-processor", nodeOptionsReduced);
    } catch (retryErr) {
      await context.close();
      const detail = formatOneLineError(retryErr, 512, "");
      const rawMessage = detail
        ? `Failed to create AudioWorkletNode (without outputChannelCount): ${detail}`
        : "Failed to create AudioWorkletNode (without outputChannelCount).";
      return createDisabledAudioOutput({
        message: formatOneLineUtf8(rawMessage, 512) || "Failed to create AudioWorkletNode (without outputChannelCount).",
        ringBuffer,
        sampleRate: actualSampleRate,
      });
    }
  }
  if (!node) {
    await context.close();
    return createDisabledAudioOutput({
      message: "Failed to create AudioWorkletNode.",
      ringBuffer,
      sampleRate: actualSampleRate,
    });
  }

  // Prefill a small amount of silence to avoid counting an initial underrun
  // between node start and the producer beginning to write samples.
  prefillSilenceIfEmpty(ringBuffer, clampFrames(startupPrefillFrames, 0, ringBuffer.capacityFrames));

  node.connect(context.destination);

  const discardBufferedFrames = () => {
    // MessagePort semantics are a little subtle: when using `addEventListener` the receiver must
    // call `start()`; when using `onmessage` it starts automatically. Calling `start()` here is a
    // harmless no-op in browsers, but makes our control channel robust.
    try {
      node.port.start();
    } catch {
      // Ignore.
    }
    try {
      node.port.postMessage({ type: "ring.reset" });
    } catch {
      // Ignore.
    }
  };

  // If the AudioContext has already reached running at least once, any later transition back to
  // running implies a suspend/resume cycle (tab backgrounding, interruption, etc.). Discard any
  // buffered backlog so playback is "live".
  let onContextStateChange: (() => void) | null = null;
  const canWatchStateChange =
    discardOnResume &&
    typeof (context as unknown as { addEventListener?: unknown }).addEventListener === "function" &&
    typeof (context as unknown as { removeEventListener?: unknown }).removeEventListener === "function";
  if (canWatchStateChange) {
    let hasEverBeenRunning = context.state === "running";
    let lastState: AudioContextState = context.state;
    onContextStateChange = () => {
      const state = context.state;
      if (state === "running") {
        if (hasEverBeenRunning && lastState !== "running") {
          discardBufferedFrames();
        }
        hasEverBeenRunning = true;
      }
      lastState = state;
    };
    context.addEventListener("statechange", onContextStateChange);
  }

  return {
    enabled: true,
    context,
    node,
    ringBuffer,
    resume: startResumeAttempt,
    async close() {
      if (onContextStateChange) {
        // The watcher is only installed when `removeEventListener` exists, but keep this robust
        // for any exotic/stub AudioContext implementations.
        try {
          context.removeEventListener("statechange", onContextStateChange);
        } catch {
          // Ignore.
        }
        onContextStateChange = null;
      }
      try {
        node.disconnect();
      } finally {
        await context.close();
      }
    },
    discardBufferedFrames,
    writeInterleaved(samples: Float32Array, srcSampleRate: number) {
      return writeRingBufferInterleaved(ringBuffer, samples, srcSampleRate, context.sampleRate);
    },
    getBufferLevelFrames() {
      return getRingBufferLevelFrames(ringBuffer);
    },
    getUnderrunCount() {
      return getRingBufferUnderrunCount(ringBuffer);
    },
    getOverrunCount() {
      return getRingBufferOverrunCount(ringBuffer);
    },
    getMetrics() {
      // `baseLatency`/`outputLatency` are optional Web Audio introspection fields (not exposed in
      // all browsers). Read them via a loose type guard so TypeScript builds remain compatible
      // even if the DOM lib definitions change.
      const baseLatencySeconds = finiteNonNegative(
        (context as AudioContext & { baseLatency?: unknown }).baseLatency,
      );
      const outputLatencySeconds = finiteNonNegative(
        (context as AudioContext & { outputLatency?: unknown }).outputLatency,
      );
      return {
        bufferLevelFrames: getRingBufferLevelFrames(ringBuffer),
        capacityFrames: ringBuffer.capacityFrames,
        underrunCount: getRingBufferUnderrunCount(ringBuffer),
        overrunCount: getRingBufferOverrunCount(ringBuffer),
        sampleRate: context.sampleRate,
        state: context.state,
        ...(baseLatencySeconds === undefined ? {} : { baseLatencySeconds }),
        ...(outputLatencySeconds === undefined ? {} : { outputLatencySeconds }),
      };
    },
  };
}

function createDisabledAudioOutput(options: {
  message: string;
  ringBuffer?: AudioRingBufferLayout;
  sampleRate?: number;
}): DisabledAudioOutput {
  const { message, ringBuffer } = options;
  const sampleRate = options.sampleRate ?? 0;
  return {
    enabled: false,
    message,
    ringBuffer,
    async resume() {},
    async close() {},
    writeInterleaved() {
      return 0;
    },
    getBufferLevelFrames() {
      return ringBuffer ? getRingBufferLevelFrames(ringBuffer) : 0;
    },
    getUnderrunCount() {
      return ringBuffer ? getRingBufferUnderrunCount(ringBuffer) : 0;
    },
    getOverrunCount() {
      return ringBuffer ? getRingBufferOverrunCount(ringBuffer) : 0;
    },
    getMetrics() {
      return {
        bufferLevelFrames: ringBuffer ? getRingBufferLevelFrames(ringBuffer) : 0,
        capacityFrames: ringBuffer?.capacityFrames ?? 0,
        underrunCount: ringBuffer ? getRingBufferUnderrunCount(ringBuffer) : 0,
        overrunCount: ringBuffer ? getRingBufferOverrunCount(ringBuffer) : 0,
        sampleRate,
        state: "disabled",
      };
    },
  };
}

export function startAudioPerfSampling(
  output: EnabledAudioOutput,
  perf: { counter(name: string, value: number): void },
  intervalMs = 250,
): () => void {
  let workletUnderrunFrames: number | null = null;

  const onWorkletMessage = (event: MessageEvent) => {
    const data = event.data as unknown;
    if (!data || typeof data !== "object") return;
    const msg = data as { type?: unknown; underrunCount?: unknown; underrunFramesTotal?: unknown };
    if (msg.type !== "underrun") return;
    const total =
      typeof msg.underrunFramesTotal === "number"
        ? msg.underrunFramesTotal
        : typeof msg.underrunCount === "number"
          ? msg.underrunCount
          : null;
    if (total === null) return;
    const next = total >>> 0;
    // Treat the counter as a wrapping u32 value (wraps at 2^32).
    workletUnderrunFrames = next;
  };

  output.node.port.addEventListener("message", onWorkletMessage);
  // Required for MessagePort when using addEventListener (as opposed to `onmessage`).
  output.node.port.start();

  const sample = () => {
    const metrics = output.getMetrics();
    const underrunFrames = workletUnderrunFrames ?? metrics.underrunCount;
    perf.counter("audio.bufferLevelFrames", metrics.bufferLevelFrames);
    perf.counter("audio.underrunFrames", underrunFrames);
    perf.counter("audio.overrunFrames", metrics.overrunCount);
    perf.counter("audio.sampleRate", metrics.sampleRate);

    // These are optional introspection fields; emit only when valid so consumers can surface
    // real output latency in traces/HUDs across browsers.
    const baseLatencySeconds = finiteNonNegative(metrics.baseLatencySeconds);
    if (baseLatencySeconds !== undefined) perf.counter("audio.baseLatencySeconds", baseLatencySeconds);
    const outputLatencySeconds = finiteNonNegative(metrics.outputLatencySeconds);
    if (outputLatencySeconds !== undefined) perf.counter("audio.outputLatencySeconds", outputLatencySeconds);
  };

  sample();
  const intervalId = globalThis.setInterval(sample, intervalMs);
  (intervalId as unknown as { unref?: () => void }).unref?.();
  let stopped = false;
  return () => {
    if (stopped) return;
    stopped = true;
    output.node.port.removeEventListener("message", onWorkletMessage);
    globalThis.clearInterval(intervalId);
  };
}

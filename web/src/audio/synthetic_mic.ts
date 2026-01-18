import {
  createMicRingBuffer,
  DROPPED_SAMPLES_INDEX,
  micRingBufferWrite,
  READ_POS_INDEX,
  WRITE_POS_INDEX,
} from "./mic_ring.js";
import { AudioFrameClock, performanceNowNs } from "./audio_frame_clock";
import { unrefBestEffort } from "../unrefSafe";

export type SyntheticMicSource = {
  ringBuffer: SharedArrayBuffer;
  sampleRate: number;
  stop(): void;
};

export type SyntheticMicOptions = {
  sampleRate?: number;
  bufferMs?: number;
  freqHz?: number;
  gain?: number;
  /**
   * Timer tick interval used to advance the generator, in milliseconds.
   *
   * The generator is time-based (it will generate enough samples to match the
   * configured sample rate), so this does not need to be sample-accurate.
   */
  tickMs?: number;
};

export function startSyntheticMic(options: SyntheticMicOptions = {}): SyntheticMicSource {
  if (typeof SharedArrayBuffer === "undefined") {
    throw new Error("SharedArrayBuffer is required for synthetic mic (crossOriginIsolated).");
  }

  const sampleRate = (options.sampleRate ?? 48_000) | 0;
  if (!Number.isFinite(sampleRate) || sampleRate <= 0) {
    throw new Error(`invalid synthetic mic sampleRate: ${sampleRate}`);
  }
  const bufferMs = Math.max(10, (options.bufferMs ?? 200) | 0);
  const freqHz = options.freqHz ?? 440;
  const gain = options.gain ?? 0.1;
  const tickMs = Math.max(1, (options.tickMs ?? 10) | 0);

  const capacitySamples = Math.max(1, Math.floor((sampleRate * bufferMs) / 1000));
  const rb = createMicRingBuffer(capacitySamples);

  // Reset indices/counters in case the buffer is reused.
  Atomics.store(rb.header, WRITE_POS_INDEX, 0);
  Atomics.store(rb.header, READ_POS_INDEX, 0);
  Atomics.store(rb.header, DROPPED_SAMPLES_INDEX, 0);

  let phase = 0;
  const phaseStep = freqHz / sampleRate;

  const clock = new AudioFrameClock(sampleRate, performanceNowNs());

  // Keep allocations bounded by chunking the time-based generator output.
  const scratch = new Float32Array(Math.max(256, Math.floor(sampleRate / 100))); // ≥10ms at 48k

  const timer = globalThis.setInterval(() => {
    let remaining = clock.advanceTo(performanceNowNs());
    if (remaining <= 0) return;

    while (remaining > 0) {
      const n = Math.min(remaining, scratch.length);
      for (let i = 0; i < n; i++) {
        scratch[i] = Math.sin(phase * 2 * Math.PI) * gain;
        phase += phaseStep;
        if (phase >= 1) phase -= 1;
      }
      micRingBufferWrite(rb, scratch.subarray(0, n));
      remaining -= n;
    }
  }, tickMs);
  unrefBestEffort(timer);

  return {
    ringBuffer: rb.sab,
    sampleRate,
    stop() {
      globalThis.clearInterval(timer);
    },
  };
}

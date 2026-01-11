import { createMicRingBuffer } from "./mic_capture";

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

const MIC_WRITE_POS = 0;
const MIC_READ_POS = 1;
const MIC_DROPPED = 2;

function writeToMicRingBuffer(header: Uint32Array, data: Float32Array, capacity: number, samples: Float32Array): void {
  let writePos = Atomics.load(header, MIC_WRITE_POS) >>> 0;
  const readPos = Atomics.load(header, MIC_READ_POS) >>> 0;

  const used = (writePos - readPos) >>> 0;
  if (used > capacity) {
    // Consumer fell too far behind (or indices got corrupted). Match the
    // MicCapture writer behavior and drop the whole block.
    Atomics.add(header, MIC_DROPPED, samples.length);
    return;
  }

  const free = capacity - used;
  if (free === 0) {
    Atomics.add(header, MIC_DROPPED, samples.length);
    return;
  }

  const toWrite = Math.min(samples.length, free);
  const dropped = samples.length - toWrite;
  if (dropped) Atomics.add(header, MIC_DROPPED, dropped);

  // Keep the most recent portion when under pressure.
  const slice = dropped ? samples.subarray(dropped) : samples;

  const start = writePos % capacity;
  const firstPart = Math.min(toWrite, capacity - start);
  data.set(slice.subarray(0, firstPart), start);
  const remaining = toWrite - firstPart;
  if (remaining) data.set(slice.subarray(firstPart), 0);

  writePos = (writePos + toWrite) >>> 0;
  Atomics.store(header, MIC_WRITE_POS, writePos);
}

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
  Atomics.store(rb.header, MIC_WRITE_POS, 0);
  Atomics.store(rb.header, MIC_READ_POS, 0);
  Atomics.store(rb.header, MIC_DROPPED, 0);

  const header = rb.header;
  const data = rb.data;
  const capacity = rb.capacity;

  let phase = 0;
  const phaseStep = freqHz / sampleRate;

  const startedAtMs = performance.now();
  let producedSamples = 0;

  // Keep allocations bounded by chunking the time-based generator output.
  const scratch = new Float32Array(Math.max(256, Math.floor(sampleRate / 100))); // ≥10ms at 48k

  const timer = globalThis.setInterval(() => {
    const nowMs = performance.now();
    const shouldHaveProduced = Math.floor(((nowMs - startedAtMs) * sampleRate) / 1000);
    let remaining = shouldHaveProduced - producedSamples;
    if (remaining <= 0) return;

    while (remaining > 0) {
      const n = Math.min(remaining, scratch.length);
      for (let i = 0; i < n; i++) {
        scratch[i] = Math.sin(phase * 2 * Math.PI) * gain;
        phase += phaseStep;
        if (phase >= 1) phase -= 1;
      }
      writeToMicRingBuffer(header, data, capacity, scratch.subarray(0, n));
      producedSamples += n;
      remaining -= n;
    }
  }, tickMs);

  return {
    ringBuffer: rb.sab,
    sampleRate,
    stop() {
      globalThis.clearInterval(timer);
    },
  };
}

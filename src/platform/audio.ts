export type CreateAudioOutputOptions = {
  sampleRate?: number;
  latencyHint?: AudioContextLatencyCategory | number;
  channelCount?: number;
  /**
   * Size of the ring buffer in frames (per channel).
   *
   * The actual sample capacity is `ringBufferFrames * channelCount`.
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

export type AudioRingBufferLayout = {
  /**
   * Shared ring buffer used for inter-thread audio sample transport.
   *
   * Layout (little-endian):
   * - u32 readFrameIndex (bytes 0..4)
   * - u32 writeFrameIndex (bytes 4..8)
   * - u32 underrunCount (bytes 8..12)
   * - u32 overrunCount (bytes 12..16) - frames dropped by the producer due to buffer full
   * - f32 samples[] (bytes 16..)
   *
   * Indices are monotonically-increasing frame counters (wrapping naturally at
   * 2^32). The producer writes samples first, then atomically advances
   * `writeFrameIndex`. The consumer reads available frames (clamped to the
   * configured capacity) and then atomically advances `readFrameIndex`.
   */
  buffer: SharedArrayBuffer;
  header: Uint32Array;
  readIndex: Uint32Array;
  writeIndex: Uint32Array;
  underrunCount: Uint32Array;
  overrunCount: Uint32Array;
  samples: Float32Array;
  channelCount: number;
  capacityFrames: number;
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
   * Write interleaved `f32` samples into the ring buffer.
   *
   * `srcSampleRate` is used for naive linear resampling if it differs from the
   * AudioContext's sample rate.
   */
  writeInterleaved(samples: Float32Array, srcSampleRate: number): number;
  getBufferLevelFrames(): number;
  getUnderrunCount(): number;
  getOverrunCount(): number;
};

export type DisabledAudioOutput = {
  enabled: false;
  message: string;
  ringBuffer?: AudioRingBufferLayout;
  resume(): Promise<void>;
  close(): Promise<void>;
  writeInterleaved(_samples: Float32Array, _srcSampleRate: number): number;
  getBufferLevelFrames(): number;
  getUnderrunCount(): number;
  getOverrunCount(): number;
};

export type AudioOutput = EnabledAudioOutput | DisabledAudioOutput;

function getAudioContextCtor(): typeof AudioContext | undefined {
  return (
    (globalThis as typeof globalThis & { webkitAudioContext?: typeof AudioContext }).AudioContext ??
    (globalThis as typeof globalThis & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  );
}

function createRingBuffer(channelCount: number, ringBufferFrames: number): AudioRingBufferLayout {
  if (typeof SharedArrayBuffer === 'undefined') {
    throw new Error('SharedArrayBuffer is required for Aero audio ring buffers.');
  }

  const sampleCapacity = ringBufferFrames * channelCount;
  const headerU32Len = 4;
  const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT;
  const buffer = new SharedArrayBuffer(headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);
  const header = new Uint32Array(buffer, 0, headerU32Len);
  const samples = new Float32Array(buffer, headerBytes, sampleCapacity);

  Atomics.store(header, 0, 0);
  Atomics.store(header, 1, 0);
  Atomics.store(header, 2, 0);
  Atomics.store(header, 3, 0);

  return {
    buffer,
    header,
    readIndex: header.subarray(0, 1),
    writeIndex: header.subarray(1, 2),
    underrunCount: header.subarray(2, 3),
    overrunCount: header.subarray(3, 4),
    samples,
    channelCount,
    capacityFrames: ringBufferFrames,
  };
}

function wrapRingBuffer(buffer: SharedArrayBuffer, channelCount: number, ringBufferFrames: number): AudioRingBufferLayout {
  const headerU32Len = 4;
  const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT;
  const sampleCapacity = ringBufferFrames * channelCount;
  const requiredBytes = headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT;
  if (buffer.byteLength < requiredBytes) {
    throw new Error(`Provided ring buffer is too small: need ${requiredBytes} bytes, got ${buffer.byteLength} bytes`);
  }

  const header = new Uint32Array(buffer, 0, headerU32Len);
  const samples = new Float32Array(buffer, headerBytes, sampleCapacity);

  return {
    buffer,
    header,
    readIndex: header.subarray(0, 1),
    writeIndex: header.subarray(1, 2),
    underrunCount: header.subarray(2, 3),
    overrunCount: header.subarray(3, 4),
    samples,
    channelCount,
    capacityFrames: ringBufferFrames,
  };
}

function inferRingBufferFrames(buffer: SharedArrayBuffer, channelCount: number): number {
  const headerBytes = 4 * Uint32Array.BYTES_PER_ELEMENT;
  const payloadBytes = buffer.byteLength - headerBytes;
  if (payloadBytes < 0 || payloadBytes % Float32Array.BYTES_PER_ELEMENT !== 0) {
    throw new Error('Provided ring buffer has an invalid byte length.');
  }
  const sampleCapacity = payloadBytes / Float32Array.BYTES_PER_ELEMENT;
  if (sampleCapacity % channelCount !== 0) {
    throw new Error('Provided ring buffer payload is not aligned to the requested channelCount.');
  }
  return sampleCapacity / channelCount;
}

const READ_FRAME_INDEX = 0;
const WRITE_FRAME_INDEX = 1;
const UNDERRUN_COUNT = 2;
const OVERRUN_COUNT = 3;

function framesAvailable(readFrameIndex: number, writeFrameIndex: number): number {
  return (writeFrameIndex - readFrameIndex) >>> 0;
}

function framesAvailableClamped(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number {
  return Math.min(framesAvailable(readFrameIndex, writeFrameIndex), capacityFrames);
}

function framesFree(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number {
  return capacityFrames - framesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames);
}

export function getRingBufferLevelFrames(ringBuffer: AudioRingBufferLayout): number {
  const read = Atomics.load(ringBuffer.header, READ_FRAME_INDEX) >>> 0;
  const write = Atomics.load(ringBuffer.header, WRITE_FRAME_INDEX) >>> 0;
  return framesAvailableClamped(read, write, ringBuffer.capacityFrames);
}

export function getRingBufferUnderrunCount(ringBuffer: AudioRingBufferLayout): number {
  return Atomics.load(ringBuffer.header, UNDERRUN_COUNT) >>> 0;
}

export function getRingBufferOverrunCount(ringBuffer: AudioRingBufferLayout): number {
  return Atomics.load(ringBuffer.header, OVERRUN_COUNT) >>> 0;
}

export function resampleLinearInterleaved(
  input: Float32Array,
  channelCount: number,
  srcRate: number,
  dstRate: number,
): Float32Array {
  if (!Number.isFinite(srcRate) || !Number.isFinite(dstRate) || srcRate <= 0 || dstRate <= 0) {
    return new Float32Array();
  }
  if (srcRate === dstRate) return input;

  const srcFrames = Math.floor(input.length / channelCount);
  if (srcFrames === 0) return new Float32Array();

  const ratio = dstRate / srcRate;
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

export function writeRingBufferInterleaved(
  ringBuffer: AudioRingBufferLayout,
  input: Float32Array,
  srcSampleRate: number,
  dstSampleRate: number,
): number {
  const samples =
    srcSampleRate === dstSampleRate
      ? input
      : resampleLinearInterleaved(input, ringBuffer.channelCount, srcSampleRate, dstSampleRate);

  const requestedFrames = Math.floor(samples.length / ringBuffer.channelCount);
  if (requestedFrames === 0) return 0;

  const read = Atomics.load(ringBuffer.header, READ_FRAME_INDEX) >>> 0;
  const write = Atomics.load(ringBuffer.header, WRITE_FRAME_INDEX) >>> 0;

  const free = framesFree(read, write, ringBuffer.capacityFrames);
  const framesToWrite = Math.min(requestedFrames, free);
  if (framesToWrite < requestedFrames) {
    Atomics.add(ringBuffer.header, OVERRUN_COUNT, requestedFrames - framesToWrite);
  }
  if (framesToWrite === 0) return 0;

  const writePos = write % ringBuffer.capacityFrames;
  const firstFrames = Math.min(framesToWrite, ringBuffer.capacityFrames - writePos);
  const secondFrames = framesToWrite - firstFrames;

  const cc = ringBuffer.channelCount;
  const firstSamples = firstFrames * cc;
  const secondSamples = secondFrames * cc;

  ringBuffer.samples.set(samples.subarray(0, firstSamples), writePos * cc);
  if (secondFrames > 0) {
    ringBuffer.samples.set(samples.subarray(firstSamples, firstSamples + secondSamples), 0);
  }

  Atomics.store(ringBuffer.header, WRITE_FRAME_INDEX, write + framesToWrite);
  return framesToWrite;
}

function prefillSilenceIfEmpty(ringBuffer: AudioRingBufferLayout, frames: number): void {
  if (frames <= 0) return;

  const read = Atomics.load(ringBuffer.header, READ_FRAME_INDEX) >>> 0;
  const write = Atomics.load(ringBuffer.header, WRITE_FRAME_INDEX) >>> 0;
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

  Atomics.store(ringBuffer.header, WRITE_FRAME_INDEX, write + framesToWrite);
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
    return {
      enabled: false,
      message: 'Web Audio API is unavailable (AudioContext missing).',
      async resume() {},
      async close() {},
      writeInterleaved() {
        return 0;
      },
      getBufferLevelFrames() {
        return 0;
      },
      getUnderrunCount() {
        return 0;
      },
      getOverrunCount() {
        return 0;
      },
    };
  }

  const sampleRate = options.sampleRate ?? 48_000;
  const latencyHint = options.latencyHint ?? 'interactive';
  const channelCount = options.channelCount ?? 2;
  const ringBufferFrames =
    options.ringBufferFrames ??
    (options.ringBuffer ? inferRingBufferFrames(options.ringBuffer, channelCount) : sampleRate); // ~1 second by default

  let ringBuffer: AudioRingBufferLayout;
  try {
    ringBuffer = options.ringBuffer
      ? wrapRingBuffer(options.ringBuffer, channelCount, ringBufferFrames)
      : createRingBuffer(channelCount, ringBufferFrames);
  } catch (err) {
    return {
      enabled: false,
      message: err instanceof Error ? err.message : 'Failed to allocate SharedArrayBuffer for audio.',
      async resume() {},
      async close() {},
      writeInterleaved() {
        return 0;
      },
      getBufferLevelFrames() {
        return 0;
      },
      getUnderrunCount() {
        return 0;
      },
      getOverrunCount() {
        return 0;
      },
    };
  }

  const context = new AudioContextCtor({
    sampleRate,
    latencyHint,
  });

  // Call resume() immediately (before any await) to satisfy autoplay policies.
  const resumePromise = context.resume();

  if (!context.audioWorklet || typeof context.audioWorklet.addModule !== 'function') {
    await context.close();
    return {
      enabled: false,
      message: 'AudioWorklet is unavailable in this browser (AudioContext.audioWorklet missing).',
      ringBuffer,
      async resume() {},
      async close() {},
      writeInterleaved() {
        return 0;
      },
      getBufferLevelFrames() {
        return 0;
      },
      getUnderrunCount() {
        return 0;
      },
      getOverrunCount() {
        return 0;
      },
    };
  }

  try {
    await context.audioWorklet.addModule(new URL('./audio-worklet-processor.js', import.meta.url));
  } catch (err) {
    await context.close();
    return {
      enabled: false,
      message:
        err instanceof Error ? `Failed to load AudioWorklet module: ${err.message}` : 'Failed to load AudioWorklet module.',
      ringBuffer,
      async resume() {},
      async close() {},
      writeInterleaved() {
        return 0;
      },
      getBufferLevelFrames() {
        return 0;
      },
      getUnderrunCount() {
        return 0;
      },
      getOverrunCount() {
        return 0;
      },
    };
  }

  let node: AudioWorkletNode;
  try {
    node = new AudioWorkletNode(context, 'aero-audio-processor', {
      processorOptions: {
        ringBuffer: ringBuffer.buffer,
        channelCount,
        capacityFrames: ringBufferFrames,
      },
      outputChannelCount: [channelCount],
    });
  } catch (err) {
    await context.close();
    return {
      enabled: false,
      message: err instanceof Error ? `Failed to create AudioWorkletNode: ${err.message}` : 'Failed to create AudioWorkletNode.',
      ringBuffer,
      async resume() {},
      async close() {},
      writeInterleaved() {
        return 0;
      },
      getBufferLevelFrames() {
        return 0;
      },
      getUnderrunCount() {
        return 0;
      },
      getOverrunCount() {
        return 0;
      },
    };
  }

  // Prefill a small amount of silence to avoid counting an initial underrun
  // between node start and the producer beginning to write samples.
  prefillSilenceIfEmpty(ringBuffer, 512);

  node.connect(context.destination);

  return {
    enabled: true,
    context,
    node,
    ringBuffer,
    async resume() {
      await resumePromise;
    },
    async close() {
      try {
        node.disconnect();
      } finally {
        await context.close();
      }
    },
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
  };
}

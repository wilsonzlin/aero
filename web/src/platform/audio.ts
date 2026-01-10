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
};

export type AudioRingBufferLayout = {
  /**
   * Shared ring buffer used for inter-thread audio sample transport.
   *
   * Layout (little-endian):
   * - u32 readIndex (bytes 0..4)
   * - u32 writeIndex (bytes 4..8)
   * - f32 samples[] (bytes 8..)
   *
   * Indices are in units of samples (not frames) and wrap modulo samples.length.
   */
  buffer: SharedArrayBuffer;
  readIndex: Uint32Array;
  writeIndex: Uint32Array;
  samples: Float32Array;
};

export type EnabledAudioOutput = {
  enabled: true;
  message?: string;
  context: AudioContext;
  node: AudioWorkletNode;
  ringBuffer: AudioRingBufferLayout;
  resume(): Promise<void>;
  close(): Promise<void>;
};

export type DisabledAudioOutput = {
  enabled: false;
  message: string;
  ringBuffer?: AudioRingBufferLayout;
  resume(): Promise<void>;
  close(): Promise<void>;
};

export type AudioOutput = EnabledAudioOutput | DisabledAudioOutput;

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

  const sampleCapacity = ringBufferFrames * channelCount;
  const headerBytes = 8;
  const buffer = new SharedArrayBuffer(headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);
  const indices = new Uint32Array(buffer, 0, 2);
  const samples = new Float32Array(buffer, headerBytes, sampleCapacity);

  return {
    buffer,
    readIndex: indices.subarray(0, 1),
    writeIndex: indices.subarray(1, 2),
    samples,
  };
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
      message: "Web Audio API is unavailable (AudioContext missing).",
      async resume() {},
      async close() {},
    };
  }

  const sampleRate = options.sampleRate ?? 48_000;
  const latencyHint = options.latencyHint ?? "interactive";
  const channelCount = options.channelCount ?? 2;
  const ringBufferFrames = options.ringBufferFrames ?? sampleRate; // ~1 second by default

  let ringBuffer: AudioRingBufferLayout;
  try {
    ringBuffer = createRingBuffer(channelCount, ringBufferFrames);
  } catch (err) {
    return {
      enabled: false,
      message: err instanceof Error ? err.message : "Failed to allocate SharedArrayBuffer for audio.",
      async resume() {},
      async close() {},
    };
  }

  const context = new AudioContextCtor({
    sampleRate,
    latencyHint,
  });

  if (!context.audioWorklet || typeof context.audioWorklet.addModule !== "function") {
    await context.close();
    return {
      enabled: false,
      message: "AudioWorklet is unavailable in this browser (AudioContext.audioWorklet missing).",
      ringBuffer,
      async resume() {},
      async close() {},
    };
  }

  try {
    await context.audioWorklet.addModule(new URL("./audio-worklet-processor.js", import.meta.url));
  } catch (err) {
    await context.close();
    return {
      enabled: false,
      message:
        err instanceof Error
          ? `Failed to load AudioWorklet module: ${err.message}`
          : "Failed to load AudioWorklet module.",
      ringBuffer,
      async resume() {},
      async close() {},
    };
  }

  let node: AudioWorkletNode;
  try {
    node = new AudioWorkletNode(context, "aero-audio-processor", {
      processorOptions: { ringBuffer: ringBuffer.buffer },
      outputChannelCount: [channelCount],
    });
  } catch (err) {
    await context.close();
    return {
      enabled: false,
      message:
        err instanceof Error
          ? `Failed to create AudioWorkletNode: ${err.message}`
          : "Failed to create AudioWorkletNode.",
      ringBuffer,
      async resume() {},
      async close() {},
    };
  }

  node.connect(context.destination);

  return {
    enabled: true,
    context,
    node,
    ringBuffer,
    async resume() {
      await context.resume();
    },
    async close() {
      try {
        node.disconnect();
      } finally {
        await context.close();
      }
    },
  };
}


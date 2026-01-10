class AeroAudioProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();

    const ringBuffer = options?.processorOptions?.ringBuffer;
    if (ringBuffer instanceof SharedArrayBuffer) {
      this._indices = new Uint32Array(ringBuffer, 0, 2);
      this._samples = new Float32Array(ringBuffer, 8);
    } else {
      this._indices = null;
      this._samples = null;
    }
  }

  /**
   * Ring buffer layout is described in `src/platform/audio.ts`.
   *
   * Samples are interleaved by channel: L0, R0, L1, R1, ...
   */
  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output) return true;

    if (!this._indices || !this._samples) {
      for (let c = 0; c < output.length; c++) output[c].fill(0);
      return true;
    }

    const channelCount = output.length;
    const frames = output[0]?.length ?? 0;
    const samplesNeeded = frames * channelCount;

    const samples = this._samples;
    const capacity = samples.length;

    let readIndex = Atomics.load(this._indices, 0) % capacity;
    const writeIndex = Atomics.load(this._indices, 1) % capacity;
    const available = (writeIndex - readIndex + capacity) % capacity;

    if (available < samplesNeeded) {
      for (let c = 0; c < output.length; c++) output[c].fill(0);
      return true;
    }

    for (let i = 0; i < frames; i++) {
      for (let c = 0; c < channelCount; c++) {
        output[c][i] = samples[readIndex];
        readIndex = (readIndex + 1) % capacity;
      }
    }

    Atomics.store(this._indices, 0, readIndex);
    return true;
  }
}

registerProcessor('aero-audio-processor', AeroAudioProcessor);


const READ_FRAME_INDEX = 0;
const WRITE_FRAME_INDEX = 1;
const UNDERRUN_COUNT = 2;

function framesAvailable(readFrameIndex, writeFrameIndex) {
  return (writeFrameIndex - readFrameIndex) >>> 0;
}

class AeroAudioProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();

    const ringBuffer = options?.processorOptions?.ringBuffer;
    const channelCount = options?.processorOptions?.channelCount;
    const capacityFrames = options?.processorOptions?.capacityFrames;

    if (ringBuffer instanceof SharedArrayBuffer) {
      // Layout is described in `web/src/platform/audio.ts`.
      this._header = new Uint32Array(ringBuffer, 0, 4);
      this._samples = new Float32Array(ringBuffer, 16);
      this._channelCount = typeof channelCount === "number" ? channelCount : null;
      this._capacityFrames = typeof capacityFrames === "number" ? capacityFrames : null;
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

    if (!this._header || !this._samples) {
      for (let c = 0; c < output.length; c++) output[c].fill(0);
      return true;
    }

    const channelCount = Math.min(this._channelCount ?? output.length, output.length);
    const framesNeeded = output[0]?.length ?? 0;
    const capacityFrames = this._capacityFrames ?? Math.floor(this._samples.length / channelCount);

    const readFrameIndex = Atomics.load(this._header, READ_FRAME_INDEX) >>> 0;
    const writeFrameIndex = Atomics.load(this._header, WRITE_FRAME_INDEX) >>> 0;
    const available = Math.min(framesAvailable(readFrameIndex, writeFrameIndex), capacityFrames);
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

    // Zero-fill any missing frames (underrun).
    if (framesToRead < framesNeeded) {
      for (let c = 0; c < output.length; c++) {
        output[c].fill(0, framesToRead);
      }
      const newCount = Atomics.add(this._header, UNDERRUN_COUNT, 1) + 1;
      this.port.postMessage({ type: "underrun", underrunCount: newCount });
    }

    if (framesToRead > 0) {
      Atomics.store(this._header, READ_FRAME_INDEX, readFrameIndex + framesToRead);
    }

    return true;
  }
}

registerProcessor("aero-audio-processor", AeroAudioProcessor);

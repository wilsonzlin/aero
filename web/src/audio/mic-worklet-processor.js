// Runs in the AudioWorklet global scope.
//
// Captures microphone PCM frames from the input and writes them into a
// SharedArrayBuffer ring buffer for consumption by the emulator worker.
//
// Ring buffer layout (little-endian):
//   u32[0] write_pos: total samples written (monotonic, wraps at 2^32)
//   u32[1] read_pos:  total samples read by consumer
//   u32[2] dropped:   total samples dropped due to buffer full
//   u32[3] capacity:  number of samples in the data section (constant)
//   f32[] data: PCM samples (mono) written at index (write_pos % capacity)
//
// The buffer is single-producer (this worklet) / single-consumer (emulator
// worker). When full, this producer drops the oldest part of the current block
// (keeps the most recent samples) to bias for low latency.

import {
  CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX,
  HEADER_BYTES,
  HEADER_U32_LEN,
  micRingBufferWrite,
  READ_POS_INDEX,
  samplesAvailable,
  WRITE_POS_INDEX,
} from "./mic_ring.js";

class AeroMicCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const ringBuffer = options?.processorOptions?.ringBuffer;
    if (!(ringBuffer instanceof SharedArrayBuffer)) {
      throw new Error("AeroMicCaptureProcessor requires processorOptions.ringBuffer (SharedArrayBuffer)");
    }

    this._header = new Uint32Array(ringBuffer, 0, HEADER_U32_LEN);
    this._data = new Float32Array(ringBuffer, HEADER_BYTES);
    const headerCap = Atomics.load(this._header, CAPACITY_SAMPLES_INDEX) >>> 0;
    if (headerCap && headerCap !== this._data.length) {
      throw new Error("Mic ring buffer capacity does not match SharedArrayBuffer size");
    }
    this._capacity = headerCap || this._data.length;
    this._rb = { header: this._header, data: this._data, capacity: this._capacity };

    this._muted = false;
    this._statsCounter = 0;
    this._mixBuf = null;

    this.port.onmessage = (event) => {
      const msg = event.data;
      if (!msg || typeof msg !== "object") return;
      if (msg.type === "set_muted") {
        this._muted = !!msg.muted;
      }
    };
  }

  process(inputs, outputs) {
    const input = inputs[0];
    if (!input || input.length === 0) {
      this._zeroOutputs(outputs);
      return true;
    }

    const frames = input[0].length;
    if (frames === 0) {
      this._zeroOutputs(outputs);
      return true;
    }

    // Downmix to mono if needed.
    let mono;
    if (input.length === 1) {
      mono = input[0];
    } else {
      if (!this._mixBuf || this._mixBuf.length < frames) {
        this._mixBuf = new Float32Array(frames);
      }
      mono = this._mixBuf.subarray(0, frames);
      const inv = 1 / input.length;
      for (let i = 0; i < frames; i++) {
        let acc = 0;
        for (let ch = 0; ch < input.length; ch++) acc += input[ch][i];
        mono[i] = acc * inv;
      }
    }

    if (!this._muted) {
      this._writeIntoRing(mono);
    }

    // Keep the node pullable but never leak mic audio to speakers. A downstream
    // GainNode with gain=0 is expected on the main thread; still, we also zero
    // here to be safe.
    this._zeroOutputs(outputs);

    // Occasionally report buffered sample count for UI/debugging. Posting every
    // render quantum is expensive.
    if ((this._statsCounter++ & 0x3f) === 0) {
      const writePos = Atomics.load(this._header, WRITE_POS_INDEX) >>> 0;
      const readPos = Atomics.load(this._header, READ_POS_INDEX) >>> 0;
      const buffered = samplesAvailable(readPos, writePos);
      this.port.postMessage({
        type: "stats",
        buffered,
        dropped: Atomics.load(this._header, DROPPED_SAMPLES_INDEX) >>> 0,
      });
    }

    return true;
  }

  _writeIntoRing(samples) {
    micRingBufferWrite(this._rb, samples);
  }

  _zeroOutputs(outputs) {
    if (!outputs || outputs.length === 0) return;
    const out = outputs[0];
    for (let ch = 0; ch < out.length; ch++) out[ch].fill(0);
  }
}

registerProcessor("aero-mic-capture", AeroMicCaptureProcessor);

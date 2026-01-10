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

const HEADER_U32 = 4;
const HEADER_BYTES = HEADER_U32 * 4;

class AeroMicCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const ringBuffer = options?.processorOptions?.ringBuffer;
    if (!(ringBuffer instanceof SharedArrayBuffer)) {
      throw new Error("AeroMicCaptureProcessor requires processorOptions.ringBuffer (SharedArrayBuffer)");
    }

    this._header = new Uint32Array(ringBuffer, 0, HEADER_U32);
    this._data = new Float32Array(ringBuffer, HEADER_BYTES);
    this._capacity = this._header[3] || this._data.length;

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
      const writePos = Atomics.load(this._header, 0) >>> 0;
      const readPos = Atomics.load(this._header, 1) >>> 0;
      const buffered = (writePos - readPos) >>> 0;
      this.port.postMessage({
        type: "stats",
        buffered,
        dropped: Atomics.load(this._header, 2) >>> 0,
      });
    }

    return true;
  }

  _writeIntoRing(samples) {
    let writePos = Atomics.load(this._header, 0) >>> 0;
    const readPos = Atomics.load(this._header, 1) >>> 0;

    let used = (writePos - readPos) >>> 0;
    if (used > this._capacity) {
      // Consumer fell behind far enough that we no longer know what's valid.
      // Drop this block to avoid making things worse.
      Atomics.add(this._header, 2, samples.length);
      return;
    }

    const free = this._capacity - used;
    if (free === 0) {
      Atomics.add(this._header, 2, samples.length);
      return;
    }

    const toWrite = Math.min(samples.length, free);
    const dropped = samples.length - toWrite;
    if (dropped) Atomics.add(this._header, 2, dropped);

    // Keep the most recent part of the block if we have to drop.
    const slice = dropped ? samples.subarray(dropped) : samples;

    const start = writePos % this._capacity;
    const firstPart = Math.min(toWrite, this._capacity - start);
    this._data.set(slice.subarray(0, firstPart), start);
    const remaining = toWrite - firstPart;
    if (remaining) {
      this._data.set(slice.subarray(firstPart), 0);
    }

    writePos = (writePos + toWrite) >>> 0;
    Atomics.store(this._header, 0, writePos);
  }

  _zeroOutputs(outputs) {
    if (!outputs || outputs.length === 0) return;
    const out = outputs[0];
    for (let ch = 0; ch < out.length; ch++) out[ch].fill(0);
  }
}

registerProcessor("aero-mic-capture", AeroMicCaptureProcessor);

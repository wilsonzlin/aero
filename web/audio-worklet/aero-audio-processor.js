// Runs in the AudioWorklet global scope.
//
// The emulator writes interleaved stereo f32 frames into a SharedArrayBuffer
// ring buffer; this worklet consumes them with Atomics-synchronized indices for
// low-latency playback.
//
// SharedArrayBuffer layout:
//   Int32Array header (4 elements):
//     [0] writeIndexFrames (u32 counter, producer-owned; updated with Atomics.store)
//     [1] readIndexFrames  (u32 counter, consumer-owned; updated with Atomics.store)
//     [2] underrunFrames   (u32 counter, consumer-owned; Atomics.add)
//     [3] overrunFrames    (u32 counter, producer-owned; optional)
//   Float32Array data (interleaved stereo): capacityFrames * channels elements.

class AeroAudioProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._header = null;
    this._data = null;
    this._capacityFrames = 0;
    this._mask = 0;
    this._channels = 2;
    this._telemetryCountdown = 0;

    this.port.onmessage = (event) => {
      const msg = event.data;
      if (!msg || msg.type !== "init") return;
      const sab = msg.sab;
      this._channels = msg.channels ?? 2;
      this._capacityFrames = msg.capacityFrames >>> 0;
      if ((this._capacityFrames & (this._capacityFrames - 1)) !== 0) {
        throw new Error("capacityFrames must be a power of two");
      }
      this._mask = this._capacityFrames - 1;
      this._header = new Int32Array(sab, 0, 4);
      this._data = new Float32Array(sab, 16);
      this._telemetryCountdown = 0;
    };
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    const frames = output[0].length;

    if (!this._data) {
      // Not initialized yet: output silence.
      for (let ch = 0; ch < output.length; ch++) output[ch].fill(0);
      return true;
    }

    let write = Atomics.load(this._header, 0) >>> 0;
    let read = Atomics.load(this._header, 1) >>> 0;
    let available = (write - read) >>> 0;

    let underrun = 0;
    for (let i = 0; i < frames; i++) {
      if (available === 0) {
        output[0][i] = 0;
        if (output.length > 1) output[1][i] = 0;
        underrun++;
        continue;
      }

      const frameIdx = read & this._mask;
      const base = frameIdx * this._channels;
      output[0][i] = this._data[base + 0];
      if (output.length > 1) output[1][i] = this._data[base + 1];

      read = (read + 1) >>> 0;
      available--;
    }

    Atomics.store(this._header, 1, read | 0);
    if (underrun !== 0) Atomics.add(this._header, 2, underrun);

    // Lightweight telemetry (avoid spamming the main thread).
    if (this._telemetryCountdown === 0) {
      const writeNow = Atomics.load(this._header, 0) >>> 0;
      const readNow = Atomics.load(this._header, 1) >>> 0;
      const level = (writeNow - readNow) >>> 0;
      this.port.postMessage({ type: "bufferLevel", availableFrames: level });
      this._telemetryCountdown = 20;
    } else {
      this._telemetryCountdown--;
    }

    return true;
  }
}

registerProcessor("aero-audio-processor", AeroAudioProcessor);


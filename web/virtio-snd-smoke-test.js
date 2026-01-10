const logEl = document.getElementById("log");
const playBtn = document.getElementById("play");

function log(msg) {
  logEl.textContent += `${msg}\n`;
}

function makeRingBuffer(capacitySamples) {
  const headerBytes = 8;
  const sab = new SharedArrayBuffer(headerBytes + capacitySamples * 4);
  const indices = new Uint32Array(sab, 0, 2);
  const samples = new Float32Array(sab, headerBytes, capacitySamples);
  Atomics.store(indices, 0, 0);
  Atomics.store(indices, 1, 0);
  return { sab, indices, samples, capacitySamples };
}

function generateStereoS16Tone({ frames, sampleRate, hz }) {
  const samples = new Int16Array(frames * 2);
  for (let i = 0; i < frames; i++) {
    const t = i / sampleRate;
    const s = Math.sin(2 * Math.PI * hz * t);
    const v = Math.max(-1, Math.min(1, s)) * 0x7fff;
    samples[i * 2 + 0] = v;
    samples[i * 2 + 1] = v;
  }
  return samples;
}

function convertS16ToF32(samples) {
  const out = new Float32Array(samples.length);
  for (let i = 0; i < samples.length; i++) {
    out[i] = samples[i] / 32768.0;
  }
  return out;
}

playBtn.addEventListener("click", async () => {
  playBtn.disabled = true;
  try {
    const ctx = new AudioContext({ sampleRate: 48000, latencyHint: "interactive" });
    await ctx.audioWorklet.addModule("./src/platform/audio-worklet-processor.js");

    const frames = 48000;
    const channelCount = 2;
    const totalSamples = frames * channelCount;
    const rb = makeRingBuffer(totalSamples + 1024);

    const node = new AudioWorkletNode(ctx, "aero-audio-processor", {
      processorOptions: { ringBuffer: rb.sab },
      outputChannelCount: [channelCount],
    });
    node.connect(ctx.destination);

    const toneS16 = generateStereoS16Tone({ frames, sampleRate: 48000, hz: 440 });
    const toneF32 = convertS16ToF32(toneS16);
    rb.samples.set(toneF32);
    Atomics.store(rb.indices, 0, 0);
    Atomics.store(rb.indices, 1, totalSamples);
    log(`Wrote: ${totalSamples} samples into Float32 ring buffer`);

    await ctx.resume();
    log("Playing...");
  } catch (e) {
    log(`Error: ${e?.stack ?? e}`);
    playBtn.disabled = false;
  }
});

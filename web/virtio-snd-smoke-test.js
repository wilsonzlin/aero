const logEl = document.getElementById("log");
const playBtn = document.getElementById("play");

function log(msg) {
  logEl.textContent += `${msg}\n`;
}

function makeRingBuffer(capacityFrames, channelCount) {
  const headerU32Len = 4;
  const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT; // 16
  const sampleCapacity = capacityFrames * channelCount;
  const sab = new SharedArrayBuffer(headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);
  const header = new Uint32Array(sab, 0, headerU32Len);
  const samples = new Float32Array(sab, headerBytes, sampleCapacity);

  Atomics.store(header, 0, 0);
  Atomics.store(header, 1, 0);
  Atomics.store(header, 2, 0);
  Atomics.store(header, 3, 0);

  return { sab, header, samples, capacityFrames, channelCount };
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
    const capacityFrames = frames + 1024;
    const totalSamples = frames * channelCount;
    const rb = makeRingBuffer(capacityFrames, channelCount);

    const node = new AudioWorkletNode(ctx, "aero-audio-processor", {
      processorOptions: { ringBuffer: rb.sab, channelCount, capacityFrames },
      outputChannelCount: [channelCount],
    });
    node.connect(ctx.destination);
    node.port.onmessage = (event) => {
      if (event.data?.type === "underrun") {
        const added = event.data.underrunFramesAdded ?? null;
        const total = event.data.underrunFramesTotal ?? event.data.underrunCount;
        log(`underrunFramesAdded=${added} underrunFramesTotal=${total}`);
      }
    };

    const toneS16 = generateStereoS16Tone({ frames, sampleRate: 48000, hz: 440 });
    const toneF32 = convertS16ToF32(toneS16);
    rb.samples.set(toneF32);
    Atomics.store(rb.header, 0, 0);
    Atomics.store(rb.header, 1, frames);
    log(`Wrote: ${frames} frames (${totalSamples} samples) into Float32 ring buffer`);

    await ctx.resume();
    log("Playing...");
  } catch (e) {
    log(`Error: ${e?.stack ?? e}`);
    playBtn.disabled = false;
  }
});

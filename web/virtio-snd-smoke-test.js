import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
} from "./src/platform/audio_worklet_ring_layout.js";

const logEl = document.getElementById("log");
const playBtn = document.getElementById("play");

function log(msg) {
  logEl.textContent += `${msg}\n`;
}

function makeRingBuffer(capacityFrames, channelCount) {
  const sampleCapacity = capacityFrames * channelCount;
  const sab = new SharedArrayBuffer(HEADER_BYTES + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);
  const header = new Uint32Array(sab, 0, HEADER_U32_LEN);
  const samples = new Float32Array(sab, HEADER_BYTES, sampleCapacity);

  Atomics.store(header, READ_FRAME_INDEX, 0);
  Atomics.store(header, WRITE_FRAME_INDEX, 0);
  Atomics.store(header, UNDERRUN_COUNT_INDEX, 0);
  Atomics.store(header, OVERRUN_COUNT_INDEX, 0);

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

function resampleInterleavedStereoF32(samples, srcSampleRate, dstSampleRate) {
  if (srcSampleRate === dstSampleRate) return samples;
  const channels = 2;
  const srcFrames = Math.floor(samples.length / channels);
  if (srcFrames === 0) return new Float32Array();
  const dstFrames = Math.round((srcFrames * dstSampleRate) / srcSampleRate);
  const out = new Float32Array(dstFrames * channels);
  const step = srcSampleRate / dstSampleRate;

  for (let i = 0; i < dstFrames; i++) {
    const pos = i * step;
    const idx = Math.floor(pos);
    const frac = pos - idx;
    const aIdx = Math.min(idx, srcFrames - 1);
    const bIdx = Math.min(idx + 1, srcFrames - 1);

    const aOff = aIdx * channels;
    const bOff = bIdx * channels;
    const outOff = i * channels;

    if (frac === 0 || aIdx === bIdx) {
      out[outOff] = samples[aOff];
      out[outOff + 1] = samples[aOff + 1];
      continue;
    }

    const aL = samples[aOff];
    const aR = samples[aOff + 1];
    const bL = samples[bOff];
    const bR = samples[bOff + 1];
    out[outOff] = aL + (bL - aL) * frac;
    out[outOff + 1] = aR + (bR - aR) * frac;
  }

  return out;
}

playBtn.addEventListener("click", async () => {
  playBtn.disabled = true;
  try {
    const requestedSampleRate = 48000;
    const ctx = new AudioContext({ sampleRate: requestedSampleRate, latencyHint: "interactive" });
    await ctx.audioWorklet.addModule("./src/platform/audio-worklet-processor.js");

    const srcSampleRate = 48000;
    const srcFrames = srcSampleRate;
    const channelCount = 2;
    const dstSampleRate = ctx.sampleRate;
    const dstFrames = Math.round((srcFrames * dstSampleRate) / srcSampleRate);
    const capacityFrames = dstFrames + 1024;
    const totalSamples = dstFrames * channelCount;
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

    log(`requestedSampleRate=${requestedSampleRate} actualSampleRate=${dstSampleRate}`);
    const toneS16 = generateStereoS16Tone({ frames: srcFrames, sampleRate: srcSampleRate, hz: 440 });
    const toneF32 = convertS16ToF32(toneS16);
    const toneResampled = resampleInterleavedStereoF32(toneF32, srcSampleRate, dstSampleRate);
    rb.samples.set(toneResampled);
    Atomics.store(rb.header, READ_FRAME_INDEX, 0);
    Atomics.store(rb.header, WRITE_FRAME_INDEX, dstFrames);
    log(`Wrote: ${dstFrames} frames (${totalSamples} samples) into Float32 ring buffer`);

    await ctx.resume();
    log("Playing...");
  } catch (e) {
    log(`Error: ${e?.stack ?? e}`);
    playBtn.disabled = false;
  }
});

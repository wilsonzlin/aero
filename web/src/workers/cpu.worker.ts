/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { perf } from "../perf/perf";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { FRAME_DIRTY, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "../shared/frameProtocol";
import {
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  type SharedFramebufferLayout,
} from "../ipc/shared-layout";
import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  addHeaderI32,
  initFramebufferHeader,
  storeHeaderI32,
  wrapSharedFramebuffer,
} from "../display/framebuffer_protocol";
import {
  CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
  CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
  CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
  CPU_WORKER_DEMO_GUEST_COUNTER_INDEX,
  CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES,
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
} from "../runtime/shared_layout";
import {
  CAPACITY_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  micRingBufferReadInto,
} from "../audio/mic_ring.js";
import type { MicRingBuffer } from "../audio/mic_ring.js";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type SetMicrophoneRingBufferMessage,
  type SetAudioRingBufferMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { initWasmForContext, type WasmApi } from "../runtime/wasm_context";
import { AeroIpcIoClient } from "../io/ipc/aero_ipc_io";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

type AudioOutputHdaDemoStartMessage = {
  type: "audioOutputHdaDemo.start";
  ringBuffer: SharedArrayBuffer;
  capacityFrames: number;
  channelCount: number;
  sampleRate: number;
  freqHz?: number;
  gain?: number;
};

type AudioOutputHdaDemoStopMessage = {
  type: "audioOutputHdaDemo.stop";
};

type AudioOutputHdaDemoStatsMessage = {
  type: "audioOutputHdaDemo.stats";
  bufferLevelFrames: number;
  targetFrames: number;
  totalFramesProduced?: number;
  totalFramesWritten?: number;
  totalFramesDropped?: number;
  lastTickRequestedFrames?: number;
  lastTickProducedFrames?: number;
  lastTickWrittenFrames?: number;
  lastTickDroppedFrames?: number;
};

let role: "cpu" | "gpu" | "io" | "jit" = "cpu";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing!: RingBuffer;
let guestI32!: Int32Array;
let guestU8!: Uint8Array;
let vgaFramebuffer: ReturnType<typeof wrapSharedFramebuffer> | null = null;
let frameState: Int32Array | null = null;
let io: AeroIpcIoClient | null = null;
let didIoDemo = false;

let irqBitmapLo = 0;
let irqBitmapHi = 0;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfCpuMs = 0;
let perfInstructions = 0n;
let sharedHeader: Int32Array | null = null;
let sharedLayout: SharedFramebufferLayout | null = null;
let sharedSlot0: Uint32Array | null = null;
let sharedSlot1: Uint32Array | null = null;
let sharedDirty0: Uint32Array | null = null;
let sharedDirty1: Uint32Array | null = null;
let sharedTileToggle = false;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

type MicRingBufferView = MicRingBuffer & { sampleRate: number };
let hdaDemoTimer: number | null = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let hdaDemoInstance: any | null = null;
let hdaDemoHeader: Uint32Array | null = null;
let hdaDemoCapacityFrames = 0;
let hdaDemoSampleRate = 0;
let hdaDemoNextStatsMs = 0;

function readDemoNumber(demo: unknown, key: string): number | undefined {
  if (!demo || typeof demo !== "object") return undefined;
  const record = demo as Record<string, unknown>;
  const value = record[key];
  if (typeof value === "number") return value;
  if (typeof value === "function") {
    try {
      // wasm-bindgen getters may appear as methods in some builds.
      const out = (value as (...args: unknown[]) => unknown).call(demo);
      return typeof out === "number" ? out : undefined;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function maybePostHdaDemoStats(): void {
  if (!hdaDemoInstance || !hdaDemoHeader) return;
  const now = typeof performance?.now === "function" ? performance.now() : Date.now();
  if (now < hdaDemoNextStatsMs) return;
  hdaDemoNextStatsMs = now + 250;

  const capacity = hdaDemoCapacityFrames;
  const sampleRate = hdaDemoSampleRate;
  const targetFrames = Math.min(capacity, Math.floor(sampleRate / 5));
  const msg: AudioOutputHdaDemoStatsMessage = {
    type: "audioOutputHdaDemo.stats",
    bufferLevelFrames: ringBufferLevelFrames(hdaDemoHeader, capacity),
    targetFrames,
  };

  const totalFramesProduced = readDemoNumber(hdaDemoInstance, "total_frames_produced");
  const totalFramesWritten = readDemoNumber(hdaDemoInstance, "total_frames_written");
  const totalFramesDropped = readDemoNumber(hdaDemoInstance, "total_frames_dropped");
  const lastTickRequestedFrames = readDemoNumber(hdaDemoInstance, "last_tick_requested_frames");
  const lastTickProducedFrames = readDemoNumber(hdaDemoInstance, "last_tick_produced_frames");
  const lastTickWrittenFrames = readDemoNumber(hdaDemoInstance, "last_tick_written_frames");
  const lastTickDroppedFrames = readDemoNumber(hdaDemoInstance, "last_tick_dropped_frames");

  if (typeof totalFramesProduced === "number") msg.totalFramesProduced = totalFramesProduced;
  if (typeof totalFramesWritten === "number") msg.totalFramesWritten = totalFramesWritten;
  if (typeof totalFramesDropped === "number") msg.totalFramesDropped = totalFramesDropped;
  if (typeof lastTickRequestedFrames === "number") msg.lastTickRequestedFrames = lastTickRequestedFrames;
  if (typeof lastTickProducedFrames === "number") msg.lastTickProducedFrames = lastTickProducedFrames;
  if (typeof lastTickWrittenFrames === "number") msg.lastTickWrittenFrames = lastTickWrittenFrames;
  if (typeof lastTickDroppedFrames === "number") msg.lastTickDroppedFrames = lastTickDroppedFrames;

  ctx.postMessage(msg);
}

function ringBufferLevelFrames(header: Uint32Array, capacityFrames: number): number {
  const read = Atomics.load(header, 0) >>> 0;
  const write = Atomics.load(header, 1) >>> 0;
  const available = (write - read) >>> 0;
  return Math.min(available, capacityFrames);
}

function stopHdaDemo(): void {
  if (hdaDemoTimer !== null) {
    ctx.clearInterval(hdaDemoTimer);
    hdaDemoTimer = null;
  }
  if (hdaDemoInstance && typeof hdaDemoInstance.free === "function") {
    hdaDemoInstance.free();
  }
  hdaDemoInstance = null;
  hdaDemoHeader = null;
  hdaDemoCapacityFrames = 0;
  hdaDemoSampleRate = 0;
  hdaDemoNextStatsMs = 0;
}

async function startHdaDemo(msg: AudioOutputHdaDemoStartMessage): Promise<void> {
  stopHdaDemo();

  const Sab = globalThis.SharedArrayBuffer;
  if (typeof Sab === "undefined" || !(msg.ringBuffer instanceof Sab)) {
    throw new Error("audioOutputHdaDemo.start requires a SharedArrayBuffer ring buffer.");
  }

  const capacityFrames = msg.capacityFrames | 0;
  const channelCount = msg.channelCount | 0;
  const sampleRate = msg.sampleRate | 0;
  if (capacityFrames <= 0) throw new Error("capacityFrames must be > 0");
  if (channelCount !== 2) throw new Error("channelCount must be 2 for HDA demo output");
  if (sampleRate <= 0) throw new Error("sampleRate must be > 0");

  // Prefer the single-threaded WASM build for this standalone demo mode.
  // Playwright CI prebuilds `pkg-single` but not always `pkg-threaded`; forcing
  // "single" avoids an extra failed fetch/compile attempt before falling back.
  //
  // If the single build is unavailable, fall back to the default auto selection.
  let api: WasmApi;
  try {
    ({ api } = await initWasmForContext({ variant: "single" }));
  } catch (err) {
    console.warn("Failed to init single-threaded WASM for HDA demo; falling back to auto:", err);
    ({ api } = await initWasmForContext());
  }
  if (typeof api.HdaPlaybackDemo !== "function") {
    // Graceful degrade: nothing to do if the WASM build doesn't include the demo wrapper.
    console.warn("HdaPlaybackDemo wasm export is unavailable; skipping HDA audio demo.");
    return;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const DemoCtor = api.HdaPlaybackDemo as any;
  // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
  const demo = new DemoCtor(msg.ringBuffer, capacityFrames, channelCount, sampleRate);

  const freqHz = typeof msg.freqHz === "number" ? msg.freqHz : 440;
  const gain = typeof msg.gain === "number" ? msg.gain : 0.1;
  if (typeof demo.init_sine_dma === "function") {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
    demo.init_sine_dma(freqHz, gain);
  }

  hdaDemoInstance = demo;
  const header = new Uint32Array(msg.ringBuffer, 0, 4);
  hdaDemoHeader = header;
  hdaDemoCapacityFrames = capacityFrames;
  hdaDemoSampleRate = sampleRate;

  // Keep ~200ms buffered.
  const targetFrames = Math.min(capacityFrames, Math.floor(sampleRate / 5));
  // Prime up to the target fill level (without overrunning if the buffer is already full).
  const level = ringBufferLevelFrames(header, capacityFrames);
  const prime = Math.max(0, targetFrames - level);
  if (prime > 0 && typeof demo.tick === "function") {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
    demo.tick(prime);
  }
  maybePostHdaDemoStats();

  hdaDemoTimer = ctx.setInterval(() => {
    if (!hdaDemoInstance || !hdaDemoHeader) return;
    const level = ringBufferLevelFrames(hdaDemoHeader, hdaDemoCapacityFrames);
    const target = Math.min(hdaDemoCapacityFrames, Math.floor(hdaDemoSampleRate / 5));
    const need = Math.max(0, target - level);
    if (need > 0) {
      // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
      hdaDemoInstance.tick(need);
    }
    maybePostHdaDemoStats();
  }, 20);
}

type WasmMicBridgeHandle = {
  read_f32_into(out: Float32Array): number;
  free?: () => void;
};

let micRingBuffer: MicRingBufferView | null = null;
let micScratch = new Float32Array();
let loopbackScratch = new Float32Array();
let micResampleScratch = new Float32Array();
let micResampler: JsStreamingLinearResamplerMono | null = null;
let wasmMicBridge: WasmMicBridgeHandle | null = null;

let wasmApi: WasmApi | null = null;
type CpuWorkerDemoCtor = NonNullable<WasmApi["CpuWorkerDemo"]>;
type CpuWorkerDemoInstance = InstanceType<CpuWorkerDemoCtor>;
let cpuDemo: CpuWorkerDemoInstance | null = null;

// Demo framebuffer region inside guest RAM. The worker drives a tiny JS→WASM→SAB
// render path by asking WASM to fill pixels here and then bulk-copying them into the VGA SAB.
// NOTE: Keep this disjoint from the shared framebuffer demo region starting at
// `CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES`.
const DEMO_FB_OFFSET = 0x500000;
const DEMO_FB_MAX_BYTES = 1024 * 768 * 4;

let audioRingBuffer: SharedArrayBuffer | null = null;
let audioDstSampleRate = 0;
let audioChannelCount = 0;
let audioCapacityFrames = 0;

let workletBridge: unknown | null = null;
type SineToneHandle = {
  write: (bridge: unknown, frames: number, freqHz: number, sampleRate: number, gain: number) => number;
  free?: () => void;
};
let sineTone: SineToneHandle | null = null;

let nextAudioFillDeadlineMs = 0;

const AUDIO_HEADER_U32_LEN = 4;
const AUDIO_HEADER_BYTES = AUDIO_HEADER_U32_LEN * Uint32Array.BYTES_PER_ELEMENT;
const AUDIO_READ_FRAME_INDEX = 0;
const AUDIO_WRITE_FRAME_INDEX = 1;
const AUDIO_UNDERRUN_COUNT_INDEX = 2;
const AUDIO_OVERRUN_COUNT_INDEX = 3;

function detachMicBridge(): void {
  if (wasmMicBridge && typeof wasmMicBridge.free === "function") {
    wasmMicBridge.free();
  }
  wasmMicBridge = null;
}

function maybeInitMicBridge(): void {
  if (wasmMicBridge) return;
  const apiAny = wasmApi as unknown as Record<string, unknown> | null;
  const mic = micRingBuffer;
  if (!apiAny || !mic) return;

  try {
    if (typeof apiAny.attach_mic_bridge === "function") {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      wasmMicBridge = (apiAny.attach_mic_bridge as any)(mic.sab) as WasmMicBridgeHandle;
      return;
    }

    const MicBridge = apiAny.MicBridge as { fromSharedBuffer?: unknown } | undefined;
    if (MicBridge && typeof MicBridge.fromSharedBuffer === "function") {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      wasmMicBridge = (MicBridge.fromSharedBuffer as any)(mic.sab) as WasmMicBridgeHandle;
    }
  } catch (err) {
    console.warn("Failed to attach WASM mic bridge:", err);
    detachMicBridge();
  }
}

function attachMicrophoneRingBuffer(msg: SetMicrophoneRingBufferMessage): void {
  const ringBuffer = msg.ringBuffer;
  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
    }
    if (!(ringBuffer instanceof Sab)) {
      throw new Error("setMicrophoneRingBuffer expects a SharedArrayBuffer or null.");
    }
  }

  if ((micRingBuffer?.sab ?? null) !== ringBuffer) {
    detachMicBridge();
    micResampler = null;
  }

  micRingBuffer = null;
  if (!ringBuffer) return;

  const header = new Uint32Array(ringBuffer, 0, MIC_HEADER_U32_LEN);
  const capacity = Atomics.load(header, CAPACITY_SAMPLES_INDEX) >>> 0;
  if (capacity === 0) {
    throw new Error("mic ring buffer capacity must be non-zero");
  }

  const requiredBytes = MIC_HEADER_BYTES + capacity * Float32Array.BYTES_PER_ELEMENT;
  if (ringBuffer.byteLength < requiredBytes) {
    throw new Error(`mic ring buffer is too small: need ${requiredBytes} bytes, got ${ringBuffer.byteLength} bytes`);
  }

  const data = new Float32Array(ringBuffer, MIC_HEADER_BYTES, capacity);
  micRingBuffer = { sab: ringBuffer, header, data, capacity, sampleRate: (msg.sampleRate ?? 0) | 0 };

  maybeInitMicBridge();
}

function audioFramesAvailable(readFrameIndex: number, writeFrameIndex: number): number {
  return (writeFrameIndex - readFrameIndex) >>> 0;
}

function audioFramesAvailableClamped(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number {
  return Math.min(audioFramesAvailable(readFrameIndex, writeFrameIndex), capacityFrames);
}

function audioFramesFree(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number {
  return capacityFrames - audioFramesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames);
}

class JsWorkletBridge {
  readonly capacity_frames: number;
  readonly channel_count: number;
  private readonly header: Uint32Array;
  private readonly samples: Float32Array;

  constructor(sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) {
    this.capacity_frames = capacityFrames;
    this.channel_count = channelCount;

    const sampleCapacity = capacityFrames * channelCount;
    const requiredBytes = AUDIO_HEADER_BYTES + sampleCapacity * Float32Array.BYTES_PER_ELEMENT;
    if (sab.byteLength < requiredBytes) {
      throw new Error(`audio ring buffer is too small: need ${requiredBytes} bytes, got ${sab.byteLength} bytes`);
    }

    this.header = new Uint32Array(sab, 0, AUDIO_HEADER_U32_LEN);
    this.samples = new Float32Array(sab, AUDIO_HEADER_BYTES, sampleCapacity);
  }

  buffer_level_frames(): number {
    const read = Atomics.load(this.header, AUDIO_READ_FRAME_INDEX) >>> 0;
    const write = Atomics.load(this.header, AUDIO_WRITE_FRAME_INDEX) >>> 0;
    return audioFramesAvailableClamped(read, write, this.capacity_frames);
  }

  underrun_count(): number {
    return Atomics.load(this.header, AUDIO_UNDERRUN_COUNT_INDEX) >>> 0;
  }

  overrun_count(): number {
    return Atomics.load(this.header, AUDIO_OVERRUN_COUNT_INDEX) >>> 0;
  }

  write_f32_interleaved(input: Float32Array): number {
    const requestedFrames = Math.floor(input.length / this.channel_count);
    if (requestedFrames === 0) return 0;

    const read = Atomics.load(this.header, AUDIO_READ_FRAME_INDEX) >>> 0;
    const write = Atomics.load(this.header, AUDIO_WRITE_FRAME_INDEX) >>> 0;

    const free = audioFramesFree(read, write, this.capacity_frames);
    const framesToWrite = Math.min(requestedFrames, free);
    const droppedFrames = requestedFrames - framesToWrite;
    if (droppedFrames > 0) {
      Atomics.add(this.header, AUDIO_OVERRUN_COUNT_INDEX, droppedFrames);
    }
    if (framesToWrite === 0) return 0;

    const writePos = write % this.capacity_frames;
    const firstFrames = Math.min(framesToWrite, this.capacity_frames - writePos);
    const secondFrames = framesToWrite - firstFrames;

    const cc = this.channel_count;
    const firstSamples = firstFrames * cc;
    const secondSamples = secondFrames * cc;

    this.samples.set(input.subarray(0, firstSamples), writePos * cc);
    if (secondFrames > 0) {
      this.samples.set(input.subarray(firstSamples, firstSamples + secondSamples), 0);
    }

    Atomics.store(this.header, AUDIO_WRITE_FRAME_INDEX, write + framesToWrite);
    return framesToWrite;
  }

  free(): void {
    // No-op; included for parity with wasm-bindgen objects.
  }
}

class JsSineTone {
  private phase = 0;
  private scratch = new Float32Array();
  private readonly channelCount: number;

  constructor(channelCount: number) {
    this.channelCount = channelCount;
  }

  write(bridge: unknown, frames: number, freqHz: number, sampleRate: number, gain: number): number {
    if (frames <= 0 || sampleRate <= 0) return 0;

    const cc = this.channelCount;
    const totalSamples = frames * cc;
    if (this.scratch.length < totalSamples) {
      this.scratch = new Float32Array(totalSamples);
    }
    const out = this.scratch.subarray(0, totalSamples);

    for (let i = 0; i < frames; i++) {
      const s = Math.sin(this.phase * 2 * Math.PI) * gain;
      for (let c = 0; c < cc; c++) out[i * cc + c] = s;
      this.phase += freqHz / sampleRate;
      if (this.phase >= 1) this.phase -= 1;
    }

    return (bridge as JsWorkletBridge).write_f32_interleaved(out);
  }

  free(): void {
    // No-op; included for parity with wasm-bindgen objects.
  }
}

class JsStreamingLinearResamplerMono {
  private srcRate = 0;
  private dstRate = 0;
  private stepSrcPerDst = 1;
  private srcPos = 0;

  private buf = new Float32Array(0);
  private start = 0;
  private end = 0;

  configure(srcRate: number, dstRate: number): void {
    const s = Number.isFinite(srcRate) ? Math.floor(srcRate) : 0;
    const d = Number.isFinite(dstRate) ? Math.floor(dstRate) : 0;
    if (s <= 0 || d <= 0) {
      this.reset();
      this.srcRate = 0;
      this.dstRate = 0;
      this.stepSrcPerDst = 1;
      return;
    }
    if (this.srcRate === s && this.dstRate === d) return;
    this.srcRate = s;
    this.dstRate = d;
    this.stepSrcPerDst = s / d;
    this.reset();
  }

  reset(): void {
    this.srcPos = 0;
    this.start = 0;
    this.end = 0;
  }

  queuedSourceFrames(): number {
    return Math.max(0, this.end - this.start);
  }

  requiredSourceFrames(dstFrames: number): number {
    const frames = Math.max(0, dstFrames | 0);
    if (frames === 0) return 0;

    // Need idx and idx+1 for the final output frame.
    const lastPos = this.srcPos + (frames - 1) * this.stepSrcPerDst;
    const idx = Math.floor(lastPos);
    const frac = lastPos - idx;
    if (Math.abs(frac) <= 1e-12) return idx + 1;
    return idx + 2;
  }

  pushSource(samples: Float32Array, count?: number): void {
    const len = Math.max(0, Math.min(samples.length, count ?? samples.length) | 0);
    if (len === 0) return;

    this.ensureCapacity(len);
    this.buf.set(samples.subarray(0, len), this.end);
    this.end += len;
  }

  produceInto(dstFrames: number, out: Float32Array): number {
    const frames = Math.max(0, dstFrames | 0);
    if (frames === 0) return 0;
    if (out.length < frames) return 0;

    let produced = 0;
    for (; produced < frames; produced++) {
      const idx = Math.floor(this.srcPos);
      const frac = this.srcPos - idx;
      const base = this.start + idx;
      if (base >= this.end) break;

      const a = this.buf[base];
      let sample = a;
      if (Math.abs(frac) > 1e-12) {
        if (base + 1 >= this.end) break;
        const b = this.buf[base + 1];
        sample = a + (b - a) * frac;
      }

      out[produced] = sample;

      this.srcPos += this.stepSrcPerDst;
      const drop = Math.floor(this.srcPos);
      if (drop > 0) {
        this.start += drop;
        this.srcPos -= drop;

        // Compact the queue once it grows a bit to avoid unbounded growth.
        if (this.start > 4096 && this.start > (this.buf.length >> 1)) {
          const remaining = this.end - this.start;
          this.buf.copyWithin(0, this.start, this.end);
          this.start = 0;
          this.end = remaining;
        }
      }
    }

    return produced;
  }

  private ensureCapacity(extra: number): void {
    const queued = this.end - this.start;
    if (queued < 0) {
      this.reset();
      return;
    }

    // First attempt to compact in-place if we have headroom at the front.
    if (this.start > 0 && this.buf.length - queued >= extra) {
      this.buf.copyWithin(0, this.start, this.end);
      this.start = 0;
      this.end = queued;
      return;
    }

    const needed = queued + extra;
    if (this.buf.length >= needed && this.start === 0) {
      return;
    }

    const nextCap = Math.max(needed, this.buf.length > 0 ? this.buf.length * 2 : 1024);
    const next = new Float32Array(nextCap);
    if (queued > 0) {
      next.set(this.buf.subarray(this.start, this.end), 0);
    }
    this.buf = next;
    this.start = 0;
    this.end = queued;
  }
}

function detachAudioOutput(): void {
  if (sineTone?.free) {
    sineTone.free();
  }
  sineTone = null;

  if (workletBridge && typeof (workletBridge as { free?: unknown }).free === "function") {
    (workletBridge as { free(): void }).free();
  }
  workletBridge = null;
  nextAudioFillDeadlineMs = 0;

  if (typeof status !== "undefined") {
    Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
    Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
    Atomics.store(status, StatusIndex.AudioOverrunCount, 0);
  }
}

function maybeInitAudioOutput(): void {
  detachAudioOutput();

  if (!audioRingBuffer) return;
  if (audioCapacityFrames <= 0 || audioChannelCount <= 0) return;

  // Prefer the WASM-side bridge + sine generator if available; otherwise fall back
  // to a tiny JS implementation so worker-driven audio works even when the WASM
  // packages are absent (e.g. in CI or fresh checkouts).
  if (wasmApi?.attach_worklet_bridge && wasmApi?.SineTone) {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      workletBridge = (wasmApi.attach_worklet_bridge as any)(audioRingBuffer, audioCapacityFrames, audioChannelCount);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      sineTone = new (wasmApi.SineTone as any)() as SineToneHandle;
      nextAudioFillDeadlineMs = performance.now();
      return;
    } catch (err) {
      console.error("Failed to init WASM audio output bridge:", err);
      detachAudioOutput();
    }
  }

  try {
    workletBridge = new JsWorkletBridge(audioRingBuffer, audioCapacityFrames, audioChannelCount);
    sineTone = new JsSineTone(audioChannelCount);
    nextAudioFillDeadlineMs = performance.now();
  } catch (err) {
    console.error("Failed to init JS audio output bridge:", err);
    detachAudioOutput();
  }
}

function attachAudioRingBuffer(msg: SetAudioRingBufferMessage): void {
  const ringBuffer = msg.ringBuffer;
  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      throw new Error("SharedArrayBuffer is unavailable; audio output requires crossOriginIsolated.");
    }
    if (!(ringBuffer instanceof Sab)) {
      throw new Error("setAudioRingBuffer expects a SharedArrayBuffer or null.");
    }
  }

  audioRingBuffer = ringBuffer;
  audioDstSampleRate = msg.dstSampleRate >>> 0;
  audioChannelCount = msg.channelCount >>> 0;
  audioCapacityFrames = msg.capacityFrames >>> 0;

  maybeInitAudioOutput();
}

function pumpMicLoopback(maxWriteFrames: number): number {
  const mic = micRingBuffer;
  const bridge = workletBridge as { write_f32_interleaved?: (samples: Float32Array) => number } | null;
  if (!mic || !bridge || typeof bridge.write_f32_interleaved !== "function") return 0;

  const cc = audioChannelCount;
  if (cc <= 0) return 0;

  const gain = 1.0;
  const maxChunkFrames = 256;
  const srcRate = mic.sampleRate;
  const dstRate = audioDstSampleRate;

  let remaining = Math.max(0, maxWriteFrames | 0);
  let totalWritten = 0;

  if (srcRate > 0 && dstRate > 0 && srcRate !== dstRate) {
    const resampler = micResampler ?? (micResampler = new JsStreamingLinearResamplerMono());
    resampler.configure(srcRate, dstRate);

    while (remaining > 0) {
      const frames = Math.min(remaining, maxChunkFrames);
      if (frames <= 0) break;

      const requiredSrc = resampler.requiredSourceFrames(frames);
      const queuedSrc = resampler.queuedSourceFrames();
      const needSrc = Math.max(0, requiredSrc - queuedSrc);

      if (needSrc > 0) {
        if (micScratch.length < needSrc) micScratch = new Float32Array(needSrc);
        const micSlice = micScratch.subarray(0, needSrc);
        let read = 0;
        if (wasmMicBridge) {
          try {
            read = wasmMicBridge.read_f32_into(micSlice) | 0;
          } catch (err) {
            console.warn("WASM mic bridge read failed; falling back to JS ring reader:", err);
            detachMicBridge();
          }
        }
        if (!wasmMicBridge) {
          read = micRingBufferReadInto(mic, micSlice);
        }

        if (read > 0) {
          resampler.pushSource(micSlice, read);
        } else if (queuedSrc === 0) {
          break;
        }
      }

      if (micResampleScratch.length < frames) micResampleScratch = new Float32Array(frames);
      const monoOut = micResampleScratch.subarray(0, frames);
      const produced = resampler.produceInto(frames, monoOut);
      if (produced === 0) break;

      const outSamples = produced * cc;
      if (loopbackScratch.length < outSamples) loopbackScratch = new Float32Array(outSamples);

      if (cc === 1) {
        for (let i = 0; i < produced; i++) loopbackScratch[i] = monoOut[i] * gain;
      } else {
        for (let i = 0; i < produced; i++) {
          const s = monoOut[i] * gain;
          const base = i * cc;
          for (let c = 0; c < cc; c++) loopbackScratch[base + c] = s;
        }
      }

      const written = bridge.write_f32_interleaved(loopbackScratch.subarray(0, outSamples)) | 0;
      if (written === 0) break;
      totalWritten += written;
      remaining -= written;
    }

    return totalWritten;
  }

  // We aren't resampling this call; reset any prior resampler state so we don't
  // replay stale queued samples if we later re-enter the resampling path with
  // the same rate pair.
  micResampler?.reset();

  while (remaining > 0) {
    const frames = Math.min(remaining, maxChunkFrames);
    if (micScratch.length < frames) micScratch = new Float32Array(frames);

    const micSlice = micScratch.subarray(0, frames);
    let read = 0;
    if (wasmMicBridge) {
      try {
        read = wasmMicBridge.read_f32_into(micSlice) | 0;
      } catch (err) {
        console.warn("WASM mic bridge read failed; falling back to JS ring reader:", err);
        detachMicBridge();
      }
    }
    if (!wasmMicBridge) {
      read = micRingBufferReadInto(mic, micSlice);
    }
    if (read === 0) break;

    const outSamples = read * cc;
    if (loopbackScratch.length < outSamples) loopbackScratch = new Float32Array(outSamples);

    if (cc === 1) {
      for (let i = 0; i < read; i++) loopbackScratch[i] = micScratch[i] * gain;
    } else {
      for (let i = 0; i < read; i++) {
        const s = micScratch[i] * gain;
        const base = i * cc;
        for (let c = 0; c < cc; c++) loopbackScratch[base + c] = s;
      }
    }

    const written = bridge.write_f32_interleaved(loopbackScratch.subarray(0, outSamples)) | 0;
    if (written === 0) break;
    totalWritten += written;
    remaining -= written;
  }

  return totalWritten;
}

let diskDemoStarted = false;
let diskDemoResponses = 0;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const msg = ev.data as
    | Partial<WorkerInitMessage>
    | Partial<ConfigUpdateMessage>
    | Partial<SetAudioRingBufferMessage>
    | Partial<SetMicrophoneRingBufferMessage>
    | Partial<AudioOutputHdaDemoStartMessage>
    | Partial<AudioOutputHdaDemoStopMessage>
    | undefined;
  if (!msg) return;

  if ((msg as Partial<AudioOutputHdaDemoStopMessage>).type === "audioOutputHdaDemo.stop") {
    stopHdaDemo();
    return;
  }

  if ((msg as Partial<AudioOutputHdaDemoStartMessage>).type === "audioOutputHdaDemo.start") {
    void startHdaDemo(msg as AudioOutputHdaDemoStartMessage).catch((err) => {
      console.error(err);
      stopHdaDemo();
    });
    return;
  }
  if ((msg as { kind?: unknown }).kind === "config.update") {
    currentConfig = (msg as ConfigUpdateMessage).config;
    currentConfigVersion = (msg as ConfigUpdateMessage).version;
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  if ((msg as Partial<SetMicrophoneRingBufferMessage>)?.type === "setMicrophoneRingBuffer") {
    attachMicrophoneRingBuffer(msg as SetMicrophoneRingBufferMessage);
    return;
  }

  if ((msg as Partial<SetAudioRingBufferMessage>)?.type === "setAudioRingBuffer") {
    attachAudioRingBuffer(msg as SetAudioRingBufferMessage);
    return;
  }

  if ((msg as { type?: unknown }).type === "setAudioOutputRingBuffer") {
    const legacy = msg as Partial<{
      ringBuffer: SharedArrayBuffer | null;
      sampleRate: number;
      channelCount: number;
      capacityFrames: number;
    }>;
    attachAudioRingBuffer({
      type: "setAudioRingBuffer",
      ringBuffer: (legacy.ringBuffer as SharedArrayBuffer | null) ?? null,
      capacityFrames: legacy.capacityFrames ?? 0,
      channelCount: legacy.channelCount ?? 0,
      dstSampleRate: legacy.sampleRate ?? 0,
    });
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind !== "init") return;
  void initAndRun(init as WorkerInitMessage);
};

async function initAndRun(init: WorkerInitMessage): Promise<void> {
  perf.spanBegin("worker:boot");
  try {
    perf.spanBegin("worker:init");
    try {
      role = init.role ?? "cpu";
      const segments = {
        control: init.controlSab!,
        guestMemory: init.guestMemory!,
        vgaFramebuffer: init.vgaFramebuffer!,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      const views = createSharedMemoryViews(segments);
      status = views.status;
      guestI32 = views.guestI32;
      guestU8 = views.guestU8;
      vgaFramebuffer = wrapSharedFramebuffer(segments.vgaFramebuffer, 0);
      frameState = init.frameStateSab ? new Int32Array(init.frameStateSab) : null;

      const demoFbEnd = DEMO_FB_OFFSET + DEMO_FB_MAX_BYTES;
      if (demoFbEnd > guestU8.byteLength) {
        const guestBytes = guestU8.byteLength;
        const message = `guestMemory too small for demo framebuffer: need >= ${demoFbEnd} bytes, got ${guestBytes} bytes.`;
        setReadyFlag(status, role, false);
        ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
        ctx.close();
        return;
      }

      if (init.perfChannel) {
        perfWriter = new PerfWriter(init.perfChannel.buffer, {
          workerKind: init.perfChannel.workerKind,
          runStartEpochMs: init.perfChannel.runStartEpochMs,
        });
        perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
      }

      initFramebufferHeader(vgaFramebuffer.header, {
        width: 320,
        height: 200,
        strideBytes: 320 * 4,
        format: FRAMEBUFFER_FORMAT_RGBA8888,
      });

      initSharedFramebufferViews(segments.sharedFramebuffer, segments.sharedFramebufferOffsetBytes);

      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
      const ioCmd = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
      const ioEvt = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);
      irqBitmapLo = 0;
      irqBitmapHi = 0;
      Atomics.store(status, StatusIndex.CpuIrqBitmapLo, 0);
      Atomics.store(status, StatusIndex.CpuIrqBitmapHi, 0);
      Atomics.store(status, StatusIndex.CpuA20Enabled, 0);
      io = new AeroIpcIoClient(ioCmd, ioEvt, {
        onIrq: (irq, level) => {
          perf.instant("cpu:io:irq", "t", { irq, level });
          const idx = irq & 0xff;
          if (idx < 32) {
            const bit = 1 << idx;
            irqBitmapLo = level ? (irqBitmapLo | bit) >>> 0 : (irqBitmapLo & ~bit) >>> 0;
            Atomics.store(status, StatusIndex.CpuIrqBitmapLo, irqBitmapLo | 0);
          } else if (idx < 64) {
            const bit = 1 << (idx - 32);
            irqBitmapHi = level ? (irqBitmapHi | bit) >>> 0 : (irqBitmapHi & ~bit) >>> 0;
            Atomics.store(status, StatusIndex.CpuIrqBitmapHi, irqBitmapHi | 0);
          }
        },
        onA20: (enabled) => {
          perf.counter("cpu:io:a20Enabled", enabled ? 1 : 0);
          Atomics.store(status, StatusIndex.CpuA20Enabled, enabled ? 1 : 0);
        },
        onSerialOutput: (port, data) => {
          // Forward serial output to the coordinator via the runtime event ring.
          // Best-effort: don't block the CPU on log traffic.
          pushEvent({ kind: "serialOutput", port: port & 0xffff, data });
        },
        onReset: () => {
          // Reset requests are rare but important; use a blocking push so the
          // coordinator reliably observes the event and can restart the VM.
          pushEventBlocking({ kind: "resetRequest" }, 250);
        },
      });
      setReadyFlag(status, role, true);
      ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
      if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });

      // WASM is optional in this repo (CI runs with `--skip-wasm`), but worker init
      // should be fast enough to start pumping AudioWorklet ring buffers.
      //
      // Kick off WASM init in the background so the worker can enter its main loop
      // immediately (JS fallbacks will be used until WASM is ready).
      void initWasmInBackground(init, segments.guestMemory, segments.sharedFramebufferOffsetBytes);
    } finally {
      perf.spanEnd("worker:init");
    }
  } finally {
    perf.spanEnd("worker:boot");
  }

  void runLoop();
}

async function initWasmInBackground(
  init: WorkerInitMessage,
  guestMemory: WebAssembly.Memory,
  sharedFramebufferOffsetBytes: number,
): Promise<void> {
  try {
    const { api, variant } = await perf.spanAsync("wasm:init", () =>
      initWasmForContext({
        variant: init.wasmVariant,
        memory: guestMemory,
        module: init.wasmModule,
      }),
    );

    // Sanity-check that the provided `guestMemory` is actually wired up as
    // the WASM module's linear memory (imported+exported memory build).
    //
    // This enables shared-memory integration where JS + WASM + other workers
    // all observe the same guest RAM.
    //
    // Probe within guest RAM (not the runtime-reserved low region of the wasm
    // linear memory) so we don't risk clobbering the Rust/WASM runtime.
    const memProbeGuestOffset = 0x100;
    const memProbeLinearOffset = guestU8.byteOffset + memProbeGuestOffset;
    const memView = new DataView(guestMemory.buffer);
    const prev = memView.getUint32(memProbeLinearOffset, true);

    const a = 0x11223344;
    memView.setUint32(memProbeLinearOffset, a, true);
    const gotA = api.mem_load_u32(memProbeLinearOffset);
    if (gotA !== a) {
      throw new Error(`WASM guestMemory wiring failed: JS wrote 0x${a.toString(16)}, WASM read 0x${gotA.toString(16)}.`);
    }

    const b = 0x55667788;
    api.mem_store_u32(memProbeLinearOffset, b);
    const gotB = memView.getUint32(memProbeLinearOffset, true);
    if (gotB !== b) {
      throw new Error(`WASM guestMemory wiring failed: WASM wrote 0x${b.toString(16)}, JS read 0x${gotB.toString(16)}.`);
    }

    // Restore the previous value so we don't permanently dirty guest RAM.
    memView.setUint32(memProbeLinearOffset, prev, true);

    wasmApi = api;
    cpuDemo = null;
    const CpuWorkerDemo = api.CpuWorkerDemo;
    if (CpuWorkerDemo) {
      try {
        const ramSizeBytes = guestMemory.buffer.byteLength >>> 0;
        const framebufferLinearOffset = (sharedFramebufferOffsetBytes ?? 0) >>> 0;
        if (framebufferLinearOffset === 0) {
          throw new Error("shared framebuffer is not embedded in guest memory; CpuWorkerDemo requires an in-wasm framebuffer.");
        }
        const guestCounterLinearOffset = (guestU8.byteOffset + CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES) >>> 0;
        cpuDemo = new CpuWorkerDemo(
          ramSizeBytes,
          framebufferLinearOffset,
          CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
          CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
          CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
          guestCounterLinearOffset,
        );
      } catch (err) {
        console.warn("Failed to init CpuWorkerDemo wasm export:", err);
        cpuDemo = null;
      }
    }

    maybeInitAudioOutput();
    maybeInitMicBridge();

    if (Atomics.load(status, StatusIndex.StopRequested) !== 1) {
      const value = api.add(20, 22);
      ctx.postMessage({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // WASM init is best-effort: keep the CPU worker alive so non-WASM demos
    // (including AudioWorklet ring-buffer smoke tests) can run in environments
    // where the generated wasm-pack output is absent.
    console.error("WASM init failed in CPU worker:", err);
    pushEvent({ kind: "log", level: "error", message: `WASM init failed: ${message}` });
    wasmApi = null;
    cpuDemo = null;
    maybeInitAudioOutput();
  }
}

async function runLoop(): Promise<void> {
  try {
    await runLoopInner();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEventBlocking({ kind: "panic", message });
    setReadyFlag(status, role, false);
    ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  }
}

async function runLoopInner(): Promise<void> {
  let running = false;
  const heartbeatIntervalMs = 250;
  const frameIntervalMs = 1000 / 60;
  const modeSwitchIntervalMs = 2500;
  const audioFillIntervalMs = 20;

  let nextHeartbeatMs = performance.now();
  let nextFrameMs = performance.now();
  let nextModeSwitchMs = performance.now() + modeSwitchIntervalMs;

  const modes = [
    { width: 320, height: 200 },
    { width: 640, height: 480 },
    { width: 1024, height: 768 },
  ] as const;
  let modeIndex = 0;
  let mode: (typeof modes)[number] = modes[0];
  let demoFbView = guestU8.subarray(DEMO_FB_OFFSET, DEMO_FB_OFFSET + mode.width * mode.height * 4);
  const demoFbLinearOffset = guestU8.byteOffset + DEMO_FB_OFFSET;
  const instructionsPerSharedFrame = BigInt(CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH * CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT);

  const maybeEmitPerfSample = () => {
    if (!perfWriter || !perfFrameHeader) return;
    const enabled = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
    const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
    if (!enabled) {
      perfLastFrameId = frameId;
      perfCpuMs = 0;
      perfInstructions = 0n;
      return;
    }
    if (frameId === 0 || frameId === perfLastFrameId) return;
    perfLastFrameId = frameId;

    perfWriter.frameSample(frameId, {
      durations: { cpu_ms: perfCpuMs },
      counters: { instructions: perfInstructions },
    });

    perfCpuMs = 0;
    perfInstructions = 0n;
  };

  while (true) {
    // Drain commands.
    while (true) {
      const bytes = commandRing.tryPop();
      if (!bytes) break;
      let cmd: Command;
      try {
        cmd = decodeCommand(bytes);
      } catch {
        continue;
      }

      if (cmd.kind === "nop") {
        running = true;
        perfCpuMs = 0;
        perfInstructions = 0n;
        perfLastFrameId = 0;
        nextHeartbeatMs = performance.now();
        nextFrameMs = performance.now();
        nextModeSwitchMs = performance.now() + modeSwitchIntervalMs;
        if (!diskDemoStarted) {
          diskDemoStarted = true;
          void runDiskReadDemo();
        }
        // Ack acts as a cheap "ring is alive" signal for the coordinator.
        pushEvent({ kind: "ack", seq: cmd.seq });
      } else if (cmd.kind === "shutdown") {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (running) {
      const now = performance.now();
      // Drain asynchronous device events (IRQs, A20, etc.) even when the CPU is
      // not actively waiting on an I/O roundtrip.
      io?.poll(64);

      if (workletBridge && audioDstSampleRate > 0 && audioCapacityFrames > 0) {
        if (nextAudioFillDeadlineMs === 0) nextAudioFillDeadlineMs = now;
        if (now >= nextAudioFillDeadlineMs) {
          let level = 0;
          let underruns = 0;
          let overruns = 0;
          const bridge = workletBridge as {
            buffer_level_frames?: () => number;
            underrun_count?: () => number;
            overrun_count?: () => number;
          };
          if (typeof bridge.buffer_level_frames === "function") level = bridge.buffer_level_frames() | 0;
          if (typeof bridge.underrun_count === "function") underruns = bridge.underrun_count() | 0;
          if (typeof bridge.overrun_count === "function") overruns = bridge.overrun_count() | 0;

          const targetFrames = Math.min(audioCapacityFrames, Math.floor(audioDstSampleRate / 5)); // ~200ms
          const need = Math.max(0, targetFrames - level);
          if (need > 0) {
            const maxWriteFrames = Math.min(need, Math.min(targetFrames, Math.floor(audioDstSampleRate / 10))); // cap to ~100ms
            if (maxWriteFrames > 0) {
              if (micRingBuffer) {
                pumpMicLoopback(maxWriteFrames);
              } else {
                sineTone?.write(workletBridge, maxWriteFrames, 440, audioDstSampleRate, 0.1);
              }
            }
          }

          // Export a tiny amount of producer-side telemetry for the UI.
          if (typeof bridge.buffer_level_frames === "function") level = bridge.buffer_level_frames() | 0;
          if (typeof bridge.underrun_count === "function") underruns = bridge.underrun_count() | 0;
          if (typeof bridge.overrun_count === "function") overruns = bridge.overrun_count() | 0;
          Atomics.store(status, StatusIndex.AudioBufferLevelFrames, level);
          Atomics.store(status, StatusIndex.AudioUnderrunCount, underruns);
          Atomics.store(status, StatusIndex.AudioOverrunCount, overruns);

          nextAudioFillDeadlineMs = now + audioFillIntervalMs;
        }
      } else {
        nextAudioFillDeadlineMs = 0;
        Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
        Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
        Atomics.store(status, StatusIndex.AudioOverrunCount, 0);
      }

      if (!didIoDemo && io && Atomics.load(status, StatusIndex.IoReady) === 1) {
        didIoDemo = true;
        try {
          perf.spanBegin("cpu:io:demo");
          // Read i8042 status (0x64) and command byte (via 0x20 -> 0x60).
          const status64 = perf.span("cpu:io:portRead 0x64", () => io!.portRead(0x64, 1));
          perf.counter("cpu:io:i8042:status", status64);
          const cmdByte = perf.span("cpu:io:i8042:readCommandByte", () => {
            io!.portWrite(0x64, 1, 0x20);
            return io!.portRead(0x60, 1);
          });
          perf.counter("cpu:io:i8042:commandByte", cmdByte);

          // Emit a couple bytes on COM1; the I/O worker should mirror them back
          // as `serialOutput` events, which we forward to the coordinator/UI.
          perf.span("cpu:io:uart16550:write", () => {
            io!.portWrite(0x3f8, 1, "H".charCodeAt(0));
            io!.portWrite(0x3f8, 1, "i".charCodeAt(0));
            io!.portWrite(0x3f8, 1, "\r".charCodeAt(0));
            io!.portWrite(0x3f8, 1, "\n".charCodeAt(0));
          });

          // eslint-disable-next-line no-console
          console.log(
            `[cpu] io demo: i8042 status=0x${status64.toString(16)} cmdByte=0x${cmdByte.toString(16)}`,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("[cpu] io demo failed:", err);
        } finally {
          perf.spanEnd("cpu:io:demo");
        }
      }

      if (now >= nextHeartbeatMs) {
        const counter = Atomics.add(status, StatusIndex.HeartbeatCounter, 1) + 1;
        if (cpuDemo) {
          cpuDemo.tick(now);
        } else {
          Atomics.add(guestI32, CPU_WORKER_DEMO_GUEST_COUNTER_INDEX, 1);
        }
        perf.counter("heartbeatCounter", counter);
        // Best-effort: heartbeat events are allowed to drop if the ring is full.
        pushEvent({ kind: "ack", seq: counter });
        nextHeartbeatMs = now + heartbeatIntervalMs;
      }

      if (now >= nextFrameMs) {
        const header = perfFrameHeader;
        const perfActive =
          !!perfWriter &&
          !!header &&
          Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0 &&
          (Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0) !== 0;
        const t0 = perfActive ? performance.now() : 0;

        if (vgaFramebuffer) {
          if (now >= nextModeSwitchMs) {
            modeIndex = (modeIndex + 1) % modes.length;
            mode = modes[modeIndex];

            const strideBytes = mode.width * 4;
            storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_WIDTH, mode.width);
            storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_HEIGHT, mode.height);
            storeHeaderI32(vgaFramebuffer.header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
            addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_CONFIG_COUNTER, 1);

            demoFbView = guestU8.subarray(DEMO_FB_OFFSET, DEMO_FB_OFFSET + strideBytes * mode.height);
            nextModeSwitchMs = now + modeSwitchIntervalMs;
          }

          const strideBytes = mode.width * 4;
          const wasmRender = wasmApi?.demo_render_rgba8888;
          if (typeof wasmRender === "function") {
            const instructions = wasmRender(demoFbLinearOffset, mode.width, mode.height, strideBytes, now) >>> 0;
            vgaFramebuffer.pixelsU8Clamped.set(demoFbView);
            if (perfActive) perfInstructions += BigInt(instructions);
          } else {
            // Fallback for dev builds where the wasm package hasn't been rebuilt yet.
            renderTestPattern(vgaFramebuffer, mode.width, mode.height, now);
            if (perfActive) perfInstructions += BigInt(mode.width * mode.height);
          }

          addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_FRAME_COUNTER, 1);
        }

        // Shared framebuffer demo: prefer the WASM-side publisher, fall back to the JS implementation.
        if (cpuDemo) {
          const seq = cpuDemo.render_frame(0, now);
          if (perfActive) perfInstructions += instructionsPerSharedFrame;
          if (frameState) {
            Atomics.store(frameState, FRAME_SEQ_INDEX, seq);
            Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
          }
        } else {
          publishSharedFramebufferFrame();
        }

        if (perfActive) perfCpuMs += performance.now() - t0;
        nextFrameMs = now + frameIntervalMs;
      }

      maybeEmitPerfSample();
    }

    // Sleep until either new commands arrive or the next heartbeat tick.
    if (!running) {
      // IMPORTANT: Use the async wait path so the worker stays responsive to
      // structured `postMessage` attachments (e.g. audio ring + mic ring buffer)
      // while idle. A blocking Atomics.wait loop would starve the message queue.
      await commandRing.waitForDataAsync(1000);
      continue;
    }

    const now = performance.now();
    const nextAudioMs = workletBridge ? nextAudioFillDeadlineMs : Number.POSITIVE_INFINITY;
    const until = Math.min(nextHeartbeatMs, nextFrameMs, nextModeSwitchMs, nextAudioMs) - now;
    await commandRing.waitForDataAsync(Math.max(0, Math.min(heartbeatIntervalMs, until)));
  }

  setReadyFlag(status, role, false);
  detachMicBridge();
  detachAudioOutput();
  if (cpuDemo) {
    cpuDemo.free();
    cpuDemo = null;
  }
  ctx.close();
}

async function runDiskReadDemo(): Promise<void> {
  const client = io;
  if (!client) return;

  // Wait until the I/O worker reports ready.
  while (Atomics.load(status, StatusIndex.IoReady) !== 1) {
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }

  // Read the first sector into guest RAM at an arbitrary scratch offset.
  const guestOffset = 0x1000n;
  const len = 512;
  try {
    const evt = client.diskRead(0n, len, guestOffset, 2000);
    diskDemoResponses += 1;
    perf.counter("diskReadDemoResponses", diskDemoResponses);
    if (perf.traceEnabled) perf.instant("diskReadDemoResp", "t", evt as unknown as Record<string, unknown>);

    if (evt.ok && evt.bytes >= 4) {
      const firstDword = new DataView(guestU8.buffer, guestU8.byteOffset + Number(guestOffset), 4).getUint32(0, true);
      perf.counter("diskReadDemoFirstDword", firstDword);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[cpu] disk read demo failed:", err);
    if (perf.traceEnabled) {
      perf.instant("diskReadDemoError", "t", { message: err instanceof Error ? err.message : String(err) });
    }
  }
}

function pushEvent(evt: Event): void {
  try {
    eventRing.tryPush(encodeEvent(evt));
  } catch {
    // Ignore malformed events.
  }
}

function pushEventBlocking(evt: Event, timeoutMs = 1000): void {
  const payload = encodeEvent(evt);
  if (eventRing.tryPush(payload)) return;
  try {
    eventRing.pushBlocking(payload, timeoutMs);
  } catch {
    // Ignore if the ring is wedged; postMessage ERROR remains a backup.
  }
}

function initSharedFramebufferViews(shared: SharedArrayBuffer, offsetBytes: number): void {
  const header = new Int32Array(shared, offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  // Stored in an Int32Array header (Atomics requires a signed typed array), so
  // compare against the signed i32 constants from `ipc/shared-layout.ts`.
  const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC) | 0;
  const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION) | 0;
  if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) {
    throw new Error(
      `shared framebuffer header mismatch: magic=0x${(magic >>> 0).toString(16)} version=${version} expected magic=0x${(SHARED_FRAMEBUFFER_MAGIC >>> 0).toString(16)} version=${SHARED_FRAMEBUFFER_VERSION}`,
    );
  }

  const layout = layoutFromHeader(header);
  const stridePixels = layout.strideBytes / 4;
  const pixelWords = stridePixels * layout.height;

  sharedHeader = header;
  sharedLayout = layout;
  sharedSlot0 = new Uint32Array(shared, offsetBytes + layout.framebufferOffsets[0], pixelWords);
  sharedSlot1 = new Uint32Array(shared, offsetBytes + layout.framebufferOffsets[1], pixelWords);
  sharedDirty0 =
    layout.dirtyWordsPerBuffer === 0 ? null : new Uint32Array(shared, offsetBytes + layout.dirtyOffsets[0], layout.dirtyWordsPerBuffer);
  sharedDirty1 =
    layout.dirtyWordsPerBuffer === 0 ? null : new Uint32Array(shared, offsetBytes + layout.dirtyOffsets[1], layout.dirtyWordsPerBuffer);
}

function publishSharedFramebufferFrame(): void {
  if (!sharedHeader || !sharedLayout || !sharedSlot0 || !sharedSlot1) return;

  const active = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
  const back = active ^ 1;

  const backPixels = back === 0 ? sharedSlot0 : sharedSlot1;
  const backDirty = back === 0 ? sharedDirty0 : sharedDirty1;

  const base = 0xff00ff00; // RGBA green in little-endian u32
  const tileColor = sharedTileToggle ? 0xff0000ff /* RGBA red */ : base;
  // Advance the toggle once per full double-buffer cycle. If we flip the color every
  // frame while also flipping which buffer is active, each slot ends up with a
  // stable color (slot0 always red, slot1 always green). That makes smoke tests
  // flaky if the presenter consistently drops one parity of frames.
  if (back === 0) sharedTileToggle = !sharedTileToggle;

  const backSlotSeq = Atomics.load(
    sharedHeader,
    back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
  );
  const backSlotInitialized = backSlotSeq !== 0;

  // Full frame is constant green; only the top-left tile toggles, allowing dirty-rect
  // uploads (when supported) to preserve the rest of the texture.
  if (!backSlotInitialized) {
    backPixels.fill(base);
  }

  const tileSize = sharedLayout.tileSize || sharedLayout.width;
  const tileW = Math.min(tileSize, sharedLayout.width);
  const tileH = Math.min(tileSize, sharedLayout.height);
  const stridePixels = sharedLayout.strideBytes / 4;
  for (let y = 0; y < tileH; y += 1) {
    const row = y * stridePixels;
    backPixels.fill(tileColor, row, row + tileW);
  }

  const prevSeq = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ);
  const newSeq = (prevSeq + 1) | 0;

  if (backDirty) {
    if (!backSlotInitialized) {
      // Initialize the presenter texture for each slot with a full upload the first
      // time we write it (double buffering means slot 1 is uninitialized on frame 1).
      backDirty.fill(0xffffffff);
    } else {
      backDirty.fill(0);
      backDirty[0] = 1; // mark tile 0 (top-left)
    }
  }

  Atomics.store(
    sharedHeader,
    back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
    newSeq,
  );
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
  Atomics.notify(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

  if (frameState) {
    Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
    Atomics.notify(frameState, FRAME_STATUS_INDEX);
  }
}

function renderTestPattern(
  fb: ReturnType<typeof wrapSharedFramebuffer>,
  width: number,
  height: number,
  nowMs: number,
): void {
  const pixels = fb.pixelsU8Clamped;
  const strideBytes = width * 4;
  const t = nowMs * 0.001;

  for (let y = 0; y < height; y++) {
    const base = y * strideBytes;
    for (let x = 0; x < width; x++) {
      const i = base + x * 4;
      pixels[i + 0] = (x + t * 60) & 255;
      pixels[i + 1] = (y + t * 35) & 255;
      pixels[i + 2] = ((x ^ y) + t * 20) & 255;
      pixels[i + 3] = 255;
    }
  }
}

// Keep config in scope for devtools inspection.
void currentConfig;

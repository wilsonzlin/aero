/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeCommand, decodeEvent, encodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { FRAME_DIRTY, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "../shared/frameProtocol";
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
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
} from "../runtime/shared_layout";
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

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: "cpu" | "gpu" | "io" | "jit" = "cpu";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing!: RingBuffer;
let guestI32!: Int32Array;
let guestU8!: Uint8Array;
let vgaFramebuffer: ReturnType<typeof wrapSharedFramebuffer> | null = null;
let frameState: Int32Array | null = null;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfCpuMs = 0;
let perfInstructions = 0n;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

type MicRingBufferView = {
  sab: SharedArrayBuffer;
  header: Uint32Array;
  data: Float32Array;
  capacity: number;
  sampleRate: number;
};

type WasmMicBridgeHandle = {
  read_f32_into(out: Float32Array): number;
  free?: () => void;
};

let micRingBuffer: MicRingBufferView | null = null;
let micScratch = new Float32Array();
let loopbackScratch = new Float32Array();
let wasmMicBridge: WasmMicBridgeHandle | null = null;

let wasmApi: WasmApi | null = null;

// Demo framebuffer region inside guest RAM. The worker drives a tiny JS→WASM→SAB
// render path by asking WASM to fill pixels here and then bulk-copying them into the VGA SAB.
const DEMO_FB_OFFSET = 0x200000;
const DEMO_FB_MAX_BYTES = 1024 * 768 * 4;

let audioRingBuffer: SharedArrayBuffer | null = null;
let audioDstSampleRate = 0;
let audioChannelCount = 0;
let audioCapacityFrames = 0;

let workletBridge: unknown | null = null;
let sineTone: { write: (...args: unknown[]) => number; free?: () => void } | null = null;

let nextAudioFillDeadlineMs = 0;

const MIC_HEADER_U32_LEN = 4;
const MIC_HEADER_BYTES = MIC_HEADER_U32_LEN * Uint32Array.BYTES_PER_ELEMENT;
const MIC_WRITE_POS_INDEX = 0;
const MIC_READ_POS_INDEX = 1;
const MIC_DROPPED_SAMPLES_INDEX = 2;
const MIC_CAPACITY_SAMPLES_INDEX = 3;

const AUDIO_HEADER_U32_LEN = 4;
const AUDIO_HEADER_BYTES = AUDIO_HEADER_U32_LEN * Uint32Array.BYTES_PER_ELEMENT;
const AUDIO_READ_FRAME_INDEX = 0;
const AUDIO_WRITE_FRAME_INDEX = 1;
const AUDIO_UNDERRUN_COUNT_INDEX = 2;
const AUDIO_OVERRUN_COUNT_INDEX = 3;

function micSamplesAvailable(readPos: number, writePos: number): number {
  return (writePos - readPos) >>> 0;
}

function micSamplesAvailableClamped(readPos: number, writePos: number, capacitySamples: number): number {
  return Math.min(micSamplesAvailable(readPos, writePos), capacitySamples >>> 0);
}

function micRingBufferReadInto(rb: MicRingBufferView, out: Float32Array): number {
  const readPos = Atomics.load(rb.header, MIC_READ_POS_INDEX) >>> 0;
  const writePos = Atomics.load(rb.header, MIC_WRITE_POS_INDEX) >>> 0;
  const available = micSamplesAvailableClamped(readPos, writePos, rb.capacity);
  const toRead = Math.min(out.length, available);
  if (toRead === 0) return 0;

  const start = readPos % rb.capacity;
  const firstPart = Math.min(toRead, rb.capacity - start);
  out.set(rb.data.subarray(start, start + firstPart), 0);
  const remaining = toRead - firstPart;
  if (remaining) {
    out.set(rb.data.subarray(0, remaining), firstPart);
  }

  Atomics.store(rb.header, MIC_READ_POS_INDEX, (readPos + toRead) >>> 0);
  return toRead;
}

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
  }

  micRingBuffer = null;
  if (!ringBuffer) return;

  const header = new Uint32Array(ringBuffer, 0, MIC_HEADER_U32_LEN);
  const capacity = header[MIC_CAPACITY_SAMPLES_INDEX] >>> 0;
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
      sineTone = new (wasmApi.SineTone as any)() as { write: (...args: unknown[]) => number; free?: () => void };
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

  let remaining = Math.max(0, maxWriteFrames | 0);
  let totalWritten = 0;

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

let ioCmdRing: RingBuffer | null = null;
let ioEvtRing: RingBuffer | null = null;

let diskDemoStarted = false;
let diskDemoResponses = 0;
let nextIoIpcId = 1;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const msg = ev.data as Partial<
    WorkerInitMessage | ConfigUpdateMessage | SetAudioRingBufferMessage | SetMicrophoneRingBufferMessage
  >;
  if (msg?.kind === "config.update") {
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

      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
      ioCmdRing = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
      ioEvtRing = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);

      try {
        const { api, variant } = await perf.spanAsync("wasm:init", () =>
          initWasmForContext({
            variant: init.wasmVariant,
            memory: segments.guestMemory,
            module: init.wasmModule,
          }),
        );

        // Sanity-check that the provided `guestMemory` is actually wired up as
        // the WASM module's linear memory (imported+exported memory build).
        //
        // This enables shared-memory integration where JS + WASM + other workers
        // all observe the same guest RAM.
        // Probe within guest RAM (not the runtime-reserved low region of the wasm
        // linear memory) so we don't risk clobbering the Rust/WASM runtime.
        const memProbeGuestOffset = 0x100;
        const memProbeLinearOffset = guestU8.byteOffset + memProbeGuestOffset;
        const memView = new DataView(segments.guestMemory.buffer);
        const prev = memView.getUint32(memProbeLinearOffset, true);

        const a = 0x11223344;
        memView.setUint32(memProbeLinearOffset, a, true);
        const gotA = api.mem_load_u32(memProbeLinearOffset);
        if (gotA !== a) {
          throw new Error(
            `WASM guestMemory wiring failed: JS wrote 0x${a.toString(16)}, WASM read 0x${gotA.toString(16)}.`,
          );
        }

        const b = 0x55667788;
        api.mem_store_u32(memProbeLinearOffset, b);
        const gotB = memView.getUint32(memProbeLinearOffset, true);
        if (gotB !== b) {
          throw new Error(
            `WASM guestMemory wiring failed: WASM wrote 0x${b.toString(16)}, JS read 0x${gotB.toString(16)}.`,
          );
        }

        // Restore the previous value so we don't permanently dirty guest RAM.
        memView.setUint32(memProbeLinearOffset, prev, true);

        wasmApi = api;
        maybeInitAudioOutput();
        maybeInitMicBridge();
        const value = api.add(20, 22);
        ctx.postMessage({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        // WASM init is best-effort: keep the CPU worker alive so non-WASM demos
        // (including AudioWorklet ring-buffer smoke tests) can run in environments
        // where the generated wasm-pack output is absent.
        console.error("WASM init failed in CPU worker:", err);
        pushEvent({ kind: "log", level: "error", message: `WASM init failed: ${message}` });
        wasmApi = null;
        maybeInitAudioOutput();
      }

      setReadyFlag(status, role, true);
      ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
      if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });
    } finally {
      perf.spanEnd("worker:init");
    }
  } finally {
    perf.spanEnd("worker:boot");
  }

  void runLoop();
}

function runLoop(): void {
  try {
    runLoopInner();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEventBlocking({ kind: "panic", message });
    setReadyFlag(status, role, false);
    ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  }
}

function runLoopInner(): void {
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

  const maybeEmitPerfSample = () => {
    if (!perfWriter || !perfFrameHeader) return;
    const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
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

      if (now >= nextHeartbeatMs) {
        const counter = Atomics.add(status, StatusIndex.HeartbeatCounter, 1) + 1;
        Atomics.add(guestI32, 0, 1);
        perf.counter("heartbeatCounter", counter);
        // Best-effort: heartbeat events are allowed to drop if the ring is full.
        pushEvent({ kind: "ack", seq: counter });
        nextHeartbeatMs = now + heartbeatIntervalMs;
      }

      if (vgaFramebuffer && now >= nextFrameMs) {
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

        const t0 = performance.now();
        const strideBytes = mode.width * 4;

        const wasmRender = wasmApi?.demo_render_rgba8888;
        if (typeof wasmRender === "function") {
          const instructions = wasmRender(demoFbLinearOffset, mode.width, mode.height, strideBytes, now) >>> 0;
          vgaFramebuffer.pixelsU8Clamped.set(demoFbView);
          perfInstructions += BigInt(instructions);
        } else {
          // Fallback for dev builds where the wasm package hasn't been rebuilt yet.
          renderTestPattern(vgaFramebuffer, mode.width, mode.height, now);
          perfInstructions += BigInt(mode.width * mode.height);
        }

        perfCpuMs += performance.now() - t0;
        addHeaderI32(vgaFramebuffer.header, HEADER_INDEX_FRAME_COUNTER, 1);
        if (frameState) {
          Atomics.add(frameState, FRAME_SEQ_INDEX, 1);
          Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
        }
        nextFrameMs = now + frameIntervalMs;
      }

      maybeEmitPerfSample();
    }

    // Sleep until either new commands arrive or the next heartbeat tick.
    if (!running) {
      commandRing.waitForData(1000);
      continue;
    }

    const now = performance.now();
    const nextAudioMs = workletBridge ? nextAudioFillDeadlineMs : Number.POSITIVE_INFINITY;
    const until = Math.min(nextHeartbeatMs, nextFrameMs, nextModeSwitchMs, nextAudioMs) - now;
    commandRing.waitForData(Math.max(0, Math.min(heartbeatIntervalMs, until)));
  }

  setReadyFlag(status, role, false);
  detachAudioOutput();
  ctx.close();
}

async function runDiskReadDemo(): Promise<void> {
  const cmdRing = ioCmdRing;
  const evtRing = ioEvtRing;
  if (!cmdRing || !evtRing) return;

  // Wait until the I/O worker reports ready.
  while (Atomics.load(status, StatusIndex.IoReady) !== 1) {
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }

  // Read the first sector into guest RAM at an arbitrary scratch offset.
  const id = (nextIoIpcId++ >>> 0) || (nextIoIpcId++ >>> 0);
  const guestOffset = 0x1000n;
  const len = 512;
  const cmdBytes = encodeCommand({ kind: "diskRead", id, diskOffset: 0n, len, guestOffset });

  // Best-effort retry if the ring is temporarily full.
  while (!cmdRing.tryPush(cmdBytes)) {
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }

  const deadlineMs = performance.now() + 2000;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    while (true) {
      const bytes = evtRing.tryPop();
      if (!bytes) break;
      const evt = decodeEvent(bytes);
      if (evt.kind !== "diskReadResp") continue;
      if (evt.id !== id) continue;

      diskDemoResponses += 1;
      perf.counter("diskReadDemoResponses", diskDemoResponses);
      if (perf.traceEnabled) perf.instant("diskReadDemoResp", "t", evt as unknown as Record<string, unknown>);

      if (evt.ok && evt.bytes >= 4) {
        const firstDword = new DataView(guestU8.buffer, guestU8.byteOffset + Number(guestOffset), 4).getUint32(0, true);
        perf.counter("diskReadDemoFirstDword", firstDword);
      }
      return;
    }

    const now = performance.now();
    if (now >= deadlineMs) {
      if (perf.traceEnabled) perf.instant("diskReadDemoTimeout", "t");
      return;
    }

    const res = await evtRing.waitForDataAsync(Math.max(0, deadlineMs - now));
    if (res === "timed-out") {
      if (perf.traceEnabled) perf.instant("diskReadDemoTimeout", "t");
      return;
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

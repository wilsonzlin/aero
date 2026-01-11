/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeEvent, encodeCommand } from "../ipc/protocol";
import type { RingBuffer as IpcRingBuffer } from "../ipc/ring_buffer";
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
import { RingBuffer as RuntimeRingBuffer } from "../runtime/ring_buffer";
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
  type SetAudioRingBufferMessage,
  type WorkerInitMessage,
  decodeProtocolMessage,
  encodeProtocolMessage,
} from "../runtime/protocol";
import { initWasmForContext, type WasmApi } from "../runtime/wasm_context";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: "cpu" | "gpu" | "io" | "jit" = "cpu";
let status!: Int32Array;
let commandRing!: RuntimeRingBuffer;
let eventRing!: RuntimeRingBuffer;
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

let wasmApi: WasmApi | null = null;

let audioRingBuffer: SharedArrayBuffer | null = null;
let audioDstSampleRate = 0;
let audioChannelCount = 0;
let audioCapacityFrames = 0;

let workletBridge: unknown | null = null;
let sineTone: { write: (...args: unknown[]) => number; free?: () => void } | null = null;

let nextAudioFillDeadlineMs = 0;

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
  }
}

function maybeInitAudioOutput(): void {
  detachAudioOutput();

  if (!audioRingBuffer) return;
  if (!wasmApi?.attach_worklet_bridge || !wasmApi?.SineTone) return;
  if (audioCapacityFrames <= 0 || audioChannelCount <= 0) return;

  // Try to initialize the WASM-side bridge + sine generator. This is a best-effort
  // path (used by the Playwright AudioWorklet worker smoke test).
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    workletBridge = (wasmApi.attach_worklet_bridge as any)(audioRingBuffer, audioCapacityFrames, audioChannelCount);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    sineTone = new (wasmApi.SineTone as any)() as { write: (...args: unknown[]) => number; free?: () => void };
    nextAudioFillDeadlineMs = performance.now();
  } catch (err) {
    console.error("Failed to init audio output bridge:", err);
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
let ioCmdRing: IpcRingBuffer | null = null;
let ioEvtRing: IpcRingBuffer | null = null;

let diskDemoStarted = false;
let diskDemoResponses = 0;
let nextIoIpcId = 1;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const msg = ev.data as Partial<WorkerInitMessage | ConfigUpdateMessage | SetAudioRingBufferMessage>;
  if (msg?.kind === "config.update") {
    currentConfig = (msg as ConfigUpdateMessage).config;
    currentConfigVersion = (msg as ConfigUpdateMessage).version;
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  if ((msg as Partial<SetAudioRingBufferMessage>)?.type === "setAudioRingBuffer") {
    attachAudioRingBuffer(msg as SetAudioRingBufferMessage);
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
      commandRing = new RuntimeRingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);
      eventRing = new RuntimeRingBuffer(segments.control, regions.event.byteOffset, regions.event.byteLength);

      ioCmdRing = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
      ioEvtRing = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);

      try {
        const { api, variant } = await perf.spanAsync("wasm:init", () =>
          initWasmForContext({ memory: segments.guestMemory }),
        );

        // Sanity-check that the provided `guestMemory` is actually wired up as
        // the WASM module's linear memory (imported+exported memory build).
        //
        // This enables shared-memory integration where JS + WASM + other workers
        // all observe the same guest RAM.
        const memProbeOffset = 0x100;
        const memView = new DataView(segments.guestMemory.buffer);
        const prev = memView.getUint32(memProbeOffset, true);

        const a = 0x11223344;
        memView.setUint32(memProbeOffset, a, true);
        const gotA = api.mem_load_u32(memProbeOffset);
        if (gotA !== a) {
          throw new Error(
            `WASM guestMemory wiring failed: JS wrote 0x${a.toString(16)}, WASM read 0x${gotA.toString(16)}.`,
          );
        }

        const b = 0x55667788;
        api.mem_store_u32(memProbeOffset, b);
        const gotB = memView.getUint32(memProbeOffset, true);
        if (gotB !== b) {
          throw new Error(
            `WASM guestMemory wiring failed: WASM wrote 0x${b.toString(16)}, JS read 0x${gotB.toString(16)}.`,
          );
        }

        // Restore the previous value so we don't permanently dirty guest RAM.
        memView.setUint32(memProbeOffset, prev, true);

        wasmApi = api;
        maybeInitAudioOutput();
        const value = api.add(20, 22);
        ctx.postMessage({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setReadyFlag(status, role, false);
        ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
        ctx.close();
        return;
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

async function runLoop(): Promise<void> {
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
  let mode = modes[0];

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
      const bytes = commandRing.pop();
      if (!bytes) break;
      const cmd = decodeProtocolMessage(bytes);
      if (!cmd) continue;

      if (cmd.type === MessageType.START) {
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
      } else if (cmd.type === MessageType.STOP) {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (running) {
      const now = performance.now();

      if (workletBridge && sineTone && audioDstSampleRate > 0 && audioCapacityFrames > 0) {
        if (nextAudioFillDeadlineMs === 0) nextAudioFillDeadlineMs = now;
        if (now >= nextAudioFillDeadlineMs) {
          let level = 0;
          let underruns = 0;
          const bridge = workletBridge as { buffer_level_frames?: () => number; underrun_count?: () => number };
          if (typeof bridge.buffer_level_frames === "function") level = bridge.buffer_level_frames() | 0;
          if (typeof bridge.underrun_count === "function") underruns = bridge.underrun_count() | 0;

          const targetFrames = Math.min(audioCapacityFrames, Math.floor(audioDstSampleRate / 5)); // ~200ms
          const need = Math.max(0, targetFrames - level);
          if (need > 0) {
            const maxWriteFrames = Math.min(need, Math.min(targetFrames, Math.floor(audioDstSampleRate / 10))); // cap to ~100ms
            if (maxWriteFrames > 0) {
              sineTone.write(workletBridge, maxWriteFrames, 440, audioDstSampleRate, 0.1);
            }
          }

          // Export a tiny amount of producer-side telemetry for the UI.
          if (typeof bridge.buffer_level_frames === "function") level = bridge.buffer_level_frames() | 0;
          if (typeof bridge.underrun_count === "function") underruns = bridge.underrun_count() | 0;
          Atomics.store(status, StatusIndex.AudioBufferLevelFrames, level);
          Atomics.store(status, StatusIndex.AudioUnderrunCount, underruns);

          nextAudioFillDeadlineMs = now + audioFillIntervalMs;
        }
      } else {
        nextAudioFillDeadlineMs = 0;
        Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
        Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
      }

      if (now >= nextHeartbeatMs) {
        const counter = Atomics.add(status, StatusIndex.HeartbeatCounter, 1) + 1;
        Atomics.add(guestI32, 0, 1);
        perf.counter("heartbeatCounter", counter);
        // Best-effort: heartbeat events are allowed to drop if the ring is full.
        eventRing.push(encodeProtocolMessage({ type: MessageType.HEARTBEAT, role, counter }));
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

          nextModeSwitchMs = now + modeSwitchIntervalMs;
        }

        const t0 = performance.now();
        renderTestPattern(vgaFramebuffer, mode.width, mode.height, now);
        perfCpuMs += performance.now() - t0;
        perfInstructions += BigInt(mode.width * mode.height);
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
      await commandRing.waitForData();
      continue;
    }

    const now = performance.now();
    const nextAudioMs = workletBridge && sineTone ? nextAudioFillDeadlineMs : Number.POSITIVE_INFINITY;
    const until = Math.min(nextHeartbeatMs, nextFrameMs, nextModeSwitchMs, nextAudioMs) - now;
    await commandRing.waitForData(Math.max(0, Math.min(heartbeatIntervalMs, until)));
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
        const firstDword = new DataView(guestU8.buffer, Number(guestOffset), 4).getUint32(0, true);
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

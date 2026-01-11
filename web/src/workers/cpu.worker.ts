/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
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
import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
  decodeProtocolMessage,
  encodeProtocolMessage,
} from "../runtime/protocol";
import { initWasmForContext } from "../runtime/wasm_context";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: "cpu" | "gpu" | "io" | "jit" = "cpu";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing!: RingBuffer;
let guestI32!: Int32Array;
let vgaFramebuffer: ReturnType<typeof wrapSharedFramebuffer> | null = null;
let frameState: Int32Array | null = null;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfCpuMs = 0;
let perfInstructions = 0n;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const msg = ev.data as Partial<WorkerInitMessage | ConfigUpdateMessage>;
  if (msg?.kind === "config.update") {
    currentConfig = (msg as ConfigUpdateMessage).config;
    currentConfigVersion = (msg as ConfigUpdateMessage).version;
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
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
      const segments = { control: init.controlSab!, guestMemory: init.guestMemory!, vgaFramebuffer: init.vgaFramebuffer! };
      const views = createSharedMemoryViews(segments);
      status = views.status;
      guestI32 = views.guestI32;
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
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset, regions.event.byteLength);

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
      } else if (cmd.type === MessageType.STOP) {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (running) {
      const now = performance.now();

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
    const until = Math.min(nextHeartbeatMs, nextFrameMs, nextModeSwitchMs) - now;
    await commandRing.waitForData(Math.max(0, Math.min(heartbeatIntervalMs, until)));
  }

  setReadyFlag(status, role, false);
  ctx.close();
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

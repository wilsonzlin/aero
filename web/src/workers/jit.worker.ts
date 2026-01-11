/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
  decodeProtocolMessage,
} from "../runtime/protocol";
import { waitUntilNotEqual } from "../runtime/atomics_wait";
import { fnv1a32Hex } from "../utils/fnv1a";
import { type JitCompileRequest, type JitWorkerResponse, isJitCompileRequest } from "./jit_protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: "cpu" | "gpu" | "io" | "jit" = "jit";
let status!: Int32Array;
let commandRing!: RingBuffer;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

function detectDynamicWasmCompilation(): boolean {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Module !== "function") {
    return false;
  }
  try {
    new WebAssembly.Module(WASM_EMPTY_MODULE_BYTES);
    return true;
  } catch {
    return false;
  }
}

type JitPlatformFeatures = {
  /**
   * Whether dynamic WebAssembly compilation is allowed in this worker context.
   *
   * Primarily gated by CSP (`script-src 'wasm-unsafe-eval'`).
   */
  jit_dynamic_wasm: boolean;
};

let currentPlatformFeatures: JitPlatformFeatures = { jit_dynamic_wasm: detectDynamicWasmCompilation() };
let jitEnabled = false;

const JIT_CACHE_MAX_ENTRIES = 64;
const moduleCache = new Map<string, WebAssembly.Module>();
const inflightCompiles = new Map<string, Promise<WebAssembly.Module>>();

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function maybeUpdatePlatformFeatures(msg: unknown): void {
  if (!isRecord(msg)) return;
  const provided = (msg as { platformFeatures?: unknown }).platformFeatures;
  if (!isRecord(provided)) return;
  if (typeof (provided as { jit_dynamic_wasm?: unknown }).jit_dynamic_wasm !== "boolean") return;

  currentPlatformFeatures.jit_dynamic_wasm = (provided as { jit_dynamic_wasm: boolean }).jit_dynamic_wasm;
}

function recomputeJitEnabled(): void {
  const hasWasm =
    typeof WebAssembly !== "undefined" && typeof WebAssembly.Module === "function" && typeof WebAssembly.compile === "function";
  jitEnabled = hasWasm && currentPlatformFeatures.jit_dynamic_wasm;
}

recomputeJitEnabled();

function isCspBlockedError(err: unknown): boolean {
  if (err instanceof DOMException) {
    if (err.name === "SecurityError") return true;
  }
  const msg = err instanceof Error ? err.message : String(err);
  return /wasm-unsafe-eval|content security policy|csp/i.test(msg);
}

function cacheKeyForWasmBytes(wasmBytes: ArrayBuffer): string {
  const bytes = new Uint8Array(wasmBytes);
  const hash = fnv1a32Hex(bytes);
  return `${hash}:${bytes.byteLength}`;
}

function cacheGet(key: string): WebAssembly.Module | undefined {
  const existing = moduleCache.get(key);
  if (!existing) return undefined;
  // Refresh insertion order for LRU.
  moduleCache.delete(key);
  moduleCache.set(key, existing);
  return existing;
}

function cacheSet(key: string, module: WebAssembly.Module): void {
  // Refresh insertion order for LRU even when the key already exists.
  moduleCache.delete(key);
  moduleCache.set(key, module);
  if (moduleCache.size <= JIT_CACHE_MAX_ENTRIES) return;
  const oldest = moduleCache.keys().next().value as string | undefined;
  if (oldest !== undefined) moduleCache.delete(oldest);
}

async function waitForCommandRingDataNonBlocking(timeoutMs?: number): Promise<void> {
  // `RingBuffer.waitForData()` uses blocking Atomics.wait() in workers for efficiency.
  //
  // The JIT worker also services structured `postMessage()` requests, so we must
  // not block the worker thread; otherwise `jit:compile` messages would never
  // be delivered while we're waiting for ring-buffer commands.
  const start = timeoutMs === undefined ? 0 : performance.now();
  while (true) {
    // RingBuffer internal layout: meta[0]=head, meta[1]=tail.
    const head = Atomics.load(commandRing.meta, 0);
    const tail = Atomics.load(commandRing.meta, 1);
    if (head !== tail) return;

    const remaining =
      timeoutMs === undefined ? undefined : Math.max(0, timeoutMs - (performance.now() - start));
    const result = await waitUntilNotEqual(commandRing.meta, 0, head, { timeoutMs: remaining, canBlock: false });
    if (result === "timed-out") return;
  }
}

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfJitMs = 0;
let perfBlocksCompiled = 0;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  if (isJitCompileRequest(ev.data)) {
    void handleCompile(ev.data);
    return;
  }

  const msg = ev.data as Partial<WorkerInitMessage | ConfigUpdateMessage>;
  if (msg?.kind === "config.update") {
    maybeUpdatePlatformFeatures(msg);
    currentConfig = (msg as ConfigUpdateMessage).config;
    currentConfigVersion = (msg as ConfigUpdateMessage).version;
    recomputeJitEnabled();
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind !== "init") return;

  perf.spanBegin("worker:boot");
  try {
    perf.spanBegin("wasm:init");
    perf.spanEnd("wasm:init");

      perf.spanBegin("worker:init");
      try {
        role = init.role ?? "jit";
        maybeUpdatePlatformFeatures(init);
        recomputeJitEnabled();
        const segments = {
          control: init.controlSab!,
          guestMemory: init.guestMemory!,
          vgaFramebuffer: init.vgaFramebuffer!,
          ioIpc: init.ioIpcSab!,
        };
        status = createSharedMemoryViews(segments).status;
        const regions = ringRegionsForWorker(role);
        commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);

      if (init.perfChannel) {
        perfWriter = new PerfWriter(init.perfChannel.buffer, {
          workerKind: init.perfChannel.workerKind,
          runStartEpochMs: init.perfChannel.runStartEpochMs,
        });
        perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
        perfLastFrameId = 0;
        perfJitMs = 0;
        perfBlocksCompiled = 0;
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
};

async function handleCompile(req: JitCompileRequest): Promise<void> {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Module !== "function" || typeof WebAssembly.compile !== "function") {
    postJitResponse({
      type: "jit:error",
      id: req.id,
      code: "unsupported",
      message: "WebAssembly.compile is unavailable in this environment.",
      durationMs: 0,
    });
    return;
  }

  if (!currentPlatformFeatures.jit_dynamic_wasm) {
    postJitResponse({
      type: "jit:error",
      id: req.id,
      code: "csp_blocked",
      message: "Dynamic WebAssembly compilation is blocked by Content Security Policy (missing 'wasm-unsafe-eval').",
      durationMs: 0,
    });
    return;
  }

  if (!jitEnabled) {
    postJitResponse({
      type: "jit:error",
      id: req.id,
      code: "unsupported",
      message: "JIT compilation is disabled in this worker.",
      durationMs: 0,
    });
    return;
  }

  const key = cacheKeyForWasmBytes(req.wasmBytes);
  const cached = cacheGet(key);
  if (cached) {
    if (perf.traceEnabled) perf.instant("jit:cache_hit", "t", { key });
    postJitResponse({ type: "jit:compiled", id: req.id, module: cached, durationMs: 0, cached: true });
    return;
  }

  const inflight = inflightCompiles.get(key);
  if (inflight) {
    const startMs = performance.now();
    if (perf.traceEnabled) perf.instant("jit:inflight_hit", "t", { key });
    try {
      const module = await inflight;
      const durationMs = performance.now() - startMs;
      cacheSet(key, module);
      postJitResponse({ type: "jit:compiled", id: req.id, module, durationMs, cached: true });
    } catch (err) {
      const durationMs = performance.now() - startMs;
      const message = err instanceof Error ? err.message : String(err);
      const code = isCspBlockedError(err) ? "csp_blocked" : "compile_failed";
      if (code === "csp_blocked") {
        currentPlatformFeatures.jit_dynamic_wasm = false;
        recomputeJitEnabled();
      }
      postJitResponse({ type: "jit:error", id: req.id, code, message, durationMs });
    }
    return;
  }

  const startMs = performance.now();
  perf.spanBegin("jit:compile");
  if (perf.traceEnabled) perf.instant("jit:compile:begin", "t", { key });

  let module: WebAssembly.Module;
  try {
    const promise = WebAssembly.compile(req.wasmBytes);
    inflightCompiles.set(key, promise);
    module = await promise;
  } catch (err) {
    const durationMs = performance.now() - startMs;
    perfJitMs += durationMs;
    const message = err instanceof Error ? err.message : String(err);
    const code = isCspBlockedError(err) ? "csp_blocked" : "compile_failed";
    if (code === "csp_blocked") {
      currentPlatformFeatures.jit_dynamic_wasm = false;
      recomputeJitEnabled();
    }
    postJitResponse({ type: "jit:error", id: req.id, code, message, durationMs });
    return;
  } finally {
    inflightCompiles.delete(key);
    perf.spanEnd("jit:compile");
  }

  const durationMs = performance.now() - startMs;
  perfJitMs += durationMs;
  perfBlocksCompiled += 1;
  if (perf.traceEnabled) perf.instant("jit:compile:end", "t", { key, durationMs });

  cacheSet(key, module);
  postJitResponse({ type: "jit:compiled", id: req.id, module, durationMs });
}

function postJitResponse(msg: JitWorkerResponse): void {
  try {
    ctx.postMessage(msg);
  } catch (err) {
    // Most commonly: `DataCloneError` when `WebAssembly.Module` is not structured-cloneable.
    const message = err instanceof Error ? err.message : String(err);
    const fallback: JitWorkerResponse = {
      type: "jit:error",
      id: msg.id,
      code: "unsupported",
      message: `Failed to post JIT response: ${message}`,
      durationMs: "durationMs" in msg ? msg.durationMs : undefined,
    };
    ctx.postMessage(fallback);
  }
}

async function runLoop(): Promise<void> {
  const pollIntervalMs = 16;

  while (true) {
    while (true) {
      const bytes = commandRing.pop();
      if (!bytes) break;
      const cmd = decodeProtocolMessage(bytes);
      if (!cmd) continue;
      if (cmd.type === MessageType.STOP) {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (perfWriter && perfFrameHeader) {
      const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
      if (frameId !== 0 && frameId !== perfLastFrameId) {
        perfLastFrameId = frameId;
        perfWriter.frameSample(frameId, {
          durations: { jit_ms: perfJitMs > 0 ? perfJitMs : 0.01 },
          counters: { instructions: perfBlocksCompiled },
        });
        perfJitMs = 0;
        perfBlocksCompiled = 0;
      }
    }

    await waitForCommandRingDataNonBlocking(perfWriter && perfFrameHeader ? pollIntervalMs : undefined);
  }

  setReadyFlag(status, role, false);
  ctx.close();
}

void currentConfig;

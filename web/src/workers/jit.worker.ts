/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
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
  moduleCache.set(key, module);
  if (moduleCache.size <= JIT_CACHE_MAX_ENTRIES) return;
  const oldest = moduleCache.keys().next().value as string | undefined;
  if (oldest !== undefined) moduleCache.delete(oldest);
}

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
      const segments = { control: init.controlSab!, guestMemory: init.guestMemory!, vgaFramebuffer: init.vgaFramebuffer! };
      status = createSharedMemoryViews(segments).status;
      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);

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

  const startMs = performance.now();
  perf.spanBegin("jit:compile");
  if (perf.traceEnabled) perf.instant("jit:compile:begin", "t", { key });

  let module: WebAssembly.Module;
  try {
    module = await WebAssembly.compile(req.wasmBytes);
  } catch (err) {
    const durationMs = performance.now() - startMs;
    const message = err instanceof Error ? err.message : String(err);
    postJitResponse({ type: "jit:error", id: req.id, code: "compile_failed", message, durationMs });
    return;
  } finally {
    perf.spanEnd("jit:compile");
  }

  const durationMs = performance.now() - startMs;
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
    await commandRing.waitForData();
  }

  setReadyFlag(status, role, false);
  ctx.close();
}

void currentConfig;

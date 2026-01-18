/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag, type WorkerRole } from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { initJitWasmForContext, type JitWasmApi, type Tier1BlockCompilation } from "../runtime/jit_wasm_loader";
import { fnv1a32Hex } from "../utils/fnv1a";
import { formatOneLineError } from "../text";
import { unrefBestEffort } from "../unrefSafe";
import {
  type JitCompileRequest,
  type JitTier1CompileRequest,
  type JitTier1CompiledResponse,
  type JitWorkerResponse,
  isJitCompileRequest,
  isJitTier1CompileRequest,
} from "./jit_protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: WorkerRole = "jit";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing: RingBuffer | null = null;
let guestMemory: WebAssembly.Memory | null = null;
let guestBase = 0;
let guestSize = 0;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

// Keep these values aligned with the Tier-1 compiler's expectations:
// - x86 instruction decoder can read up to 15 bytes per instruction.
// - `aero-jit-wasm` caps the maximum input code slice to 1MiB.
const TIER1_DEFAULT_MAX_BYTES = 1024;
const TIER1_DECODE_WINDOW_SLACK_BYTES = 15;
const TIER1_MAX_COMPILER_CODE_BYTES = 1024 * 1024;

const MAX_JIT_ERROR_MESSAGE_BYTES = 512;
const MAX_JIT_ERROR_SCAN_BYTES = 2048;

function formatJitErrorForScan(err: unknown): string {
  return formatOneLineError(err, MAX_JIT_ERROR_SCAN_BYTES, "");
}

function formatJitErrorMessage(err: unknown): string {
  return formatOneLineError(err, MAX_JIT_ERROR_MESSAGE_BYTES);
}

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
type InflightCompile = {
  promise: Promise<WebAssembly.Module>;
  startMs: number;
  endMs: number | null;
};

const inflightCompiles = new Map<string, InflightCompile>();

let jitWasmApiPromise: Promise<JitWasmApi> | null = null;
let canPostTier1Module: boolean | null = null;

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
  const msg = formatJitErrorForScan(err);
  return /wasm-unsafe-eval|content security policy|csp/i.test(msg);
}

function isDataCloneError(err: unknown): boolean {
  const domException = (globalThis as unknown as { DOMException?: unknown }).DOMException;
  if (typeof domException === "function") {
    const DomException = domException as unknown as { new (...args: unknown[]): unknown };
    try {
      if (err instanceof DomException && (err as { name?: unknown }).name === "DataCloneError") return true;
    } catch {
      // ignore
    }
  }
  if (err && typeof err === "object") {
    try {
      const name = (err as { name?: unknown }).name;
      if (name === "DataCloneError") return true;
    } catch {
      // ignore
    }
  }
  const message = formatJitErrorForScan(err);
  return /DataCloneError|could not be cloned/i.test(message);
}

async function loadJitWasmApi(): Promise<JitWasmApi> {
  if (!jitWasmApiPromise) {
    jitWasmApiPromise = initJitWasmForContext().then(({ api }) => api);
  }
  try {
    return await jitWasmApiPromise;
  } catch (err) {
    // Allow retries if initialization fails (e.g. missing assets during dev or CSP restrictions).
    jitWasmApiPromise = null;
    throw err;
  }
}

function clampU32(n: number): number {
  if (!Number.isFinite(n) || n < 0) return 0;
  return n > 0xffffffff ? 0xffffffff : (n >>> 0);
}

function sliceTier1CodeWindow(entryRip: number, maxBytes: number): Uint8Array {
  const mem = guestMemory;
  if (!mem) throw new Error("guest memory not initialized");
  const buf = mem.buffer as unknown as ArrayBufferLike;

  const entry = clampU32(entryRip);
  const max = clampU32(maxBytes);
  const effectiveMax = max === 0 ? TIER1_DEFAULT_MAX_BYTES : max;
  const desiredLen = effectiveMax + TIER1_DECODE_WINDOW_SLACK_BYTES;

  const availableGuest = Math.max(0, guestSize - entry);
  const lenGuest = Math.min(desiredLen, availableGuest);

  const base = guestBase + entry;
  const bufByteLen = buf.byteLength;
  if (base < 0 || base > bufByteLen) {
    throw new Error(
      `entryRip out of guest memory bounds: entryRip=0x${entryRip.toString(16)} guestBase=0x${guestBase.toString(16)} wasmBytes=0x${bufByteLen.toString(16)}`,
    );
  }
  const availableBuf = Math.max(0, bufByteLen - base);
  const len = Math.min(lenGuest, availableBuf);

  return new Uint8Array(buf, base, len);
}

function toOwnedArrayBufferBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // Ensure the payload is backed by a detached-transferable ArrayBuffer (not SharedArrayBuffer),
  // and ideally owns exactly the byte range being transferred.
  if (bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength) {
    return bytes as Uint8Array<ArrayBuffer>;
  }
  // Fallback: copy into a fresh ArrayBuffer so:
  // - WebAssembly.validate/compile accept it under `ES2024.SharedMemory` libs
  // - We never accidentally transfer/detach a large wasm memory buffer
  // - It is safe to transfer the exact wasm bytes payload.
  return new Uint8Array(bytes) as Uint8Array<ArrayBuffer>;
}

function normalizeTier1Compilation(result: unknown, fallbackCodeByteLen: number): Tier1BlockCompilation {
  const fallbackLen = clampU32(fallbackCodeByteLen);
  // The Tier-1 wasm-bindgen ABI is still evolving. Older builds returned raw `Uint8Array` bytes,
  // while newer ones return a small object with `{ wasm_bytes, code_byte_len, ... }`.
  if (result instanceof Uint8Array) {
    return {
      wasm_bytes: result,
      code_byte_len: fallbackLen,
      exit_to_interpreter: false,
    };
  }

  if (result && typeof result === "object") {
    const wasmBytes = (result as Partial<Tier1BlockCompilation>).wasm_bytes;
    if (!(wasmBytes instanceof Uint8Array)) {
      throw new Error("JIT compiler returned unexpected result (missing wasm_bytes Uint8Array)");
    }
    const codeByteLenRaw = (result as Partial<Tier1BlockCompilation>).code_byte_len;
    let codeByteLen = fallbackLen;
    if (typeof codeByteLenRaw === "number" && Number.isFinite(codeByteLenRaw)) {
      codeByteLen = clampU32(codeByteLenRaw);
      if (codeByteLen > fallbackLen) codeByteLen = fallbackLen;
    }
    const exitToInterp =
      typeof (result as Partial<Tier1BlockCompilation>).exit_to_interpreter === "boolean"
        ? (result as Partial<Tier1BlockCompilation>).exit_to_interpreter!
        : false;
    return { wasm_bytes: wasmBytes, code_byte_len: codeByteLen, exit_to_interpreter: exitToInterp };
  }

  throw new Error("JIT compiler returned unexpected result (expected Uint8Array or { wasm_bytes: Uint8Array, ... })");
}

function isTier1CompileError(err: unknown): boolean {
  // Newer aero-jit-wasm builds prefix their error messages with `compile_tier1_block:`. Use that
  // to distinguish genuine compilation failures from ABI mismatches (wrong arg types/count) when
  // running against older wasm-pack outputs.
  const message = formatJitErrorForScan(err);
  return message.trimStart().startsWith("compile_tier1_block:");
}

function isTier1AbiMismatchError(err: unknown): boolean {
  // wasm-bindgen argument mismatches typically show up as TypeErrors (wrong BigInt/number types,
  // wrong arg count, etc). Use a best-effort heuristic so we don't accidentally swallow real
  // compiler/runtime errors by retrying with legacy call signatures.
  if (err instanceof TypeError) return true;
  const message = formatJitErrorForScan(err);
  return /bigint|cannot convert|argument|parameter|is not a function/i.test(message);
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
  // JIT worker must remain responsive to structured `postMessage()` compile requests,
  // so avoid blocking `Atomics.wait()` here.
  await commandRing.waitForDataAsync(timeoutMs);
}

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;

let pendingJitMs = 0;
let pendingJitFlushTimer: number | null = null;
let pendingJitFlushAttempts = 0;

const PENDING_JIT_FLUSH_INTERVAL_MS = 20;
const PENDING_JIT_FLUSH_MAX_ATTEMPTS = 10;

function stopPendingJitFlushTimer(): void {
  if (pendingJitFlushTimer === null) return;
  clearInterval(pendingJitFlushTimer);
  pendingJitFlushTimer = null;
  pendingJitFlushAttempts = 0;
}

function maybeStartPendingJitFlushTimer(): void {
  if (pendingJitFlushTimer !== null) return;
  pendingJitFlushAttempts = 0;
  const timer = setInterval(() => {
    const writer = perfWriter;
    const header = perfFrameHeader;
    if (!writer || !header) {
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
      return;
    }

    const enabled = Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
    if (!enabled) {
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
      return;
    }

    const frameId = Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
    if (frameId !== 0) {
      const jitMs = pendingJitMs;
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
      if (jitMs > 0) {
        writer.frameSample(frameId, { durations: { jit_ms: jitMs } });
      }
      return;
    }

    pendingJitFlushAttempts += 1;
    if (pendingJitFlushAttempts >= PENDING_JIT_FLUSH_MAX_ATTEMPTS) {
      // Avoid keeping a hot timer alive indefinitely if RAF is throttled / paused.
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
    }
  }, PENDING_JIT_FLUSH_INTERVAL_MS) as unknown as number;
  unrefBestEffort(timer);
  pendingJitFlushTimer = timer;
}

function maybeWritePerfSample(jitMs: number): void {
  const writer = perfWriter;
  const header = perfFrameHeader;
  if (!writer || !header) return;
  const enabled = Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
  if (!enabled) return;
  const frameId = Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
  if (frameId === 0) {
    // Perf enabled but no published RAF frame ID yet; stash and retry briefly.
    pendingJitMs += jitMs;
    maybeStartPendingJitFlushTimer();
    return;
  }

  const totalMs = pendingJitMs > 0 ? pendingJitMs + jitMs : jitMs;
  pendingJitMs = 0;
  stopPendingJitFlushTimer();
  writer.frameSample(frameId, { durations: { jit_ms: totalMs } });
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  if (isJitCompileRequest(ev.data)) {
    void handleCompile(ev.data);
    return;
  }
  if (isJitTier1CompileRequest(ev.data)) {
    void handleTier1Compile(ev.data);
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
        scanoutState: init.scanoutState,
        scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
        cursorState: init.cursorState,
        cursorStateOffsetBytes: init.cursorStateOffsetBytes ?? 0,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      const views = createSharedMemoryViews(segments);
      status = views.status;
      guestMemory = segments.guestMemory;
      guestBase = views.guestLayout.guest_base >>> 0;
      guestSize = views.guestLayout.guest_size >>> 0;
      canPostTier1Module = null;
      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
      pushEvent({ kind: "log", level: "info", message: "worker ready" });

      if (init.perfChannel) {
        perfWriter = new PerfWriter(init.perfChannel.buffer, {
          workerKind: init.perfChannel.workerKind,
          runStartEpochMs: init.perfChannel.runStartEpochMs,
        });
        perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
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

  // Warm up the Tier-1 compiler module in the background so the first hot block compile has
  // lower latency.
  void loadJitWasmApi().catch(() => {});

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
    if (perf.traceEnabled) perf.instant("jit:inflight_hit", "t", { key });
    try {
      const module = await inflight.promise;
      const durationMs = (inflight.endMs ?? performance.now()) - inflight.startMs;
      cacheSet(key, module);
      postJitResponse({ type: "jit:compiled", id: req.id, module, durationMs, cached: true });
    } catch (err) {
      const durationMs = (inflight.endMs ?? performance.now()) - inflight.startMs;
      const message = formatJitErrorMessage(err);
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

  const inflightEntry: InflightCompile = {
    startMs,
    endMs: null,
    // Placeholder; assigned below so the promise can reference the entry.
    promise: null as unknown as Promise<WebAssembly.Module>,
  };
  inflightEntry.promise = WebAssembly.compile(req.wasmBytes).then(
    (module) => {
      inflightEntry.endMs = performance.now();
      return module;
    },
    (err) => {
      inflightEntry.endMs = performance.now();
      throw err;
    },
  );
  inflightCompiles.set(key, inflightEntry);

  try {
    const module = await inflightEntry.promise;
    const durationMs = (inflightEntry.endMs ?? performance.now()) - inflightEntry.startMs;
    maybeWritePerfSample(durationMs);
    if (perf.traceEnabled) perf.instant("jit:compile:end", "t", { key, durationMs });

    cacheSet(key, module);
    postJitResponse({ type: "jit:compiled", id: req.id, module, durationMs });
  } catch (err) {
    const durationMs = (inflightEntry.endMs ?? performance.now()) - inflightEntry.startMs;
    maybeWritePerfSample(durationMs);
    const message = formatJitErrorMessage(err);
    const code = isCspBlockedError(err) ? "csp_blocked" : "compile_failed";
    if (code === "csp_blocked") {
      currentPlatformFeatures.jit_dynamic_wasm = false;
      recomputeJitEnabled();
    }
    postJitResponse({ type: "jit:error", id: req.id, code, message, durationMs });
  } finally {
    inflightCompiles.delete(key);
    perf.spanEnd("jit:compile");
  }
}

async function handleTier1Compile(req: JitTier1CompileRequest): Promise<void> {
  const startMs = performance.now();

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

  let api: JitWasmApi;
  try {
    api = await loadJitWasmApi();
  } catch (err) {
    const durationMs = performance.now() - startMs;
    const message = formatJitErrorMessage(err);
    const code = isCspBlockedError(err) ? "csp_blocked" : "compile_failed";
    if (code === "csp_blocked") {
      currentPlatformFeatures.jit_dynamic_wasm = false;
      recomputeJitEnabled();
    }
    postJitResponse({ type: "jit:error", id: req.id, code, message, durationMs });
    return;
  }

  const maxInsts = 64;
  let requestedMaxBytes = clampU32(req.maxBytes);
  if (requestedMaxBytes === 0) requestedMaxBytes = TIER1_DEFAULT_MAX_BYTES;
  // Clamp to keep the decode window (maxBytes + slack) within the compiler's input cap.
  const maxBytes = Math.min(requestedMaxBytes, TIER1_MAX_COMPILER_CODE_BYTES - TIER1_DECODE_WINDOW_SLACK_BYTES);

  const bitnessInput = req.bitness;
  const bitness = bitnessInput === 16 || bitnessInput === 32 || bitnessInput === 64 ? bitnessInput : 0;

  const entryRipBigint = typeof req.entryRip === "bigint" ? req.entryRip : BigInt(clampU32(req.entryRip));

  let codeBytes: Uint8Array;
  let fallbackCodeByteLen = 0;
  try {
    if (req.codeBytes) {
      // If the caller supplied code bytes, ensure we don't exceed the compiler's cap.
      const desiredLen = Math.min(req.codeBytes.byteLength, maxBytes + TIER1_DECODE_WINDOW_SLACK_BYTES);
      codeBytes = new Uint8Array(req.codeBytes.subarray(0, desiredLen));
      fallbackCodeByteLen = Math.min(maxBytes, codeBytes.byteLength);
    } else {
      // No explicit bytes provided: snapshot from the worker's shared guest memory.
      const entryRipU32 = (() => {
        if (typeof req.entryRip === "bigint") {
          if (req.entryRip < 0n || req.entryRip > 0xffff_ffffn) {
            throw new Error(`entryRip out of range: ${req.entryRip.toString(16)}`);
          }
          return Number(req.entryRip);
        }
        if (!Number.isFinite(req.entryRip) || req.entryRip < 0 || req.entryRip > 0xffff_ffff) {
          throw new Error(`invalid entryRip: ${String(req.entryRip)}`);
        }
        return req.entryRip >>> 0;
      })();
      const codeWindow = sliceTier1CodeWindow(entryRipU32, maxBytes);
      // Copy bytes into an unshared buffer so compilation cannot race with guest writes.
      codeBytes = new Uint8Array(codeWindow);
      fallbackCodeByteLen = Math.min(maxBytes, codeBytes.byteLength);
    }
  } catch (err) {
    const durationMs = performance.now() - startMs;
    const message = formatJitErrorMessage(err);
    postJitResponse({ type: "jit:error", id: req.id, code: "compile_failed", message, durationMs });
    return;
  }

  let compilation: Tier1BlockCompilation;
  try {
    let result: unknown;
    try {
      result = api.compile_tier1_block(
        entryRipBigint,
        codeBytes,
        maxInsts,
        maxBytes,
        true, // inlineTlb
        req.memoryShared,
        bitness,
      ) as unknown;
    } catch (err) {
      if (isTier1CompileError(err)) throw err;
      if (!isTier1AbiMismatchError(err)) throw err;
      // Backwards-compat: older JIT wasm builds used simpler argument lists.
      const compileTier1BlockCompat = api.compile_tier1_block as unknown as (...args: any[]) => unknown;
      try {
        result = compileTier1BlockCompat(Number(entryRipBigint), maxBytes) as unknown;
      } catch {
        result = compileTier1BlockCompat(codeBytes, Number(entryRipBigint), maxBytes) as unknown;
      }
    }

    // Older JIT WASM builds returned only the `Uint8Array` wasm bytes and did not expose the
    // decoded block length. Fall back to a conservative bound that never exceeds the provided
    // code slice or configured max.
    compilation = normalizeTier1Compilation(result, Math.min(maxBytes, fallbackCodeByteLen));
  } catch (err) {
    const durationMs = performance.now() - startMs;
    const message = formatJitErrorMessage(err);
    postJitResponse({ type: "jit:error", id: req.id, code: "compile_failed", message, durationMs });
    return;
  }

  const wasmBytes = toOwnedArrayBufferBytes(compilation.wasm_bytes);
  if (typeof WebAssembly.validate === "function") {
    if (!WebAssembly.validate(wasmBytes)) {
      const durationMs = performance.now() - startMs;
      postJitResponse({
        type: "jit:error",
        id: req.id,
        code: "compile_failed",
        message: "WebAssembly.validate failed for compiled Tier-1 block",
        durationMs,
      });
      return;
    }
  }

  const base = {
    type: "jit:tier1:compiled" as const,
    id: req.id,
    entryRip: req.entryRip,
    wasmBytes: wasmBytes.buffer,
    codeByteLen: compilation.code_byte_len,
    exitToInterpreter: compilation.exit_to_interpreter,
  };

  if (canPostTier1Module === false) {
    // `WebAssembly.Module` cannot be cloned in this environment: return raw bytes.
    const msg: JitTier1CompiledResponse = base;
    ctx.postMessage(msg, [wasmBytes.buffer]);
    return;
  }

  try {
    const module = await WebAssembly.compile(wasmBytes);

    // Prefer returning a compiled `WebAssembly.Module` (avoids compiling again in the CPU worker),
    // but fall back to raw bytes when structured cloning the module isn't supported.
    try {
      const msg: JitTier1CompiledResponse = { ...base, module };
      ctx.postMessage(msg, [wasmBytes.buffer]);
      canPostTier1Module = true;
    } catch (err) {
      if (canPostTier1Module === null && isDataCloneError(err)) canPostTier1Module = false;
      const msg: JitTier1CompiledResponse = base;
      ctx.postMessage(msg, [wasmBytes.buffer]);
    }
  } catch (err) {
    const durationMs = performance.now() - startMs;
    const message = formatJitErrorMessage(err);
    postJitResponse({ type: "jit:error", id: req.id, code: "compile_failed", message, durationMs });
  }
}

function postJitResponse(msg: JitWorkerResponse): void {
  try {
    ctx.postMessage(msg);
  } catch (err) {
    // Most commonly: `DataCloneError` when `WebAssembly.Module` is not structured-cloneable.
    const message = formatJitErrorMessage(err);
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
  try {
    while (true) {
      while (true) {
        const bytes = commandRing.tryPop();
        if (!bytes) break;
        let cmd: Command;
        try {
          cmd = decodeCommand(bytes);
        } catch {
          continue;
        }
        if (cmd.kind === "shutdown") {
          Atomics.store(status, StatusIndex.StopRequested, 1);
        }
      }

      if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;
      await waitForCommandRingDataNonBlocking();
    }
  } catch (err) {
    const message = formatJitErrorMessage(err);
    pushEventBlocking({ kind: "panic", message });
    setReadyFlag(status, role, false);
    ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
    return;
  }

  pushEvent({ kind: "log", level: "info", message: "worker shutdown" });
  setReadyFlag(status, role, false);
  ctx.close();
}

void currentConfig;

function pushEvent(evt: Event): void {
  if (!eventRing) return;
  eventRing.tryPush(encodeEvent(evt));
}

function pushEventBlocking(evt: Event, timeoutMs = 1000): void {
  if (!eventRing) return;
  const payload = encodeEvent(evt);
  if (eventRing.tryPush(payload)) return;
  try {
    eventRing.pushBlocking(payload, timeoutMs);
  } catch {
    // ignore
  }
}

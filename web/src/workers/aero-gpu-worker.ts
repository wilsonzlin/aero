/// <reference lib="webworker" />

import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";

import wasmInit, * as wasm from "../wasm/aero-gpu";

import type {
  BackendKind,
  FrameTimingsReport,
  GpuStats,
  GpuErrorCategory,
  GpuErrorEvent,
  GpuErrorSeverity,
  GpuWorkerErrorKind,
  GpuWorkerErrorPayload,
  GpuWorkerGpuErrorMessage,
  GpuWorkerErrorEventMessage,
  GpuWorkerIncomingMessage,
  GpuWorkerInitMessage,
  GpuWorkerOutgoingMessage,
  GpuWorkerReadyMessage,
  GpuWorkerRequestTimingsMessage,
  GpuWorkerResizeMessage,
  GpuWorkerScreenshotMessage,
  GpuWorkerStatsMessage,
  GpuWorkerTimingsMessage,
} from "../ipc/gpu-messages";

type WorkerScope = DedicatedWorkerGlobalScope;

const scope = self as unknown as WorkerScope;

void installWorkerPerfHandlers();

let initPromise: Promise<void> | null = null;
let isReady = false;
let lastBackendKind: BackendKind | null = null;
let pendingResize: GpuWorkerResizeMessage | null = null;
let lastInitMessage: GpuWorkerInitMessage | null = null;

let telemetryTimer: number | null = null;
let telemetryTickInFlight = false;
let fatalError = false;
let shutdownRequested = false;

let pendingWebGlRecovery = false;
let recoveryInProgress = false;
let autoReinitAttempted = false;

const LOCAL_STATS_DEFAULT: GpuStats = {
  presents_attempted: 0,
  presents_succeeded: 0,
  recoveries_attempted: 0,
  recoveries_succeeded: 0,
  surface_reconfigures: 0,
};

let localStats: GpuStats = { ...LOCAL_STATS_DEFAULT };

let messageQueue: Promise<void> = Promise.resolve();

let cssWidth = 0;
let cssHeight = 0;
let devicePixelRatio = 1;

function clampNonZero(n: number): number {
  if (!Number.isFinite(n)) return 1;
  return Math.max(1, Math.round(n));
}

function pixelSizeFromCss(width: number, height: number, dpr: number): { width: number; height: number } {
  const ratio = dpr || 1;
  return {
    width: clampNonZero(width * ratio),
    height: clampNonZero(height * ratio),
  };
}

function postMessage(msg: GpuWorkerOutgoingMessage, transfer: Transferable[] = []): void {
  scope.postMessage(msg, transfer);
}

function postErrorEvent(event: GpuErrorEvent): void {
  const message: GpuWorkerErrorEventMessage = { type: "gpu_error_event", event };
  postMessage(message);
}

function emitErrorEvent(
  severity: GpuErrorSeverity,
  category: GpuErrorCategory,
  message: string,
  details?: Record<string, unknown>,
): void {
  postErrorEvent({
    time_ms: Date.now(),
    backend_kind: lastBackendKind ?? "webgl2",
    severity,
    category,
    message,
    ...(details ? { details } : {}),
  });
}

function toErrorPayload(kind: GpuWorkerErrorKind, err: unknown, hints: string[] = []): GpuWorkerErrorPayload {
  if (err instanceof Error) {
    return { kind, message: err.message, stack: err.stack, hints: hints.length ? hints : undefined };
  }
  return { kind, message: String(err), hints: hints.length ? hints : undefined };
}

function isGpuWorkerErrorPayload(value: unknown): value is GpuWorkerErrorPayload {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return typeof record.kind === "string" && typeof record.message === "string";
}

function sendGpuError(msg: GpuWorkerGpuErrorMessage): void {
  postMessage(msg);
}

function forwardFatal(kind: GpuWorkerErrorKind, err: unknown, hints: string[] = []): void {
  fatalError = true;
  isReady = false;
  stopTelemetry();
  sendGpuError({ type: "gpu_error", fatal: true, error: toErrorPayload(kind, err, hints) });
}

function forwardNonFatal(kind: GpuWorkerErrorKind, err: unknown, hints: string[] = []): void {
  sendGpuError({ type: "gpu_error", fatal: false, error: toErrorPayload(kind, err, hints) });
}

scope.addEventListener("error", (event) => {
  const err = (event as ErrorEvent).error ?? event;
  forwardNonFatal("unexpected", err);
});

scope.addEventListener("unhandledrejection", (event) => {
  forwardNonFatal("unexpected", (event as PromiseRejectionEvent).reason);
});

async function callMaybeAsync<T>(fn: () => T | Promise<T>): Promise<T> {
  return await fn();
}

function normalizeInitSize(message: { width: number; height: number; devicePixelRatio: number }): {
  width: number;
  height: number;
  devicePixelRatio: number;
} {
  const ratio = message.devicePixelRatio || 1;
  // Avoid initializing a 0x0 surface: many backends consider this invalid.
  return {
    width: Math.max(0, Math.round(message.width)),
    height: Math.max(0, Math.round(message.height)),
    devicePixelRatio: ratio,
  };
}

function detectWebGpuSupport(): boolean {
  return (
    typeof (self as unknown as { navigator?: unknown }).navigator !== "undefined" &&
    typeof (navigator as unknown as { gpu?: unknown }).gpu !== "undefined"
  );
}

function attachWebGlContextLossListeners(canvas: OffscreenCanvas): void {
  // Some environments may not dispatch context loss events on OffscreenCanvas.
  // Treat listener registration as best-effort to avoid crashing the worker.
  const target = canvas as unknown as {
    addEventListener?: (
      type: string,
      listener: (ev: Event) => void,
      options?: boolean | AddEventListenerOptions,
    ) => void;
  };
  if (typeof target.addEventListener !== "function") return;

  try {
    target.addEventListener(
      "webglcontextlost",
      (ev) => {
        // Keep control of restore semantics; coordinate with Rust/main-thread state.
        ev.preventDefault();
        handleDeviceLost({
          source: "webgl2",
          message: "WebGL2 context lost",
        });
        forwardNonFatal("unexpected", new Error("WebGL2 context lost"));
      },
      { passive: false },
    );

    target.addEventListener("webglcontextrestored", () => {
      if (!pendingWebGlRecovery) return;
      pendingWebGlRecovery = false;
      emitErrorEvent("Info", "DeviceLost", "WebGL2 context restored; attempting re-init");
      void attemptAutoReinit("webgl2").catch((err) => forwardNonFatal("unexpected", err));
    });
  } catch (err) {
    // Ignore; context loss events are non-essential.
    emitErrorEvent("Warning", "Unknown", "Failed to register WebGL context loss listeners", {
      error: err instanceof Error ? err.message : String(err),
    });
  }
}

async function initWithFallback(message: GpuWorkerInitMessage): Promise<GpuWorkerReadyMessage> {
  const normalized = normalizeInitSize(message);
  cssWidth = normalized.width;
  cssHeight = normalized.height;
  devicePixelRatio = normalized.devicePixelRatio;

  const userOptions = message.gpuOptions ?? {};
  const preferWebGpu = userOptions.preferWebGpu !== false;
  const disableWebGpu = userOptions.disableWebGpu === true;

  // Always clamp to a non-zero physical size for init.
  const initCssWidth = cssWidth || 1;
  const initCssHeight = cssHeight || 1;

  let fallback: GpuWorkerReadyMessage["fallback"];

  if (preferWebGpu) {
    if (disableWebGpu || !detectWebGpuSupport()) {
      try {
        await wasm.init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
          ...userOptions,
          preferWebGpu: false,
        });
        fallback = {
          from: "webgpu",
          to: "webgl2",
          reason: disableWebGpu ? "WebGPU disabled by configuration." : "WebGPU not supported in this environment.",
        };
      } catch (err) {
        throw toErrorPayload("webgl2_not_supported", err, [
          "WebGPU is unavailable and WebGL2 context creation failed.",
          "Ensure hardware acceleration is enabled in your browser settings.",
        ]);
      }
    } else {
      try {
        await wasm.init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
          ...userOptions,
          preferWebGpu: true,
        });
      } catch (webGpuErr) {
        try {
          await wasm.init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
            ...userOptions,
            preferWebGpu: false,
          });
          fallback = {
            from: "webgpu",
            to: "webgl2",
            reason: "WebGPU initialization failed; fell back to WebGL2.",
            originalErrorMessage: webGpuErr instanceof Error ? webGpuErr.message : String(webGpuErr),
          };
        } catch (webGlErr) {
          throw toErrorPayload("webgpu_init_failed", webGpuErr, [
            "WebGPU initialization failed.",
            "WebGL2 fallback also failed; ensure hardware acceleration is enabled and WebGL is not disabled.",
            webGlErr instanceof Error ? `WebGL2 error: ${webGlErr.message}` : `WebGL2 error: ${String(webGlErr)}`,
          ]);
        }
      }
    }
  } else {
    try {
      await wasm.init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
        ...userOptions,
        preferWebGpu: false,
      });
    } catch (err) {
      throw toErrorPayload("webgl2_init_failed", err, [
        "WebGL2 initialization failed.",
        "Ensure hardware acceleration is enabled and WebGL is not disabled.",
      ]);
    }
  }

  const backendKind = wasm.backend_kind();
  lastBackendKind = backendKind;
  const caps = wasm.capabilities();
  let adapterInfo;
  try {
    adapterInfo = wasm.adapter_info();
  } catch {
    adapterInfo = undefined;
  }

  // If the backend selected WebGL2 despite a WebGPU preference and we didn't
  // record a reason (e.g. internal fallback inside wasm), report it as fallback.
  if (preferWebGpu && backendKind === "webgl2" && !fallback) {
    fallback = {
      from: "webgpu",
      to: "webgl2",
      reason: "WebGPU was preferred but WebGL2 was selected.",
    };
  }

  // Apply any resize that arrived while initializing.
  if (pendingResize) {
    const resizeMsg = pendingResize;
    pendingResize = null;
    await applyResize(resizeMsg);
  } else if (cssWidth !== initCssWidth || cssHeight !== initCssHeight) {
    // We clamped a 0x0 init; keep surface in sync when real size is known.
    await applyResize({ type: "resize", width: cssWidth, height: cssHeight, devicePixelRatio });
  }

  return {
    type: "ready",
    backendKind,
    capabilities: caps,
    adapterInfo,
    fallback,
  };
}

async function applyResize(message: GpuWorkerResizeMessage): Promise<void> {
  cssWidth = Math.max(0, Math.round(message.width));
  cssHeight = Math.max(0, Math.round(message.height));
  devicePixelRatio = message.devicePixelRatio || 1;

  // Avoid asking the backend to create a 0x0 swapchain.
  const safeWidth = cssWidth || 1;
  const safeHeight = cssHeight || 1;
  localStats.surface_reconfigures += 1;
  wasm.resize(safeWidth, safeHeight, devicePixelRatio);
}

type TelemetryWasmExports = {
  get_gpu_stats?: () => unknown | Promise<unknown>;
  getGpuStats?: () => unknown | Promise<unknown>;
  drain_gpu_events?: () => unknown | Promise<unknown>;
  drain_gpu_error_events?: () => unknown | Promise<unknown>;
  take_gpu_events?: () => unknown | Promise<unknown>;
  take_gpu_error_events?: () => unknown | Promise<unknown>;
  drainGpuEvents?: () => unknown | Promise<unknown>;
};

function stopTelemetry(): void {
  if (telemetryTimer === null) return;
  clearInterval(telemetryTimer);
  telemetryTimer = null;
}

function startTelemetry(): void {
  if (telemetryTimer !== null) return;
  if (fatalError || shutdownRequested) return;

  // 3 Hz is low enough to avoid overhead but fast enough for UI debugging.
  telemetryTimer = setInterval(() => {
    void tickTelemetry().catch((err) => forwardNonFatal("unexpected", err));
  }, 333) as unknown as number;

  // Emit first stats/event batch immediately rather than waiting one interval.
  void tickTelemetry().catch((err) => forwardNonFatal("unexpected", err));
}

function coerceNumber(value: unknown): number | null {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value === "bigint") return Number(value);
  if (typeof value === "string") {
    const n = Number(value);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

function parseGpuStats(raw: unknown): GpuStats | null {
  if (raw == null) return null;

  let data: unknown = raw;
  if (typeof raw === "string") {
    try {
      data = JSON.parse(raw);
    } catch {
      return null;
    }
  }

  if (!data || typeof data !== "object") return null;
  const record = data as Record<string, unknown>;

  const fields: Array<keyof GpuStats> = [
    "presents_attempted",
    "presents_succeeded",
    "recoveries_attempted",
    "recoveries_succeeded",
    "surface_reconfigures",
  ];

  const out: Partial<GpuStats> = {};
  for (const key of fields) {
    const val = coerceNumber(record[key]);
    // Some runtimes may add fields incrementally; default missing/invalid values
    // to zero so the UI still receives a stable stats object.
    out[key] = val ?? 0;
  }

  return out as GpuStats;
}

function normalizeWasmEvents(raw: unknown): unknown[] {
  if (raw == null) return [];
  if (typeof raw === "string") {
    try {
      const parsed = JSON.parse(raw) as unknown;
      return normalizeWasmEvents(parsed);
    } catch {
      return [];
    }
  }
  if (Array.isArray(raw)) return raw;
  if (typeof raw === "object") return [raw];
  return [];
}

function parseGpuErrorEvent(raw: unknown): GpuErrorEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const record = raw as Record<string, unknown>;

  const message = typeof record.message === "string" ? record.message : String(record.message ?? "");
  if (!message) return null;

  const time = coerceNumber(record.time_ms) ?? Date.now();

  const backendKind =
    record.backend_kind === "webgpu" || record.backend_kind === "webgl2"
      ? (record.backend_kind as BackendKind)
      : lastBackendKind ?? "webgl2";

  const severity =
    record.severity === "Info" || record.severity === "Warning" || record.severity === "Error" || record.severity === "Fatal"
      ? (record.severity as GpuErrorSeverity)
      : "Error";

  const category =
    record.category === "Init" ||
    record.category === "DeviceLost" ||
    record.category === "Surface" ||
    record.category === "ShaderCompile" ||
    record.category === "PipelineCreate" ||
    record.category === "Validation" ||
    record.category === "OutOfMemory" ||
    record.category === "Unknown"
      ? (record.category as GpuErrorCategory)
      : "Unknown";

  const details =
    record.details && typeof record.details === "object" ? (record.details as Record<string, unknown>) : undefined;

  return {
    time_ms: time,
    backend_kind: backendKind,
    severity,
    category,
    message,
    ...(details ? { details } : {}),
  };
}

async function drainWasmEvents(): Promise<void> {
  const exportsRecord = wasm as unknown as TelemetryWasmExports;
  const drainFn =
    exportsRecord.drain_gpu_events ??
    exportsRecord.drain_gpu_error_events ??
    exportsRecord.take_gpu_events ??
    exportsRecord.take_gpu_error_events ??
    exportsRecord.drainGpuEvents;

  if (typeof drainFn !== "function") return;

  let raw: unknown;
  try {
    raw = await callMaybeAsync(() => drainFn());
  } catch (err) {
    // If draining fails it likely means the runtime is in a bad state; keep
    // this non-fatal so the caller can attempt a restart/re-init.
    forwardNonFatal("unexpected", err);
    return;
  }

  for (const entry of normalizeWasmEvents(raw)) {
    const event = parseGpuErrorEvent(entry);
    if (!event) continue;

    postErrorEvent(event);

    if (event.category === "DeviceLost" && (event.severity === "Error" || event.severity === "Fatal")) {
      handleDeviceLost({
        source: "wasm",
        message: event.message,
        originalEvent: event,
      });
    }
  }
}

async function pollWasmStats(): Promise<GpuStats | null> {
  const exportsRecord = wasm as unknown as TelemetryWasmExports;
  const statsFn = exportsRecord.get_gpu_stats ?? exportsRecord.getGpuStats;
  if (typeof statsFn !== "function") return null;

  try {
    const raw = await callMaybeAsync(() => statsFn());
    return parseGpuStats(raw);
  } catch (err) {
    forwardNonFatal("unexpected", err);
    return null;
  }
}

async function tickTelemetry(): Promise<void> {
  if (telemetryTickInFlight) return;
  if (fatalError || shutdownRequested) return;
  if (!initPromise) return;

  telemetryTickInFlight = true;
  try {
    await drainWasmEvents();

    // Stats are only meaningful when the backend has been initialized. Emit
    // local fallback counters even if the wasm runtime doesn't yet expose stats.
    if (isReady) {
      const stats = (await pollWasmStats()) ?? localStats;
      const msg: GpuWorkerStatsMessage = { type: "gpu_stats", stats };
      postMessage(msg);
    }
  } finally {
    telemetryTickInFlight = false;
  }
}

function handleDeviceLost(args: { source: "webgl2" | "wasm"; message: string; originalEvent?: GpuErrorEvent }): void {
  if (fatalError || shutdownRequested) return;
  if (!initPromise) return;

  isReady = false;
  stopTelemetry();

  if (args.source === "webgl2") {
    pendingWebGlRecovery = true;
    emitErrorEvent("Error", "DeviceLost", args.message);
  }

  void attemptAutoReinit(args.source).catch((err) => forwardNonFatal("unexpected", err));
}

async function attemptAutoReinit(source: "webgl2" | "wasm"): Promise<void> {
  if (fatalError || shutdownRequested) return;
  if (recoveryInProgress) return;
  if (autoReinitAttempted) return;
  if (!lastInitMessage) return;

  // Avoid racing the initial init path: if we're still booting, wait for init to
  // settle before attempting recovery.
  if (initPromise) {
    await initPromise;
  }

  if (fatalError || shutdownRequested) return;
  if (isReady) return;

  // WebGL2 recovery must wait for the browser to restore the context.
  if (source === "webgl2" && pendingWebGlRecovery) return;

  recoveryInProgress = true;
  autoReinitAttempted = true;
  localStats.recoveries_attempted += 1;

  try {
    emitErrorEvent("Info", "DeviceLost", "Attempting GPU re-init after device loss", { source });

    const msg: GpuWorkerInitMessage = {
      type: "init",
      canvas: lastInitMessage.canvas,
      width: cssWidth,
      height: cssHeight,
      devicePixelRatio,
      gpuOptions: lastInitMessage.gpuOptions,
    };

    const ready = await initWithFallback(msg);

    isReady = true;
    pendingWebGlRecovery = false;
    autoReinitAttempted = false;
    localStats.recoveries_succeeded += 1;

    // Inform the main thread that rendering has resumed. We avoid re-sending the
    // `ready` message because callers treat it as a one-time init handshake.
    emitErrorEvent("Info", "DeviceLost", "GPU re-init succeeded; rendering resumed", {
      backend_kind: ready.backendKind,
      ...(ready.fallback ? { fallback: ready.fallback } : {}),
    });

    startTelemetry();
  } catch (err) {
    fatalError = true;
    isReady = false;
    stopTelemetry();

    emitErrorEvent("Fatal", "DeviceLost", "GPU re-init failed; rendering stopped", {
      error: err instanceof Error ? err.message : String(err),
    });

    const payload = isGpuWorkerErrorPayload(err) ? err : toErrorPayload("unexpected", err);
    sendGpuError({ type: "gpu_error", fatal: true, error: payload });
  } finally {
    recoveryInProgress = false;
  }
}

async function handleInit(message: GpuWorkerInitMessage): Promise<void> {
  if (initPromise) {
    forwardNonFatal("unexpected", new Error("aero-gpu-worker received duplicate init message."));
    return;
  }

  perf.spanBegin("worker:boot");
  perf.spanBegin("worker:init");
  lastInitMessage = message;
  attachWebGlContextLossListeners(message.canvas);

  initPromise = (async () => {
    try {
      try {
        await perf.spanAsync("wasm:init", () => wasmInit());
      } catch (err) {
        forwardFatal("wasm_init_failed", err, [
          "Failed to initialize the aero-gpu wasm module.",
          "Ensure the wasm bundle is correctly built and served with the correct MIME type.",
        ]);
        return;
      }

      try {
        const ready = await perf.spanAsync("gpu:init", () => initWithFallback(message));
        isReady = true;
        fatalError = false;
        autoReinitAttempted = false;
        localStats = { ...LOCAL_STATS_DEFAULT };
        postMessage(ready);
        startTelemetry();
        if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role: "aero-gpu" });
      } catch (err) {
        // `initWithFallback` throws `GpuWorkerErrorPayload` on purpose so the main
        // thread can show actionable hints.
        const payload = isGpuWorkerErrorPayload(err) ? err : toErrorPayload("unexpected", err);
        fatalError = true;
        stopTelemetry();
        sendGpuError({ type: "gpu_error", fatal: true, error: payload });
      }
    } finally {
      perf.spanEnd("worker:init");
      perf.spanEnd("worker:boot");
    }
  })();

  await initPromise;
}

async function handleResize(message: GpuWorkerResizeMessage): Promise<void> {
  if (!initPromise || !isReady) {
    pendingResize = message;
    return;
  }
  await applyResize(message);
}

async function handlePresentTestPattern(): Promise<void> {
  if (!initPromise || !isReady) return;
  try {
    localStats.presents_attempted += 1;
    await callMaybeAsync(() => wasm.present_test_pattern());
    localStats.presents_succeeded += 1;
  } catch (err) {
    forwardNonFatal("unexpected", err);
  }
}

function arrayBufferFromUnknown(value: unknown): ArrayBuffer {
  if (value instanceof ArrayBuffer) return value;
  if (ArrayBuffer.isView(value)) {
    return value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
  }
  throw new Error("Screenshot result was not an ArrayBuffer or typed array.");
}

async function handleRequestScreenshot(requestId: number): Promise<void> {
  if (!initPromise || !isReady) {
    // Keep caller-side promises from hanging forever; return a 1x1 black pixel.
    const rgba8 = new Uint8Array([0, 0, 0, 255]).buffer;
    const message: GpuWorkerScreenshotMessage = {
      type: "screenshot",
      requestId,
      width: 1,
      height: 1,
      rgba8,
      origin: "top-left",
    };
    postMessage(message, [rgba8]);
    return;
  }

  try {
    const result = await callMaybeAsync(() => wasm.request_screenshot());
    const rgba8 = arrayBufferFromUnknown(result);
    const pixels = pixelSizeFromCss(cssWidth || 1, cssHeight || 1, devicePixelRatio);

    const message: GpuWorkerScreenshotMessage = {
      type: "screenshot",
      requestId,
      width: pixels.width,
      height: pixels.height,
      rgba8,
      origin: "top-left",
    };

    postMessage(message, [rgba8]);
  } catch (err) {
    forwardNonFatal("unexpected", err);
    // Ensure the main thread doesn't hang waiting for a response.
    const rgba8 = new Uint8Array([0, 0, 0, 255]).buffer;
    const message: GpuWorkerScreenshotMessage = {
      type: "screenshot",
      requestId,
      width: 1,
      height: 1,
      rgba8,
      origin: "top-left",
    };
    postMessage(message, [rgba8]);
  }
}

async function handleRequestTimings(message: GpuWorkerRequestTimingsMessage): Promise<void> {
  if (!initPromise || !isReady) {
    const response: GpuWorkerTimingsMessage = { type: "timings", requestId: message.requestId, timings: null };
    postMessage(response);
    return;
  }

  try {
    const timings = (await callMaybeAsync(() => wasm.get_frame_timings())) as FrameTimingsReport | null;
    const response: GpuWorkerTimingsMessage = { type: "timings", requestId: message.requestId, timings };
    postMessage(response);
  } catch (err) {
    forwardNonFatal("unexpected", err);
    const response: GpuWorkerTimingsMessage = { type: "timings", requestId: message.requestId, timings: null };
    postMessage(response);
  }
}

async function handleShutdown(): Promise<void> {
  // Best-effort: the wasm side may expose explicit cleanup hooks in the future.
  shutdownRequested = true;
  stopTelemetry();
  scope.close();
}

scope.addEventListener("message", (event: MessageEvent) => {
  const message = event.data as GpuWorkerIncomingMessage;
  if (!message || typeof message !== "object" || typeof (message as { type?: unknown }).type !== "string") {
    return;
  }

  if (message.type.startsWith("aero:perf:")) return;

  const run = async () => {
    switch (message.type) {
      case "init":
        await handleInit(message);
        break;
      case "resize":
        await handleResize(message);
        break;
      case "present_test_pattern":
        await handlePresentTestPattern();
        break;
      case "request_screenshot":
        await handleRequestScreenshot(message.requestId);
        break;
      case "request_timings":
        await handleRequestTimings(message);
        break;
      case "shutdown":
        await handleShutdown();
        break;
      default:
        forwardNonFatal(
          "unexpected",
          new Error(`Unknown aero-gpu-worker message type: ${(message as { type: string }).type}`),
        );
        break;
    }
  };

  messageQueue = messageQueue.then(run, run).catch((err) => {
    forwardNonFatal("unexpected", err);
  });
});

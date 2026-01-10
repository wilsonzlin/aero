/// <reference lib="webworker" />

import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";

import wasmInit, {
  adapter_info,
  backend_kind,
  capabilities,
  init_gpu,
  present_test_pattern,
  request_screenshot,
  resize,
} from "../wasm/aero-gpu";

import type {
  GpuWorkerErrorKind,
  GpuWorkerErrorPayload,
  GpuWorkerGpuErrorMessage,
  GpuWorkerErrorEventMessage,
  GpuWorkerIncomingMessage,
  GpuWorkerInitMessage,
  GpuWorkerOutgoingMessage,
  GpuWorkerReadyMessage,
  GpuWorkerResizeMessage,
  GpuWorkerScreenshotMessage,
  GpuErrorEvent,
  GpuErrorSeverity,
  GpuErrorCategory,
  BackendKind,
} from "../ipc/gpu-messages";

type WorkerScope = DedicatedWorkerGlobalScope;

const scope = self as unknown as WorkerScope;

void installWorkerPerfHandlers();

let initPromise: Promise<void> | null = null;
let isReady = false;
let lastBackendKind: BackendKind | null = null;
let pendingResize: GpuWorkerResizeMessage | null = null;

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
        isReady = false;
        emitErrorEvent("Error", "DeviceLost", "WebGL2 context lost");
        forwardNonFatal("unexpected", new Error("WebGL2 context lost"));
      },
      { passive: false },
    );

    target.addEventListener("webglcontextrestored", () => {
      // The underlying GL context object is no longer valid; require a wasm-side
      // re-init (or full worker restart) before resuming rendering.
      isReady = false;
      emitErrorEvent("Info", "DeviceLost", "WebGL2 context restored; re-init required");
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
        await init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
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
        await init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
          ...userOptions,
          preferWebGpu: true,
        });
      } catch (webGpuErr) {
        try {
          await init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
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
      await init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
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

  const backendKind = backend_kind();
  lastBackendKind = backendKind;
  const caps = capabilities();
  let adapterInfo;
  try {
    adapterInfo = adapter_info();
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
  resize(safeWidth, safeHeight, devicePixelRatio);
}

async function handleInit(message: GpuWorkerInitMessage): Promise<void> {
  if (initPromise) {
    forwardNonFatal("unexpected", new Error("aero-gpu-worker received duplicate init message."));
    return;
  }

  perf.spanBegin("worker:boot");
  perf.spanBegin("worker:init");
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
        postMessage(ready);
        perf.instant("boot:worker:ready", "p", { role: "aero-gpu" });
      } catch (err) {
        // `initWithFallback` throws `GpuWorkerErrorPayload` on purpose so the main
        // thread can show actionable hints.
        const payload = isGpuWorkerErrorPayload(err) ? err : toErrorPayload("unexpected", err);
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
    await callMaybeAsync(() => present_test_pattern());
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
  if (!initPromise || !isReady) return;

  try {
    const result = await callMaybeAsync(() => request_screenshot());
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
  }
}

async function handleShutdown(): Promise<void> {
  // Best-effort: the wasm side may expose explicit cleanup hooks in the future.
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

/// <reference lib="webworker" />

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
  GpuWorkerIncomingMessage,
  GpuWorkerInitMessage,
  GpuWorkerOutgoingMessage,
  GpuWorkerReadyMessage,
  GpuWorkerResizeMessage,
  GpuWorkerScreenshotMessage,
} from "../ipc/gpu-messages";

type WorkerScope = DedicatedWorkerGlobalScope;

const scope = self as unknown as WorkerScope;

let initPromise: Promise<void> | null = null;
let isReady = false;
let pendingResize: GpuWorkerResizeMessage | null = null;

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

async function initWithFallback(message: GpuWorkerInitMessage): Promise<GpuWorkerReadyMessage> {
  const normalized = normalizeInitSize(message);
  cssWidth = normalized.width;
  cssHeight = normalized.height;
  devicePixelRatio = normalized.devicePixelRatio;

  const userOptions = message.gpuOptions ?? {};
  const preferWebGpu = userOptions.preferWebGpu !== false;

  // Always clamp to a non-zero physical size for init.
  const initCssWidth = cssWidth || 1;
  const initCssHeight = cssHeight || 1;

  let fallback: GpuWorkerReadyMessage["fallback"];

  if (preferWebGpu) {
    if (!detectWebGpuSupport()) {
      try {
        await init_gpu(message.canvas, initCssWidth, initCssHeight, devicePixelRatio, {
          ...userOptions,
          preferWebGpu: false,
        });
        fallback = {
          from: "webgpu",
          to: "webgl2",
          reason: "WebGPU not supported in this environment.",
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

  initPromise = (async () => {
    try {
      await wasmInit();
    } catch (err) {
      forwardFatal("wasm_init_failed", err, [
        "Failed to initialize the aero-gpu wasm module.",
        "Ensure the wasm bundle is correctly built and served with the correct MIME type.",
      ]);
      return;
    }

    try {
      const ready = await initWithFallback(message);
      isReady = true;
      postMessage(ready);
    } catch (err) {
      // `initWithFallback` throws `GpuWorkerErrorPayload` on purpose so the main
      // thread can show actionable hints.
      const payload = isGpuWorkerErrorPayload(err) ? err : toErrorPayload("unexpected", err);
      sendGpuError({ type: "gpu_error", fatal: true, error: payload });
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

  switch (message.type) {
    case "init":
      void handleInit(message);
      break;
    case "resize":
      void handleResize(message);
      break;
    case "present_test_pattern":
      void handlePresentTestPattern();
      break;
    case "request_screenshot":
      void handleRequestScreenshot(message.requestId);
      break;
    case "shutdown":
      void handleShutdown();
      break;
    default:
      forwardNonFatal("unexpected", new Error(`Unknown aero-gpu-worker message type: ${(message as { type: string }).type}`));
      break;
  }
});

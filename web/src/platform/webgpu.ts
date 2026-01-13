export type RequestWebGpuDeviceOptions = {
  powerPreference?: GPUPowerPreference;
  /**
   * Optional callback for WebGPU "uncaptured" errors (validation, pipeline creation, etc).
   *
   * When unset, errors are logged to `console.error` so they aren't silently dropped.
   */
  onUncapturedError?: (error: unknown) => void;
};

export type WebGpuDeviceInfo = {
  adapter: GPUAdapter;
  device: GPUDevice;
  preferredFormat: GPUTextureFormat;
};

function getNavigatorGpu(): GPU | undefined {
  if (typeof navigator === "undefined") return undefined;
  return (navigator as Navigator & { gpu?: GPU }).gpu;
}

/**
 * Requests a WebGPU adapter/device.
 *
 * This helper is safe to call from both the main thread and dedicated workers.
 *
 * For now we intentionally request no required features/limits to maximize
 * compatibility while the rendering stack is still being built out.
 *
 * Planned (see docs/11-browser-apis.md) for later:
 * - requiredFeatures: ['texture-compression-bc', 'texture-compression-etc2', 'texture-compression-astc', 'float32-filterable']
 * - requiredLimits: { maxStorageBufferBindingSize, maxBufferSize, ... }
 */
export async function requestWebGpuDevice(
  options: RequestWebGpuDeviceOptions = {},
): Promise<WebGpuDeviceInfo> {
  const gpu = getNavigatorGpu();
  if (!gpu) {
    throw new Error("WebGPU is not available in this browser/context (navigator.gpu is missing).");
  }

  const adapter = await gpu.requestAdapter({
    powerPreference: options.powerPreference,
  });
  if (!adapter) {
    throw new Error("WebGPU adapter request failed (navigator.gpu.requestAdapter returned null).");
  }

  const device = await adapter.requestDevice({
    // Keep empty for now (see docstring above).
  });

  // Surface async validation/pipeline errors (which do not always throw) for debugging.
  const customUncapturedHandler = options.onUncapturedError;
  const hasCustomUncapturedHandler = typeof customUncapturedHandler === "function";
  const onUncapturedError = hasCustomUncapturedHandler
    ? customUncapturedHandler!
    : (error: unknown) => console.error("[webgpu] uncapturederror", error);
  const seenErrorKeys = new Set<string>();
  const uncapturedHandler = (ev: any) => {
    try {
      // Avoid double-reporting when cancelable.
      (ev as any).preventDefault?.();
    } catch {
      // Ignore.
    }
    const error = ev?.error ?? ev;
    if (!hasCustomUncapturedHandler) {
      // Avoid flooding the console with the same validation error over and over.
      const err = ev?.error;
      const errorName =
        (typeof err?.name === "string" && err.name) ||
        (typeof err?.constructor?.name === "string" && err.constructor.name) ||
        "";
      const errorMessage = typeof err?.message === "string" ? err.message : "";
      let msg = errorMessage || (err != null ? String(err) : "WebGPU uncaptured error");
      if (errorName && msg && !msg.toLowerCase().startsWith(errorName.toLowerCase())) {
        msg = `${errorName}: ${msg}`;
      }
      const key = msg;
      if (seenErrorKeys.has(key)) return;
      seenErrorKeys.add(key);
      if (seenErrorKeys.size > 128) {
        seenErrorKeys.clear();
        seenErrorKeys.add(key);
      }
    }

    try {
      onUncapturedError(error);
    } catch {
      // Ignore.
    }
  };
  try {
    if (typeof (device as any).addEventListener === "function") {
      (device as any).addEventListener("uncapturederror", uncapturedHandler);
    } else {
      (device as any).onuncapturederror = uncapturedHandler;
    }
  } catch {
    // Best-effort; ignore.
  }

  return {
    adapter,
    device,
    preferredFormat: gpu.getPreferredCanvasFormat(),
  };
}

export function createWebGpuCanvasContext(
  canvas: HTMLCanvasElement | OffscreenCanvas,
  device: GPUDevice,
  format?: GPUTextureFormat,
): GPUCanvasContext {
  const context = (canvas as unknown as { getContext(type: "webgpu"): GPUCanvasContext | null }).getContext(
    "webgpu",
  );
  if (!context) {
    throw new Error('Failed to acquire WebGPU canvas context (getContext("webgpu") returned null).');
  }

  const gpu = getNavigatorGpu();
  const resolvedFormat = format ?? gpu?.getPreferredCanvasFormat?.() ?? ("bgra8unorm" as GPUTextureFormat);

  context.configure({
    device,
    format: resolvedFormat,
    // Keep configuration minimal for now; callers can extend once we standardize
    // presentation usage, alphaMode, and viewFormats.
  });

  return context;
}

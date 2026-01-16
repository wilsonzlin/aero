import { createGpuWorker } from "./src/main/createGpuWorker";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "./src/ipc/gpu-protocol";
import { fnv1a32Hex } from "./src/utils/fnv1a";
import { formatOneLineError } from "./src/text";
import { aerogpuFormatToString } from "../emulator/protocol/aerogpu/aerogpu_pci.ts";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      fallback?: {
        from: string;
        to: string;
        reason: string;
      };
      error?: string;
      hash?: string;
      expectedHash?: string;
      pass?: boolean;
      lastStats?: unknown;
      lastWasmStats?: unknown;
      events?: unknown[];
      samplePixels?: () => Promise<{
        backend: string;
        width: number;
        height: number;
        topLeft: number[];
        topRight: number[];
        bottomLeft: number[];
        bottomRight: number[];
      }>;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function getBoolParam(name: string): boolean {
  const url = new URL(window.location.href);
  const value = url.searchParams.get(name);
  if (value === null) return false;
  return value !== "0" && value !== "false";
}

function getStringParam(name: string): string | null {
  const url = new URL(window.location.href);
  const value = url.searchParams.get(name);
  if (value == null || value === "") return null;
  return value;
}

function getBackendParam(): "auto" | "webgpu" | "webgl2" {
  const url = new URL(window.location.href);
  const value = url.searchParams.get("backend");
  if (value === "webgpu" || value === "webgl2") return value;
  return "auto";
}

function renderError(message: string) {
  const status = $("status");
  if (status) {
    // Preserve earlier diagnostics lines (gpu_event/gpu_error) when we fail after already
    // receiving events from the worker.
    if (status.textContent && status.textContent.length > 0) {
      status.textContent += `${message}\n`;
    } else {
      status.textContent = message;
    }
  }
  window.__aeroTest = { ...(window.__aeroTest ?? {}), ready: true, error: message };
}

function safeJsonStringify(value: unknown): string {
  try {
    return JSON.stringify(value, (_key, v) => (typeof v === "bigint" ? v.toString() : v));
  } catch {
    try {
      return String(value);
    } catch {
      return "<unprintable>";
    }
  }
}

function fmtScanoutSource(source: unknown): string {
  const n = typeof source === "number" ? source : NaN;
  switch (n) {
    case 0:
      return "LegacyText";
    case 1:
      return "LegacyVbeLfb";
    case 2:
      return "Wddm";
    default:
      return typeof source === "number" ? String(source) : "n/a";
  }
}

function createExpectedTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const left = x < halfW;
      const top = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      if (top && left) {
        out[i + 0] = 255;
        out[i + 1] = 0;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (top && !left) {
        out[i + 0] = 0;
        out[i + 1] = 255;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (!top && left) {
        out[i + 0] = 0;
        out[i + 1] = 0;
        out[i + 2] = 255;
        out[i + 3] = 255;
      } else {
        out[i + 0] = 255;
        out[i + 1] = 255;
        out[i + 2] = 255;
        out[i + 3] = 255;
      }
    }
  }

  return out;
}

async function main() {
  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError("Canvas element not found");
    return;
  }

  const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

  const status = $("status");
  const telemetryEl = $("telemetry");
  const backendEl = $("backend");
  let loggedFrameTimings = false;
  const eventLog: Array<{ time_ms: number; backend_kind: string; severity: string; category: string; message: string }> = [];

  const cssWidth = 64;
  const cssHeight = 64;
  const devicePixelRatio = 1;

  canvas.width = cssWidth * devicePixelRatio;
  canvas.height = cssHeight * devicePixelRatio;
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;

  try {
    let statsLinesWritten = 0;
    const requestedBackend = getBackendParam();
    const disableWebGpu = getBoolParam("disableWebGpu");
    const triggerPresenterError = getBoolParam("triggerPresenterError");
    const expectInitFailure = getBoolParam("expectInitFailure");
    const forceBackendRaw = getStringParam("forceBackend");
    // In practice, WebGPU-in-worker can be flaky in headless Chromium unless
    // launched with the dedicated WebGPU project flags. Prefer WebGL2 for the
    // default smoke test, but still allow forcing WebGPU (or testing fallback)
    // via query params.
    const preferWebGpu = requestedBackend === "webgpu" || disableWebGpu;

    const forceBackend =
      forceBackendRaw === "webgpu" || forceBackendRaw === "webgl2_wgpu" || forceBackendRaw === "webgl2_raw"
        ? (forceBackendRaw as "webgpu" | "webgl2_wgpu" | "webgl2_raw")
        : undefined;

    const gpu = createGpuWorker({
      canvas,
      width: cssWidth,
      height: cssHeight,
      devicePixelRatio,
      gpuOptions: { preferWebGpu, disableWebGpu, ...(forceBackend ? { forceBackend } : {}) },
      onError: (msg) => {
        if (!status) return;
        status.textContent += `gpu_error msg=${msg.message}${msg.code ? ` code=${msg.code}` : ""}\n`;
      },
      onEvents: (msg) => {
        if (!status) return;
        for (const ev of msg.events) {
          status.textContent += `gpu_event ${ev.severity} ${ev.category}${ev.backend_kind ? ` (${ev.backend_kind})` : ""}: ${ev.message}\n`;
          eventLog.push(ev);
        }
        window.__aeroTest = { ...(window.__aeroTest ?? {}), events: eventLog };
      },
      onStats: (msg) => {
        // Print a small preview of WASM telemetry so the page can be used as a
        // diagnostics wiring check (particularly for the wgpu-backed WebGL2 presenter).
        window.__aeroTest = { ...(window.__aeroTest ?? {}), lastStats: msg, lastWasmStats: msg.wasm };

        if (telemetryEl) {
          const scanout = msg.scanout;
          const outputSource = msg.outputSource;
          const presentUpload = msg.presentUpload;
          const lines: string[] = [];
          if (typeof outputSource === "string") {
            lines.push(`outputSource=${outputSource}`);
          }
          if (presentUpload && typeof presentUpload === "object") {
            const kind = typeof presentUpload.kind === "string" ? presentUpload.kind : "n/a";
            const n = typeof presentUpload.dirtyRectCount === "number" ? presentUpload.dirtyRectCount : null;
            lines.push(`presentUpload=${kind}${kind === "dirty_rects" ? ` (n=${n ?? "?"})` : ""}`);
          }
          if (scanout && typeof scanout === "object") {
            const src = fmtScanoutSource(scanout.source);
            const base = typeof scanout.base_paddr === "string" ? scanout.base_paddr : "n/a";
            const w = typeof scanout.width === "number" ? scanout.width : "?";
            const h = typeof scanout.height === "number" ? scanout.height : "?";
            const pitch = typeof scanout.pitchBytes === "number" ? scanout.pitchBytes : "?";
            // Newer workers include `format_str` directly so consumers don't have to reimplement
            // the AerogpuFormat enum mapping.
            const fmt =
              typeof scanout.format_str === "string" ? scanout.format_str : aerogpuFormatToString(scanout.format);
            const gen = typeof scanout.generation === "number" ? scanout.generation : "?";
            lines.push(`scanout=${src} gen=${gen} base=${base} ${w}x${h} pitch=${pitch} fmt=${fmt}`);
          }
          telemetryEl.textContent = lines.join("\n");
        }

        if (msg.backendKind === "webgl2_wgpu" && !loggedFrameTimings) {
          const wasm = msg.wasm;
          const frameTimings =
            wasm && typeof wasm === "object" ? (wasm as Record<string, unknown>).frameTimings : undefined;
          if (frameTimings) {
            loggedFrameTimings = true;
            console.log("[gpu-worker] wasm frame timings", frameTimings);
            if (status) {
              status.textContent += `wasm.frameTimings=${safeJsonStringify(frameTimings)}\n`;
            }
          }
        }
        if (!status) return;
        if (!msg.wasm) return;
        if (statsLinesWritten >= 1) return;
        statsLinesWritten += 1;
        const preview = safeJsonStringify(msg.wasm);
        status.textContent += `gpu_stats wasm=${preview.slice(0, 400)}${preview.length > 400 ? "…" : ""}\n`;
      },
    });

    let ready: Awaited<typeof gpu.ready>;
    try {
      ready = await gpu.ready;
    } catch (err) {
      const message = formatOneLineError(err, 512);
      if (expectInitFailure) {
        renderError(message);
        return;
      }
      throw err;
    }
    if (backendEl) backendEl.textContent = ready.backendKind;
    if (ready.fallback && status) {
      status.textContent += `fallback ${ready.fallback.from} -> ${ready.fallback.to}: ${ready.fallback.reason}\n`;
    }

    gpu.presentTestPattern();
    // Note: worker screenshots are defined as a deterministic readback of the *source framebuffer* bytes
    // (pre-scaling / pre-color-management). Hash-based tests rely on this contract; it is intentionally
    // not a "what the user sees" capture of the presented canvas.
    const screenshot = await gpu.requestScreenshot();

    const rgba8 = new Uint8Array(screenshot.rgba8);
    const expected = createExpectedTestPattern(screenshot.width, screenshot.height);

    const hash = fnv1a32Hex(rgba8);
    const expectedHash = fnv1a32Hex(expected);
    const pass = hash === expectedHash;

    if (status) {
      status.textContent += `hash=${hash} expected=${expectedHash} ${pass ? "PASS" : "FAIL"}\n`;
    }

    const sample = (x: number, y: number) => {
      const i = (y * screenshot.width + x) * 4;
      return [rgba8[i + 0], rgba8[i + 1], rgba8[i + 2], rgba8[i + 3]];
    };

    window.__aeroTest = {
      ready: true,
      backend: ready.backendKind,
      fallback: ready.fallback,
      hash,
      expectedHash,
      pass,
      events: eventLog,
      lastStats: window.__aeroTest?.lastStats,
      lastWasmStats: window.__aeroTest?.lastWasmStats,
      samplePixels: async () => ({
        backend: ready.backendKind,
        width: screenshot.width,
        height: screenshot.height,
        topLeft: sample(8, 8),
        topRight: sample(screenshot.width - 9, 8),
        bottomLeft: sample(8, screenshot.height - 9),
        bottomRight: sample(screenshot.width - 9, screenshot.height - 9),
      }),
    };

    if (triggerPresenterError) {
      // Trigger a non-device-lost presenter error path and ensure it is surfaced as a structured
      // `events` message (in addition to the legacy `error` message). Use a cursor_set_image with
      // invalid dimensions because it is deterministic and does not depend on backend-specific
      // failure modes.
      //
      // Send it twice so the worker's per-generation dedupe can be exercised (events should only
      // include one entry, even though the legacy `error` message is posted twice).
      for (let i = 0; i < 2; i += 1) {
        const rgba8 = new ArrayBuffer(0);
        gpu.worker.postMessage(
          { ...GPU_MESSAGE_BASE, type: "cursor_set_image", width: 0, height: 0, rgba8 },
          [rgba8],
        );
      }
    }
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();

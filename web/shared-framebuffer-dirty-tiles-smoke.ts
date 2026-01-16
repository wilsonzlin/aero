import { fnv1a32Hex } from "./src/utils/fnv1a";
import { formatOneLineError } from "./src/text";
import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "./src/ipc/shared-layout";
import { FRAME_DIRTY, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "./src/ipc/gpu-protocol";
import type { GpuTelemetrySnapshot } from "./src/gpu/telemetry";

const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      error?: string;
      pass?: boolean;
      backend?: string;
      uploadBytesMax?: number;
      uploadBytesFullEstimate?: number;
      hashes?: { green: string; mixed: string; gotGreen: string; gotMixed: string };
      samples?: {
        greenTopLeft: number[];
        mixedTopLeft: number[];
        mixedTopRight: number[];
      };
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function renderError(message: string) {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function solidRgba8(width: number, height: number, rgba: [number, number, number, number]): Uint8Array {
  const out = new Uint8Array(width * height * 4);
  for (let i = 0; i < out.length; i += 4) {
    out[i + 0] = rgba[0];
    out[i + 1] = rgba[1];
    out[i + 2] = rgba[2];
    out[i + 3] = rgba[3];
  }
  return out;
}

function mixedTopLeftTileRgba8(
  width: number,
  height: number,
  tileSize: number,
  topLeft: [number, number, number, number],
  rest: [number, number, number, number],
): Uint8Array {
  const out = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const inTile = x < tileSize && y < tileSize;
      const rgba = inTile ? topLeft : rest;
      out[i + 0] = rgba[0];
      out[i + 1] = rgba[1];
      out[i + 2] = rgba[2];
      out[i + 3] = rgba[3];
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

  const status = $("status");
  const log = (line: string) => {
    if (status) status.textContent += `${line}\n`;
  };

  const width = 64;
  const height = 64;
  const strideBytes = width * 4;
  const tileSize = 32;

  canvas.width = width;
  canvas.height = height;
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;

  if (!("transferControlToOffscreen" in canvas)) {
    renderError("OffscreenCanvas is not supported in this browser.");
    return;
  }

  const layout = computeSharedFramebufferLayout(width, height, strideBytes, FramebufferFormat.RGBA8, tileSize);
  const shared = new SharedArrayBuffer(layout.totalBytes);
  const header = new Int32Array(shared, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
  const frameState = new Int32Array(sharedFrameState);

  // Initialize the header early so the GPU worker can bootstrap without waiting
  // on the CPU mock (avoids rare ready-message races).
  Atomics.store(header, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
  Atomics.store(header, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
  Atomics.store(header, SharedFramebufferHeaderIndex.WIDTH, width);
  Atomics.store(header, SharedFramebufferHeaderIndex.HEIGHT, height);
  Atomics.store(header, SharedFramebufferHeaderIndex.STRIDE_BYTES, strideBytes);
  Atomics.store(header, SharedFramebufferHeaderIndex.FORMAT, FramebufferFormat.RGBA8);
  Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILE_SIZE, tileSize);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILES_X, layout.tilesX);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILES_Y, layout.tilesY);
  Atomics.store(header, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, layout.dirtyWordsPerBuffer);
  Atomics.store(header, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FLAGS, 0);

  const cpu = new Worker(new URL("./src/workers/cpu-worker-mock.ts", import.meta.url), { type: "module" });
  cpu.postMessage({
    type: "init",
    shared,
    framebufferOffsetBytes: 0,
    sharedFrameState,
    width,
    height,
    tileSize,
    pattern: "tile_toggle",
  });

  const gpu = new Worker(new URL("./src/workers/gpu.worker.ts", import.meta.url), { type: "module" });

  const offscreen = canvas.transferControlToOffscreen();

  const pendingScreenshots = new Map<
    number,
    {
      resolve: (msg: { width: number; height: number; rgba8: ArrayBuffer; frameSeq: number }) => void;
      reject: (err: unknown) => void;
    }
  >();
  let nextRequestId = 1;

  let readyPayload: Record<string, unknown> | null = null;
  let latestTelemetry: GpuTelemetrySnapshot | null = null;

  const ready = new Promise<void>((resolve, reject) => {
    const onMessage = (event: MessageEvent) => {
      const msg = event.data as unknown;
      if (!msg || typeof msg !== "object") return;
      const typed = msg as { protocol?: unknown; protocolVersion?: unknown; type?: unknown; message?: unknown };
      if (typed.protocol !== GPU_PROTOCOL_NAME || typed.protocolVersion !== GPU_PROTOCOL_VERSION) return;
      if (typed.type === "ready") {
        readyPayload = msg as Record<string, unknown>;
        gpu.removeEventListener("message", onMessage);
        resolve();
      } else if (typed.type === "error") {
        gpu.removeEventListener("message", onMessage);
        reject(new Error(formatOneLineError(typed.message, 512, "unknown error")));
      }
    };
    gpu.addEventListener("message", onMessage);
  });

  gpu.addEventListener("message", (event: MessageEvent) => {
    const msg = event.data as unknown;
    if (!msg || typeof msg !== "object") return;
    const typed = msg as {
      protocol?: unknown;
      protocolVersion?: unknown;
      type?: unknown;
      message?: unknown;
      requestId?: unknown;
      width?: unknown;
      height?: unknown;
      rgba8?: unknown;
      frameSeq?: unknown;
      telemetry?: unknown;
    };
    if (typed.protocol !== GPU_PROTOCOL_NAME || typed.protocolVersion !== GPU_PROTOCOL_VERSION) return;
    if (typed.type === "error") {
      renderError(formatOneLineError(typed.message, 512, "gpu worker error"));
      return;
    }
    if (typed.type === "screenshot") {
      if (typeof typed.requestId !== "number") return;
      const pending = pendingScreenshots.get(typed.requestId);
      if (!pending) return;
      pendingScreenshots.delete(typed.requestId);
      pending.resolve({
        width: typeof typed.width === "number" ? typed.width : 0,
        height: typeof typed.height === "number" ? typed.height : 0,
        rgba8: typed.rgba8 instanceof ArrayBuffer ? typed.rgba8 : new ArrayBuffer(0),
        frameSeq: typeof typed.frameSeq === "number" ? typed.frameSeq : 0,
      });
      return;
    }
    if (typed.type === "metrics") {
      if (typed.telemetry && typeof typed.telemetry === "object") {
        latestTelemetry = typed.telemetry as GpuTelemetrySnapshot;
      }
    }
  });

  gpu.postMessage(
    {
      ...GPU_MESSAGE_BASE,
      type: "init",
      canvas: offscreen,
      sharedFrameState,
      sharedFramebuffer: shared,
      sharedFramebufferOffsetBytes: 0,
      options: { forceBackend: "webgl2_raw", disableWebGpu: true },
    },
    [offscreen],
  );

  const requestScreenshot = (): Promise<{ width: number; height: number; rgba8: ArrayBuffer; frameSeq: number }> => {
    const requestId = nextRequestId++;
    gpu.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId });
    return new Promise((resolve, reject) => {
      pendingScreenshots.set(requestId, { resolve, reject });
      setTimeout(() => {
        const pending = pendingScreenshots.get(requestId);
        if (!pending) return;
        pendingScreenshots.delete(requestId);
        reject(new Error("screenshot request timed out"));
      }, 2000);
    });
  };

  const waitForSeqAtLeast = async (seq: number): Promise<void> => {
    while (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) < seq) {
      await sleep(1);
    }
  };

  const requestScreenshotWithParity = async (wantOdd: boolean): Promise<{
    width: number;
    height: number;
    rgba8: ArrayBuffer;
    frameSeq: number;
  }> => {
    while (true) {
      const shot = await requestScreenshot();
      const isOdd = (shot.frameSeq & 1) === 1;
      if (isOdd === wantOdd) return shot;
      await sleep(5);
    }
  };

  const waitForTelemetryFrame = async (): Promise<GpuTelemetrySnapshot> => {
    const deadline = performance.now() + 2_000;
    while (performance.now() < deadline) {
      const telem = latestTelemetry;
      const count = telem?.textureUpload.bytesPerFrame.stats.count;
      if (typeof count === "number" && count > 0) {
        return telem!;
      }
      await sleep(25);
    }
    throw new Error("Timed out waiting for GPU telemetry (metrics messages)");
  };

  try {
    await ready;

    // Tick loop: drive the worker presenter while the CPU mock publishes frames.
    const tick = () => {
      if (Atomics.load(frameState, FRAME_STATUS_INDEX) === FRAME_DIRTY) {
        gpu.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);

    // Wait for at least one frame and capture an odd-seq frame (all green).
    await waitForSeqAtLeast(1);
    const greenShot = await requestScreenshotWithParity(true);

    // Capture an even-seq frame (top-left tile red).
    const mixedShot = await requestScreenshotWithParity(false);

    const gotGreen = new Uint8Array(greenShot.rgba8);
    const gotMixed = new Uint8Array(mixedShot.rgba8);

    const expectedGreen = solidRgba8(width, height, [0, 255, 0, 255]);
    const expectedMixed = mixedTopLeftTileRgba8(width, height, tileSize, [255, 0, 0, 255], [0, 255, 0, 255]);

    const gotGreenHash = fnv1a32Hex(gotGreen);
    const gotMixedHash = fnv1a32Hex(gotMixed);
    const expectedGreenHash = fnv1a32Hex(expectedGreen);
    const expectedMixedHash = fnv1a32Hex(expectedMixed);

    const fullUploadEstimate = width * height * 4;
    const telemetry = await waitForTelemetryFrame();
    const uploadBytesMax = telemetry?.textureUpload.bytesPerFrame.stats.max;
    if (typeof uploadBytesMax !== "number") {
      throw new Error("GPU telemetry missing textureUpload.bytesPerFrame.stats.max");
    }

    const backend = typeof readyPayload?.backendKind === "string" ? readyPayload.backendKind : undefined;
    const partialUploadsOk = uploadBytesMax <= 10_000;
    const hashesOk = gotGreenHash === expectedGreenHash && gotMixedHash === expectedMixedHash;
    const pass = hashesOk && partialUploadsOk;

    if (!partialUploadsOk) {
      throw new Error(
        `Expected partial texture uploads (presentDirtyRects). maxUploadBytes=${uploadBytesMax} fullEstimate=${fullUploadEstimate} backend=${backend ?? "unknown"}`,
      );
    }

    const sample = (rgba: Uint8Array, x: number, y: number) => {
      const i = (y * width + x) * 4;
      return [rgba[i + 0], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
    };

    log(`green frame seq=${greenShot.frameSeq} hash=${gotGreenHash} expected=${expectedGreenHash}`);
    log(`mixed frame seq=${mixedShot.frameSeq} hash=${gotMixedHash} expected=${expectedMixedHash}`);
    log(pass ? "PASS" : "FAIL");

    window.__aeroTest = {
      ready: true,
      pass,
      backend,
      uploadBytesMax,
      uploadBytesFullEstimate: fullUploadEstimate,
      hashes: { green: expectedGreenHash, mixed: expectedMixedHash, gotGreen: gotGreenHash, gotMixed: gotMixedHash },
      samples: {
        greenTopLeft: sample(gotGreen, 8, 8),
        mixedTopLeft: sample(gotMixed, 8, 8),
        mixedTopRight: sample(gotMixed, width - 9, 8),
      },
    };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();

import { fnv1a32Hex } from "./src/utils/fnv1a";
import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
} from "./src/ipc/shared-layout";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      error?: string;
      pass?: boolean;
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

  const cpu = new Worker(new URL("./src/workers/cpu-worker-mock.ts", import.meta.url), { type: "module" });
  cpu.postMessage({
    type: "init",
    shared,
    framebufferOffsetBytes: 0,
    width,
    height,
    tileSize,
    pattern: "tile_toggle",
  });

  const gpu = new Worker(new URL("./src/workers/shared-framebuffer-presenter.worker.ts", import.meta.url), {
    type: "module",
  });

  const offscreen = canvas.transferControlToOffscreen();

  const pendingScreenshots = new Map<
    number,
    {
      resolve: (msg: { width: number; height: number; rgba8: ArrayBuffer; frameSeq: number }) => void;
      reject: (err: unknown) => void;
    }
  >();
  let nextRequestId = 1;

  const ready = new Promise<void>((resolve, reject) => {
    const onMessage = (event: MessageEvent) => {
      const msg = event.data as any;
      if (!msg || typeof msg !== "object") return;
      if (msg.type === "ready") {
        gpu.removeEventListener("message", onMessage);
        resolve();
      } else if (msg.type === "error") {
        gpu.removeEventListener("message", onMessage);
        reject(new Error(String(msg.message ?? "unknown error")));
      }
    };
    gpu.addEventListener("message", onMessage);
  });

  gpu.addEventListener("message", (event: MessageEvent) => {
    const msg = event.data as any;
    if (!msg || typeof msg !== "object") return;
    if (msg.type === "error") {
      renderError(String(msg.message ?? "gpu worker error"));
      return;
    }
    if (msg.type === "screenshot") {
      const pending = pendingScreenshots.get(msg.requestId);
      if (!pending) return;
      pendingScreenshots.delete(msg.requestId);
      pending.resolve({ width: msg.width, height: msg.height, rgba8: msg.rgba8, frameSeq: msg.frameSeq });
    }
  });

  gpu.postMessage(
    {
      type: "init",
      canvas: offscreen,
      shared,
      framebufferOffsetBytes: 0,
    },
    [offscreen],
  );

  const requestScreenshot = (): Promise<{ width: number; height: number; rgba8: ArrayBuffer; frameSeq: number }> => {
    const requestId = nextRequestId++;
    gpu.postMessage({ type: "request_screenshot", requestId });
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

  try {
    await ready;

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

    const pass = gotGreenHash === expectedGreenHash && gotMixedHash === expectedMixedHash;

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
      hashes: { green: expectedGreenHash, mixed: expectedMixedHash, gotGreen: gotGreenHash, gotMixed: gotMixedHash },
      samples: {
        greenTopLeft: sample(gotGreen, 8, 8),
        mixedTopLeft: sample(gotMixed, 8, 8),
        mixedTopRight: sample(gotMixed, width - 9, 8),
      },
    };
  } catch (err) {
    renderError(err instanceof Error ? err.message : String(err));
  }
}

void main();


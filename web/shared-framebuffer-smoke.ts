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
      hashes?: { first: string; second: string; green: string; red: string };
      samples?: { first: number[]; second: number[] };
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

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
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

  try {
    await ready;

    // Wait for the CPU worker to publish at least one frame.
    await waitForSeqAtLeast(1);
    await sleep(10);
    const first = await requestScreenshot();

    // Ensure the second screenshot captures a frame with different parity (CPU mock alternates colors).
    const firstParity = first.frameSeq & 1;
    while (true) {
      const seqNow = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
      if (seqNow > first.frameSeq && (seqNow & 1) !== firstParity) break;
      await sleep(1);
    }
    await sleep(10);
    const second = await requestScreenshot();

    const firstBytes = new Uint8Array(first.rgba8);
    const secondBytes = new Uint8Array(second.rgba8);

    const expectedGreen = solidRgba8(first.width, first.height, [0, 255, 0, 255]);
    const expectedRed = solidRgba8(first.width, first.height, [255, 0, 0, 255]);

    const firstHash = fnv1a32Hex(firstBytes);
    const secondHash = fnv1a32Hex(secondBytes);
    const greenHash = fnv1a32Hex(expectedGreen);
    const redHash = fnv1a32Hex(expectedRed);

    const expectedFirstHash = (first.frameSeq & 1) === 1 ? greenHash : redHash;
    const expectedSecondHash = (second.frameSeq & 1) === 1 ? greenHash : redHash;
    const pass = firstHash === expectedFirstHash && secondHash === expectedSecondHash;

    const sample = (rgba: Uint8Array) => [rgba[0], rgba[1], rgba[2], rgba[3]];

    log(`frame 1 hash=${firstHash}`);
    log(`frame 2 hash=${secondHash}`);
    log(`frame 1 seq=${first.frameSeq}`);
    log(`frame 2 seq=${second.frameSeq}`);
    log(`expected green hash=${greenHash}`);
    log(`expected red hash=${redHash}`);
    log(pass ? "PASS" : "FAIL");

    window.__aeroTest = {
      ready: true,
      pass,
      hashes: { first: firstHash, second: secondHash, green: greenHash, red: redHash },
      samples: { first: sample(firstBytes), second: sample(secondBytes) },
    };
  } catch (err) {
    renderError(err instanceof Error ? err.message : String(err));
  }
}

void main();

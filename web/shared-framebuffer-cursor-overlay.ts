import {
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  computeSharedFramebufferLayout,
} from "./src/ipc/shared-layout";
import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
} from "./src/ipc/gpu-protocol";
import { formatOneLineError } from "./src/text";

const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      error?: string;
      pass?: boolean;
      sample?: number[];
      expected?: number[];
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

function log(line: string) {
  const status = $("status");
  if (status) status.textContent += `${line}\n`;
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

  const width = 4;
  const height = 4;
  const strideBytes = width * 4;
  const tileSize = 0;

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

  // Initialize the shared framebuffer header (mirror `cpu-worker-mock.ts`).
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

  Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
  Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

  const slot0 = new Uint32Array(shared, layout.framebufferOffsets[0], (strideBytes / 4) * height);
  const slot1 = new Uint32Array(shared, layout.framebufferOffsets[1], (strideBytes / 4) * height);
  let activeIndex = 0;

  const publishFrame = (fill: (buf: Uint32Array) => void): number => {
    const back = activeIndex ^ 1;
    const dst = back === 0 ? slot0 : slot1;
    fill(dst);

    const newSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
    Atomics.store(
      header,
      back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
      newSeq,
    );
    Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
    Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);
    activeIndex = back;

    Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
    return newSeq;
  };

  // Solid blue background.
  publishFrame((buf) => buf.fill(0xffff0000));

  const gpu = new Worker(new URL("./src/workers/gpu.worker.ts", import.meta.url), {
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
      const msg = event.data as unknown;
      if (!msg || typeof msg !== "object") return;
      const typed = msg as { protocol?: unknown; protocolVersion?: unknown; type?: unknown; message?: unknown };
      if (typed.protocol !== GPU_PROTOCOL_NAME || typed.protocolVersion !== GPU_PROTOCOL_VERSION) return;
      if (typed.type === "ready") {
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
      options: {
        forceBackend: "webgl2_raw",
        outputWidth: width,
        outputHeight: height,
        dpr: 1,
      },
    },
    [offscreen],
  );

  const requestScreenshot = (
    includeCursor: boolean,
  ): Promise<{ width: number; height: number; rgba8: ArrayBuffer; frameSeq: number }> => {
    const requestId = nextRequestId++;
    gpu.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId, includeCursor });
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

  try {
    await ready;

    // Upload the base frame first.
    gpu.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });

    // Cursor image: 1x1 red @ 50% alpha.
    const cursorBytes = new Uint8Array([255, 0, 0, 128]);
    gpu.postMessage({ ...GPU_MESSAGE_BASE, type: "cursor_set_image", width: 1, height: 1, rgba8: cursorBytes.buffer }, [
      cursorBytes.buffer,
    ]);
    gpu.postMessage({ ...GPU_MESSAGE_BASE, type: "cursor_set_state", enabled: true, x: 0, y: 0, hotX: 0, hotY: 0 });

    // Give the worker a moment to upload cursor state before capture.
    await sleep(10);

    const shot = await requestScreenshot(true);
    const rgba = new Uint8Array(shot.rgba8);
    const sample = [rgba[0], rgba[1], rgba[2], rgba[3]];

    // Background is solid blue (0,0,255). Cursor is solid red (255,0,0) with alpha=128.
    // Expected output: (r=128, g=0, b=127, a=255).
    const expected = [128, 0, 127, 255];
    const pass = sample.join(",") === expected.join(",");

    log(`sample=${sample.join(",")}`);
    log(`expected=${expected.join(",")}`);
    log(pass ? "PASS" : "FAIL");

    window.__aeroTest = { ready: true, pass, sample, expected };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();

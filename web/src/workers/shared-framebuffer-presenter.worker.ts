/// <reference lib="webworker" />

import { RawWebGL2Presenter } from "../gpu/raw-webgl2-presenter";
import {
  dirtyTilesToRects,
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  type SharedFramebufferLayout,
} from "../ipc/shared-layout";

type InitMessage = {
  type: "init";
  canvas: OffscreenCanvas;
  shared: SharedArrayBuffer;
  framebufferOffsetBytes: number;
};

type RequestScreenshotMessage = {
  type: "request_screenshot";
  requestId: number;
};

type ShutdownMessage = { type: "shutdown" };

type IncomingMessage = InitMessage | RequestScreenshotMessage | ShutdownMessage;

type ReadyMessage = { type: "ready" };
type ScreenshotMessage = {
  type: "screenshot";
  requestId: number;
  frameSeq: number;
  width: number;
  height: number;
  rgba8: ArrayBuffer;
  origin: "top-left";
};
type ErrorMessage = { type: "error"; message: string };

type OutgoingMessage = ReadyMessage | ScreenshotMessage | ErrorMessage;

const scope = self as unknown as DedicatedWorkerGlobalScope;

let stop = false;

let header: Int32Array | null = null;
let layout: SharedFramebufferLayout | null = null;
let slot0: Uint8Array | null = null;
let slot1: Uint8Array | null = null;
let dirty0: Uint32Array | null = null;
let dirty1: Uint32Array | null = null;

let presenter: RawWebGL2Presenter | null = null;

let lastSeq = 0;

function post(msg: OutgoingMessage, transfer: Transferable[] = []): void {
  scope.postMessage(msg, transfer);
}

async function waitForFrameSeqChange(expected: number): Promise<void> {
  const h = header;
  if (!h) return;

  const waitAsync = (Atomics as unknown as { waitAsync?: unknown }).waitAsync;
  if (typeof waitAsync === "function") {
    const result = (waitAsync as any)(h, SharedFramebufferHeaderIndex.FRAME_SEQ, expected);
    // Spec: returns { async: boolean, value: string | Promise<string> }.
    if (result && typeof result === "object" && "async" in result && "value" in result) {
      if (result.async) {
        await result.value;
      }
    }
    return;
  }

  // Portable fallback: cooperative polling.
  await new Promise((resolve) => setTimeout(resolve, 1));
}

function refreshViews(shared: SharedArrayBuffer, framebufferOffsetBytes: number): void {
  const hdr = new Int32Array(shared, framebufferOffsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);

  const magic = Atomics.load(hdr, SharedFramebufferHeaderIndex.MAGIC);
  const version = Atomics.load(hdr, SharedFramebufferHeaderIndex.VERSION);
  if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) {
    throw new Error(
      `shared framebuffer header mismatch: magic=0x${magic.toString(16)} version=${version} expected magic=0x${SHARED_FRAMEBUFFER_MAGIC.toString(
        16,
      )} version=${SHARED_FRAMEBUFFER_VERSION}`,
    );
  }

  const computed = layoutFromHeader(hdr);

  header = hdr;
  layout = computed;

  const pixelLen = computed.strideBytes * computed.height;
  slot0 = new Uint8Array(shared, framebufferOffsetBytes + computed.framebufferOffsets[0], pixelLen);
  slot1 = new Uint8Array(shared, framebufferOffsetBytes + computed.framebufferOffsets[1], pixelLen);

  if (computed.dirtyWordsPerBuffer > 0) {
    dirty0 = new Uint32Array(shared, framebufferOffsetBytes + computed.dirtyOffsets[0], computed.dirtyWordsPerBuffer);
    dirty1 = new Uint32Array(shared, framebufferOffsetBytes + computed.dirtyOffsets[1], computed.dirtyWordsPerBuffer);
  } else {
    dirty0 = null;
    dirty1 = null;
  }
}

function presentLatest(): void {
  if (!header || !layout || !presenter || !slot0 || !slot1) return;

  const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
  const pixels = active === 0 ? slot0 : slot1;
  const dirtyWords = active === 0 ? dirty0 : dirty1;

  const width = layout.width;
  const height = layout.height;
  const strideBytes = layout.strideBytes;

  if (dirtyWords) {
    const rects = dirtyTilesToRects(layout, dirtyWords);
    presenter.setSourceRgba8StridedDirtyRects(pixels, width, height, strideBytes, rects);
  } else {
    presenter.setSourceRgba8Strided(pixels, width, height, strideBytes);
  }

  presenter.present();
}

async function runPresentLoop(): Promise<void> {
  if (!header) return;

  lastSeq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);

  while (!stop) {
    const seq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
    if (seq !== lastSeq) {
      lastSeq = seq;
      try {
        presentLatest();
      } catch (err) {
        post({ type: "error", message: err instanceof Error ? err.message : String(err) });
      }
      continue;
    }

    await waitForFrameSeqChange(lastSeq);
  }
}

function readPixelsTopLeft(gl: WebGL2RenderingContext, width: number, height: number): Uint8Array {
  const pixels = new Uint8Array(width * height * 4);
  gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

  // WebGL readPixels origin is bottom-left; convert to top-left for easier hashing/tests.
  const rowStride = width * 4;
  const flipped = new Uint8Array(pixels.length);
  for (let y = 0; y < height; y += 1) {
    const srcStart = (height - 1 - y) * rowStride;
    const dstStart = y * rowStride;
    flipped.set(pixels.subarray(srcStart, srcStart + rowStride), dstStart);
  }
  return flipped;
}

scope.onmessage = (event: MessageEvent<IncomingMessage>) => {
  const msg = event.data;

  switch (msg.type) {
    case "init": {
      stop = false;

      try {
        refreshViews(msg.shared, msg.framebufferOffsetBytes);
      } catch (err) {
        post({ type: "error", message: err instanceof Error ? err.message : String(err) });
        return;
      }

      if (!layout) {
        post({ type: "error", message: "shared framebuffer layout unavailable" });
        return;
      }

      // Drive the output at the framebuffer's pixel size (the page can scale via CSS).
      msg.canvas.width = layout.width;
      msg.canvas.height = layout.height;

      try {
        presenter = new RawWebGL2Presenter(msg.canvas, {
          framebufferColorSpace: "linear",
          outputColorSpace: "srgb",
          alphaMode: "opaque",
        });
      } catch (err) {
        post({ type: "error", message: err instanceof Error ? err.message : String(err) });
        return;
      }

      post({ type: "ready" });
      void runPresentLoop();
      break;
    }

    case "request_screenshot": {
      if (!presenter || !layout) return;

      // Ensure we render the latest published buffer before capture.
      try {
        presentLatest();
      } catch (err) {
        post({ type: "error", message: err instanceof Error ? err.message : String(err) });
        return;
      }

      const gl = presenter.gl as WebGL2RenderingContext;
      gl.finish();
      const frameSeq = header ? Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) : 0;
      const rgba8 = readPixelsTopLeft(gl, layout.width, layout.height);
      post(
        {
          type: "screenshot",
          requestId: msg.requestId,
          frameSeq,
          width: layout.width,
          height: layout.height,
          rgba8: rgba8.buffer,
          origin: "top-left",
        },
        [rgba8.buffer],
      );
      break;
    }

    case "shutdown": {
      stop = true;
      break;
    }
  }
};

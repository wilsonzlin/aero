import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
import {
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  computeSharedFramebufferLayout,
  layoutFromHeader,
} from "../ipc/shared-layout";

async function waitForWorkerMessage(
  worker: Worker,
  predicate: (msg: unknown) => boolean,
  timeoutMs: number,
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${timeoutMs}ms waiting for worker message`));
    }, timeoutMs);
    (timer as unknown as { unref?: () => void }).unref?.();

    const onMessage = (msg: unknown) => {
      // Surface runtime worker errors eagerly.
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const errMsg = typeof maybeProtocol.message === "string" ? maybeProtocol.message : "";
        reject(new Error(`worker reported error${errMsg ? `: ${errMsg}` : ""}`));
        return;
      }
      try {
        if (!predicate(msg)) return;
      } catch (err) {
        cleanup();
        reject(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      cleanup();
      resolve(msg);
    };

    const onError = (err: unknown) => {
      cleanup();
      reject(err instanceof Error ? err : new Error(String(err)));
    };

    const onExit = (code: number) => {
      cleanup();
      reject(new Error(`worker exited before emitting the expected message (code=${code})`));
    };

    function cleanup(): void {
      clearTimeout(timer);
      worker.off("message", onMessage);
      worker.off("error", onError);
      worker.off("exit", onExit);
    }

    worker.on("message", onMessage);
    worker.on("error", onError);
    worker.on("exit", onExit);
  });
}

function firstPixelU32(rgba8: ArrayBuffer): number {
  const px = new Uint8Array(rgba8);
  if (px.byteLength < 4) return 0;
  return (((px[0] ?? 0) | ((px[1] ?? 0) << 8) | ((px[2] ?? 0) << 16) | ((px[3] ?? 0) << 24)) >>> 0);
}

describe("workers/gpu-worker WDDM scanout disabled descriptor", () => {
  it("returns a blank (black) screenshot instead of falling back to the legacy shared framebuffer", async () => {
    // Set up a minimal shared framebuffer with a distinctive non-black pixel.
    const fbLayout = computeSharedFramebufferLayout(1, 1, 4, FramebufferFormat.RGBA8, 0);
    const sharedFramebuffer = new SharedArrayBuffer(fbLayout.totalBytes);
    const header = new Int32Array(sharedFramebuffer, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    Atomics.store(header, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
    Atomics.store(header, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
    Atomics.store(header, SharedFramebufferHeaderIndex.WIDTH, fbLayout.width);
    Atomics.store(header, SharedFramebufferHeaderIndex.HEIGHT, fbLayout.height);
    Atomics.store(header, SharedFramebufferHeaderIndex.STRIDE_BYTES, fbLayout.strideBytes);
    Atomics.store(header, SharedFramebufferHeaderIndex.FORMAT, fbLayout.format);
    Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILE_SIZE, fbLayout.tileSize);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILES_X, fbLayout.tilesX);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILES_Y, fbLayout.tilesY);
    Atomics.store(header, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, fbLayout.dirtyWordsPerBuffer);
    Atomics.store(header, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.FLAGS, 0);

    const layout = layoutFromHeader(header);
    const slot0 = new Uint8Array(sharedFramebuffer, layout.framebufferOffsets[0], layout.strideBytes * layout.height);
    // RGBA = 11 22 33 FF -> u32 0xff332211 (little-endian).
    slot0.set([0x11, 0x22, 0x33, 0xff], 0);
    // Establish a happens-before edge so the worker sees the pixel bytes.
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Start in legacy scanout mode so screenshots come from the shared framebuffer.
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_TEXT,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Control-plane init (SharedArrayBuffers + guest RAM + scanout state).
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };
      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
      );

      // GPU protocol init in headless mode (no canvas).
      const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
      const frameState = new Int32Array(sharedFrameState);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
      Atomics.store(frameState, FRAME_SEQ_INDEX, 0);
      worker.postMessage({
        protocol: GPU_PROTOCOL_NAME,
        protocolVersion: GPU_PROTOCOL_VERSION,
        type: "init",
        sharedFrameState,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        10_000,
      );

      const requestScreenshot = async (requestId: number) => {
        const shotPromise = waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
            (msg as { type?: unknown }).type === "screenshot" &&
            (msg as { requestId?: unknown }).requestId === requestId,
          10_000,
        );
        worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });
        return (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      };

      const legacyShot = await requestScreenshot(1);
      expect(legacyShot.width).toBe(1);
      expect(legacyShot.height).toBe(1);
      expect(firstPixelU32(legacyShot.rgba8)).toBe(0xff332211);

      // Publish the WDDM-disabled scanout descriptor (`base/width/height/pitch=0`). This represents
      // "blank output" while WDDM retains ownership; legacy output must remain suppressed.
      publishScanoutState(views.scanoutStateI32!, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: 0,
        basePaddrHi: 0,
        width: 0,
        height: 0,
        pitchBytes: 0,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

      const blankShot = await requestScreenshot(2);
      expect(blankShot.width).toBe(1);
      expect(blankShot.height).toBe(1);
      expect(firstPixelU32(blankShot.rgba8)).toBe(0xff000000);
    } finally {
      await worker.terminate();
    }
  }, 25_000);
});

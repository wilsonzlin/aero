import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { VRAM_BASE_PADDR } from "../arch/guest_phys";
import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import { allocateSharedMemorySegments, createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
import { CURSOR_FORMAT_B8G8R8A8, CURSOR_FORMAT_B8G8R8X8, publishCursorState } from "../ipc/cursor_state";

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
        const errMsg = typeof (maybeProtocol as { message?: unknown }).message === "string" ? (maybeProtocol as any).message : "";
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

async function initHeadlessGpuWorker(worker: Worker, initMsg: WorkerInitMessage): Promise<void> {
  worker.postMessage(initMsg);
  await waitForWorkerMessage(
    worker,
    (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
    10_000,
  );

  const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
  const frameState = new Int32Array(sharedFrameState);
  Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
  Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

  worker.postMessage({
    protocol: GPU_PROTOCOL_NAME,
    protocolVersion: GPU_PROTOCOL_VERSION,
    type: "init",
    sharedFrameState,
    sharedFramebuffer: initMsg.sharedFramebuffer,
    sharedFramebufferOffsetBytes: initMsg.sharedFramebufferOffsetBytes,
  });

  await waitForWorkerMessage(
    worker,
    (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
    10_000,
  );
}

async function requestScreenshot(worker: Worker, requestId: number, includeCursor: boolean): Promise<{ width: number; height: number; rgba8: ArrayBuffer }> {
  const shotPromise = waitForWorkerMessage(
    worker,
    (msg) =>
      (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
      (msg as { type?: unknown }).type === "screenshot" &&
      (msg as { requestId?: unknown }).requestId === requestId,
    10_000,
  );
  worker.postMessage({
    protocol: GPU_PROTOCOL_NAME,
    protocolVersion: GPU_PROTOCOL_VERSION,
    type: "screenshot",
    requestId,
    ...(includeCursor ? { includeCursor: true } : {}),
  });
  return (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
}

describe("workers/gpu-worker cursor screenshot overlay", () => {
  it("composites X8 cursor formats as opaque (alpha forced to 0xff)", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;
    // Scanout pixel: BGRX -> RGBA = [0x30, 0x20, 0x10, 0xff].
    views.guestU8.set([0x10, 0x20, 0x30, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor pixel: BGRX with X=0 (would be fully transparent if treated as alpha).
    views.guestU8.set([0x01, 0x02, 0x03, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8X8,
      basePaddrLo: cursorPaddr,
      basePaddrHi: 0,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      await initHeadlessGpuWorker(worker, {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shotNoCursor = await requestScreenshot(worker, 1, false);
      expect(shotNoCursor.width).toBe(1);
      expect(shotNoCursor.height).toBe(1);
      expect(firstPixelU32(shotNoCursor.rgba8)).toBe(0xff102030);

      const shotWithCursor = await requestScreenshot(worker, 2, true);
      expect(shotWithCursor.width).toBe(1);
      expect(shotWithCursor.height).toBe(1);
      // Cursor pixel: BGRX [01 02 03 00] -> RGBA [03 02 01 ff] => 0xff010203.
      expect(firstPixelU32(shotWithCursor.rgba8)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("respects alpha for A8 cursor formats (transparent cursor does not overwrite)", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;
    // Scanout pixel: BGRX -> RGBA = [0x30, 0x20, 0x10, 0xff].
    views.guestU8.set([0x10, 0x20, 0x30, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor pixel: BGRA with A=0 (fully transparent).
    views.guestU8.set([0x01, 0x02, 0x03, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8A8,
      basePaddrLo: cursorPaddr,
      basePaddrHi: 0,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      await initHeadlessGpuWorker(worker, {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      // Transparent cursor should not modify the scanout pixel.
      expect(firstPixelU32(shot.rgba8)).toBe(0xff102030);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("reads VRAM-backed cursor surfaces when compositing screenshots", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 1 });
    const views = createSharedMemoryViews(segments);
    if (!segments.vram || views.vramSizeBytes === 0) {
      throw new Error("test requires a non-empty shared VRAM segment");
    }

    const scanoutPaddr = 0x1000;
    // Scanout pixel: BGRX -> RGBA = [0x30, 0x20, 0x10, 0xff].
    views.guestU8.set([0x10, 0x20, 0x30, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const cursorVramOffset = 0x2000;
    if (cursorVramOffset + 4 > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for cursor pixel");
    }
    views.vramU8.set([0x01, 0x02, 0x03, 0x00], cursorVramOffset);
    const cursorBasePaddr = (VRAM_BASE_PADDR + cursorVramOffset) >>> 0;
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8X8,
      basePaddrLo: cursorBasePaddr,
      basePaddrHi: 0,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      await initHeadlessGpuWorker(worker, {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
        vramBasePaddr: VRAM_BASE_PADDR,
        vramSizeBytes: segments.vram.byteLength,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shotNoCursor = await requestScreenshot(worker, 1, false);
      expect(shotNoCursor.width).toBe(1);
      expect(shotNoCursor.height).toBe(1);
      expect(firstPixelU32(shotNoCursor.rgba8)).toBe(0xff102030);

      const shotWithCursor = await requestScreenshot(worker, 2, true);
      expect(shotWithCursor.width).toBe(1);
      expect(shotWithCursor.height).toBe(1);
      expect(firstPixelU32(shotWithCursor.rgba8)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("syncs hardware cursor state for screenshots even when no tick is forced", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
    const views = createSharedMemoryViews(segments);

    // Provide a tiny shared framebuffer (1x1) so the headless screenshot path doesn't
    // need to copy the full demo framebuffer.
    const layout = computeSharedFramebufferLayout(1, 1, 4, FramebufferFormat.RGBA8, 0);
    const sharedFramebuffer = new SharedArrayBuffer(layout.totalBytes);
    const header = new Int32Array(sharedFramebuffer, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    Atomics.store(header, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
    Atomics.store(header, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
    Atomics.store(header, SharedFramebufferHeaderIndex.WIDTH, layout.width);
    Atomics.store(header, SharedFramebufferHeaderIndex.HEIGHT, layout.height);
    Atomics.store(header, SharedFramebufferHeaderIndex.STRIDE_BYTES, layout.strideBytes);
    Atomics.store(header, SharedFramebufferHeaderIndex.FORMAT, layout.format);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILE_SIZE, layout.tileSize);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILES_X, layout.tilesX);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILES_Y, layout.tilesY);
    Atomics.store(header, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, layout.dirtyWordsPerBuffer);
    Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 1);
    Atomics.store(header, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
    Atomics.store(header, SharedFramebufferHeaderIndex.FLAGS, 0);

    const slot0 = new Uint8Array(sharedFramebuffer, layout.framebufferOffsets[0], layout.strideBytes * layout.height);
    // Background pixel: RGBA [0x30, 0x20, 0x10, 0xff] => 0xff102030.
    slot0.set([0x30, 0x20, 0x10, 0xff]);

    const cursorPaddr = 0x1000;
    // Cursor pixel: BGRX with X=0 (would be treated as transparent if misinterpreted as alpha).
    views.guestU8.set([0x01, 0x02, 0x03, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8X8,
      basePaddrLo: cursorPaddr,
      basePaddrHi: 0,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Runtime init (wire up CursorState) but omit scanoutState so screenshot does not force a tick.
      await initHeadlessGpuWorker(worker, {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer,
        sharedFramebufferOffsetBytes: 0,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shotNoCursor = await requestScreenshot(worker, 1, false);
      expect(firstPixelU32(shotNoCursor.rgba8)).toBe(0xff102030);

      const shotWithCursor = await requestScreenshot(worker, 2, true);
      // Cursor pixel: BGRX [01 02 03 00] -> RGBA [03 02 01 ff] => 0xff010203.
      expect(firstPixelU32(shotWithCursor.rgba8)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);
});

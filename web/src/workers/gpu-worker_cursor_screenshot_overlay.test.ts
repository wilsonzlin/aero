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
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8A8, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
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
  return pixelU32At(rgba8, 1, 0, 0);
}

function pixelU32At(rgba8: ArrayBuffer, width: number, x: number, y: number): number {
  const px = new Uint8Array(rgba8);
  const w = Math.max(0, width | 0);
  const xx = x | 0;
  const yy = y | 0;
  if (w <= 0 || xx < 0 || yy < 0) return 0;
  const off = (yy * w + xx) * 4;
  if (off < 0 || off + 3 >= px.byteLength) return 0;
  return (((px[off + 0] ?? 0) | ((px[off + 1] ?? 0) << 8) | ((px[off + 2] ?? 0) << 16) | ((px[off + 3] ?? 0) << 24)) >>> 0);
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
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
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
        vgaFramebuffer: segments.sharedFramebuffer,
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
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
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
        vgaFramebuffer: segments.sharedFramebuffer,
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
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
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
        vgaFramebuffer: segments.sharedFramebuffer,
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
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
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
        vgaFramebuffer: sharedFramebuffer,
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

  it("alpha-blends partially transparent cursor pixels", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;
    // Background pixel: BGRX all zeros -> RGBA [0,0,0,0xff].
    views.guestU8.set([0x00, 0x00, 0x00, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor pixel: BGRA with 50% alpha (A=0x80), red channel at 0xff.
    // BGRA [00 00 ff 80] -> RGBA [ff 00 00 80].
    views.guestU8.set([0x00, 0x00, 0xff, 0x80], cursorPaddr);
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
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      // 50% red over black: R = round(255*128/255) = 128 => 0xff000080.
      expect(firstPixelU32(shot.rgba8)).toBe(0xff000080);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("propagates cursor alpha when the scanout pixel is transparent", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;
    // Background pixel: BGRA all zeros -> RGBA [0,0,0,0].
    views.guestU8.set([0x00, 0x00, 0x00, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8A8,
    });

    // Cursor pixel: BGRA with A=0x80, red=0xff.
    views.guestU8.set([0x00, 0x00, 0xff, 0x80], cursorPaddr);
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
      // outRgb = 50% red over black, outA = 0x80 over alpha=0 background => 0x80000080.
      expect(firstPixelU32(shot.rgba8)).toBe(0x80000080);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("applies cursor hotX offsets when compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout: 2x1 BGRX pixels.
    // pixel0: BGRX [10 20 30 00] -> RGBA [30 20 10 ff] => 0xff102030
    // pixel1: BGRX [01 02 03 00] -> RGBA [03 02 01 ff] => 0xff010203
    views.guestU8.set([0x10, 0x20, 0x30, 0x00, 0x01, 0x02, 0x03, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 2,
      height: 1,
      pitchBytes: 8,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor pixel: BGRX [04 05 06 00] -> RGBA [06 05 04 ff] => 0xff040506.
    views.guestU8.set([0x04, 0x05, 0x06, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 1,
      y: 0,
      hotX: 1,
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
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(2);
      expect(shot.height).toBe(1);

      // x=1, hotX=1 -> originX=0, so cursor lands on pixel0.
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff040506);
      expect(pixelU32At(shot.rgba8, shot.width, 1, 0)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("clips cursors that extend beyond the screenshot bounds", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout: 2x1 BGRX pixels.
    views.guestU8.set([0x10, 0x20, 0x30, 0x00, 0x01, 0x02, 0x03, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 2,
      height: 1,
      pitchBytes: 8,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor: 2x1 BGRX pixels, but positioned so the second pixel is off-screen.
    // cursor pixel0 @x=1 (drawn), pixel1 @x=2 (clipped).
    // pixel0: [0a 0b 0c 00] -> RGBA [0c 0b 0a ff] => 0xff0a0b0c
    // pixel1: [0d 0e 0f 00] -> RGBA [0f 0e 0d ff] => 0xff0d0e0f
    views.guestU8.set([0x0a, 0x0b, 0x0c, 0x00, 0x0d, 0x0e, 0x0f, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 1,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 2,
      height: 1,
      pitchBytes: 8,
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
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      });

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(2);
      expect(shot.height).toBe(1);

      // pixel0 stays scanout ([10 20 30] -> 0xff102030).
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff102030);
      // pixel1 becomes cursor pixel0; cursor pixel1 is clipped.
      expect(pixelU32At(shot.rgba8, shot.width, 1, 0)).toBe(0xff0a0b0c);
    } finally {
      await worker.terminate();
    }
  }, 25_000);
});

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
import { allocateSharedMemorySegments, createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import {
  publishScanoutState,
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_R8G8B8A8_SRGB,
  SCANOUT_SOURCE_WDDM,
} from "../ipc/scanout_state";
import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_FORMAT_B8G8R8A8_SRGB,
  CURSOR_FORMAT_B8G8R8X8,
  publishCursorState,
} from "../ipc/cursor_state";

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

type TestWorkerInitMessage = Omit<WorkerInitMessage, "vgaFramebuffer"> & { vgaFramebuffer?: SharedArrayBuffer };

async function initHeadlessGpuWorker(worker: Worker, initMsg: TestWorkerInitMessage): Promise<void> {
  const fullInit: WorkerInitMessage = { ...initMsg, vgaFramebuffer: initMsg.vgaFramebuffer ?? initMsg.sharedFramebuffer };
  worker.postMessage(fullInit);
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
    sharedFramebuffer: fullInit.sharedFramebuffer,
    sharedFramebufferOffsetBytes: fullInit.sharedFramebufferOffsetBytes,
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
      guestRamBytes: 64 * 1024,
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
      guestRamBytes: 64 * 1024,
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
      guestRamBytes: 64 * 1024,
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

  it("reads VRAM-backed cursor surfaces even when last-row pitch padding is out of bounds", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 64,
    });
    const views = createSharedMemoryViews(segments);
    if (!segments.vram || views.vramSizeBytes === 0) {
      throw new Error("test requires a non-empty shared VRAM segment");
    }

    const scanoutPaddr = 0x1000;
    // Scanout: 1x2 BGRX pixels, both black.
    views.guestU8.set([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 2,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const cursorWidth = 1;
    const cursorHeight = 2;
    const cursorPitchBytes = 16;
    const cursorRowBytes = cursorWidth * 4;
    const cursorRequiredBytes = cursorPitchBytes * (cursorHeight - 1) + cursorRowBytes;
    if (cursorRequiredBytes > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for cursor");
    }
    const cursorVramOffset = views.vramU8.byteLength - cursorRequiredBytes;

    // Cursor: 1x2 BGRX pixels with padded pitch. Place it at the end of the VRAM SAB so the unused
    // pitch padding after the last row would be out of bounds.
    views.vramU8.fill(0);
    // Row 0: red (BGRX bytes [00 00 FF 00]).
    views.vramU8.set([0x00, 0x00, 0xff, 0x00], cursorVramOffset);
    // Row 1: green at offset + pitchBytes.
    views.vramU8.set([0x00, 0xff, 0x00, 0x00], cursorVramOffset + cursorPitchBytes);

    // Override the VRAM base paddr passed to the worker to ensure cursor readback honors it
    // (instead of assuming the arch default `VRAM_BASE_PADDR`).
    const vramBasePaddr = (VRAM_BASE_PADDR + 0x10000) >>> 0;
    const cursorBasePaddr = (vramBasePaddr + cursorVramOffset) >>> 0;
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: cursorWidth,
      height: cursorHeight,
      pitchBytes: cursorPitchBytes,
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
        vramBasePaddr,
        vramSizeBytes: segments.vram.byteLength,
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
      expect(shot.height).toBe(2);
      // Cursor is fully opaque (X8 alpha forced), so it overwrites both scanout pixels.
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff0000ff);
      expect(pixelU32At(shot.rgba8, shot.width, 0, 1)).toBe(0xff00ff00);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("syncs hardware cursor state for screenshots even when no tick is forced", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
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
      guestRamBytes: 64 * 1024,
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

  it("decodes sRGB cursor formats before compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;
    // Background pixel: black BGRX -> RGBA [0,0,0,0xff].
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

    // Cursor pixel: BGRA with R=0x80 in an sRGB format.
    // If treated as linear, we'd see R=0x80. When decoded from sRGB -> linear, it becomes ~0x37.
    views.guestU8.set([0x00, 0x00, 0x80, 0xff], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8A8_SRGB,
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
      // sRGB 0x80 -> linear ~0x37, with alpha forced to 0xff => 0xff000037.
      expect(firstPixelU32(shot.rgba8)).toBe(0xff000037);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("blends sRGB cursor over sRGB scanout in linear space when compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout pixel: RGBA with R=0x80 in an sRGB format (alpha preserved).
    // This should decode to linear R ~= 0x37.
    views.guestU8.set([0x80, 0x00, 0x00, 0xff], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_R8G8B8A8_SRGB,
    });

    // Cursor pixel: BGRA with G=0x80 in an sRGB format and 50% alpha (A=0x80).
    // This should decode to linear G ~= 0x37, then blend over the linearized scanout:
    // outR = floor((0*128 + 0x37*127 + 127)/255) = 0x1b
    // outG = floor((0x37*128 + 0*127 + 127)/255) = 0x1c
    views.guestU8.set([0x00, 0x80, 0x00, 0x80], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8A8_SRGB,
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
      expect(firstPixelU32(shot.rgba8)).toBe(0xff001c1b);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("blends sRGB cursor over sRGB scanout from the VRAM aperture in linear space when compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);
    if (!segments.vram || views.vramSizeBytes === 0) {
      throw new Error("test requires a non-empty shared VRAM segment");
    }

    const scanoutVramOffset = 0x1000;
    const cursorVramOffset = 0x2000;
    if (cursorVramOffset + 4 > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for scanout/cursor pixels");
    }

    // Scanout pixel: RGBA with R=0x80 in an sRGB format (alpha preserved).
    // This should decode to linear R ~= 0x37.
    views.vramU8.fill(0);
    views.vramU8.set([0x80, 0x00, 0x00, 0xff], scanoutVramOffset);
    const scanoutPaddr = (VRAM_BASE_PADDR + scanoutVramOffset) >>> 0;
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_R8G8B8A8_SRGB,
    });

    // Cursor pixel: BGRA with G=0x80 in an sRGB format and 50% alpha (A=0x80).
    // This should decode to linear G ~= 0x37, then blend over the linearized scanout.
    views.vramU8.set([0x00, 0x80, 0x00, 0x80], cursorVramOffset);
    const cursorPaddr = (VRAM_BASE_PADDR + cursorVramOffset) >>> 0;
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8A8_SRGB,
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

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      expect(firstPixelU32(shot.rgba8)).toBe(0xff001c1b);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("decodes sRGB cursor formats from the shared VRAM aperture before compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
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
    // Use an unaligned VRAM base for the cursor to force the byte-fallback swizzle path.
    const cursorVramOffset = 0x2001;
    if (cursorVramOffset + 4 > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for cursor pixel");
    }

    // Background pixel: black BGRX -> RGBA [0,0,0,0xff].
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

    // Cursor pixel: BGRA with R=0x80 in an sRGB format.
    // If treated as linear, we'd see R=0x80. When decoded from sRGB -> linear, it becomes ~0x37.
    views.vramU8.set([0x00, 0x00, 0x80, 0xff], cursorVramOffset);
    const cursorPaddr = (VRAM_BASE_PADDR + cursorVramOffset) >>> 0;
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: CURSOR_FORMAT_B8G8R8A8_SRGB,
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

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      // sRGB 0x80 -> linear ~0x37, with alpha forced to 0xff => 0xff000037.
      expect(firstPixelU32(shot.rgba8)).toBe(0xff000037);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("decodes sRGB scanout formats before returning screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);
    if (!segments.vram || views.vramSizeBytes === 0) {
      throw new Error("test requires a non-empty shared VRAM segment");
    }

    const scanoutVramOffset = 0x1000;
    if (scanoutVramOffset + 4 > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for scanout pixel");
    }
    // Scanout pixel: RGBA with R=0x80 in an sRGB format.
    // If treated as linear, we'd see R=0x80. When decoded from sRGB -> linear, it becomes ~0x37.
    views.vramU8.set([0x80, 0x00, 0x00, 0xff], scanoutVramOffset);
    const scanoutPaddr = (VRAM_BASE_PADDR + scanoutVramOffset) >>> 0;
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_R8G8B8A8_SRGB,
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
      });

      const shot = await requestScreenshot(worker, 1, false);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      // sRGB 0x80 -> linear ~0x37, alpha preserved => 0xff000037.
      expect(firstPixelU32(shot.rgba8)).toBe(0xff000037);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("decodes sRGB scanout formats from guest RAM before returning screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    // Scanout pixel: RGBA with R=0x80 in an sRGB format.
    // If treated as linear, we'd see R=0x80. When decoded from sRGB -> linear, it becomes ~0x37.
    views.guestU8.set([0x80, 0x00, 0x00, 0xff], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_R8G8B8A8_SRGB,
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
      });

      const shot = await requestScreenshot(worker, 1, false);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      // sRGB 0x80 -> linear ~0x37, alpha preserved => 0xff000037.
      expect(firstPixelU32(shot.rgba8)).toBe(0xff000037);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("propagates cursor alpha when the scanout pixel is transparent", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
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

  it("alpha-blends cursor pixels over partially transparent scanout pixels (alpha composited)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;
    // Background pixel: black BGRA with A=0x80 -> RGBA [0,0,0,0x80].
    views.guestU8.set([0x00, 0x00, 0x00, 0x80], scanoutPaddr);
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
    // Blend 50% red over black with dstA=0x80 => outA ~= 0xc0.
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
      // outRgb = 50% red over black => R=0x80, outA = 0x80 + 0x80*(1-0x80) ~= 0xc0 => 0xc0000080.
      expect(firstPixelU32(shot.rgba8)).toBe(0xc0000080);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("applies cursor hotX offsets when compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
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

  it("applies cursor hotY offsets when compositing screenshots", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout: 1x2 BGRX pixels.
    // pixel(0,0): BGRX [10 20 30 00] -> RGBA [30 20 10 ff] => 0xff102030
    // pixel(0,1): BGRX [01 02 03 00] -> RGBA [03 02 01 ff] => 0xff010203
    views.guestU8.set([0x10, 0x20, 0x30, 0x00, 0x01, 0x02, 0x03, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 2,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor pixel: BGRX [04 05 06 00] -> RGBA [06 05 04 ff] => 0xff040506.
    views.guestU8.set([0x04, 0x05, 0x06, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 1,
      hotX: 0,
      hotY: 1,
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

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(2);

      // y=1, hotY=1 -> originY=0, so cursor lands on pixel(0,0).
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff040506);
      expect(pixelU32At(shot.rgba8, shot.width, 0, 1)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("clips cursors that extend beyond the screenshot bounds", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
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

  it("clips cursors that extend beyond the screenshot bounds vertically", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout: 1x2 BGRX pixels.
    // pixel(0,0): 0xff102030, pixel(0,1): 0xff010203.
    views.guestU8.set([0x10, 0x20, 0x30, 0x00, 0x01, 0x02, 0x03, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 2,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor: 1x2 BGRX pixels, but positioned so the second row is off-screen.
    // cursor row0 @y=1 (drawn), row1 @y=2 (clipped).
    // row0: [0a 0b 0c 00] -> 0xff0a0b0c
    // row1: [0d 0e 0f 00] -> 0xff0d0e0f
    views.guestU8.set([0x0a, 0x0b, 0x0c, 0x00, 0x0d, 0x0e, 0x0f, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 1,
      hotX: 0,
      hotY: 0,
      width: 1,
      height: 2,
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

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(2);

      // pixel(0,0) stays scanout, pixel(0,1) becomes cursor row0; cursor row1 is clipped.
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff102030);
      expect(pixelU32At(shot.rgba8, shot.width, 0, 1)).toBe(0xff0a0b0c);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("clips cursors when hotX causes a negative origin", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout: 2x1 BGRX pixels.
    // pixel0: 0xff102030, pixel1: 0xff010203.
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

    // Cursor: 2x1 BGRX pixels. Place cursor at x=0 with hotX=1 so originX=-1.
    // cursor pixel0 would land at x=-1 (clipped), pixel1 lands at x=0 (visible).
    // pixel0: [0a 0b 0c 00] -> 0xff0a0b0c
    // pixel1: [0d 0e 0f 00] -> 0xff0d0e0f
    views.guestU8.set([0x0a, 0x0b, 0x0c, 0x00, 0x0d, 0x0e, 0x0f, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 1,
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

      // pixel0 becomes cursor pixel1, pixel1 stays scanout.
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff0d0e0f);
      expect(pixelU32At(shot.rgba8, shot.width, 1, 0)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("clips cursors when hotY causes a negative origin", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutPaddr = 0x1000;
    const cursorPaddr = 0x2000;

    // Scanout: 1x2 BGRX pixels.
    // pixel(0,0): 0xff102030, pixel(0,1): 0xff010203.
    views.guestU8.set([0x10, 0x20, 0x30, 0x00, 0x01, 0x02, 0x03, 0x00], scanoutPaddr);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutPaddr,
      basePaddrHi: 0,
      width: 1,
      height: 2,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Cursor: 1x2 BGRX pixels. Place cursor at y=0 with hotY=1 so originY=-1.
    // cursor row0 would land at y=-1 (clipped), row1 lands at y=0 (visible).
    // row0: [0a 0b 0c 00] -> 0xff0a0b0c
    // row1: [0d 0e 0f 00] -> 0xff0d0e0f
    views.guestU8.set([0x0a, 0x0b, 0x0c, 0x00, 0x0d, 0x0e, 0x0f, 0x00], cursorPaddr);
    publishCursorState(views.cursorStateI32!, {
      enable: 1,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 1,
      width: 1,
      height: 2,
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

      const shot = await requestScreenshot(worker, 1, true);
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(2);

      // pixel(0,0) becomes cursor row1, pixel(0,1) stays scanout.
      expect(pixelU32At(shot.rgba8, shot.width, 0, 0)).toBe(0xff0d0e0f);
      expect(pixelU32At(shot.rgba8, shot.width, 0, 1)).toBe(0xff010203);
    } finally {
      await worker.terminate();
    }
  }, 25_000);
});

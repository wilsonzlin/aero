import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { VRAM_BASE_PADDR } from "../arch/guest_phys";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import {
  publishScanoutState,
  SCANOUT_FORMAT_B5G5R5A1,
  SCANOUT_FORMAT_B5G6R5,
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8A8_SRGB,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_B8G8R8X8_SRGB,
  SCANOUT_FORMAT_R8G8B8A8,
  SCANOUT_FORMAT_R8G8B8X8,
  SCANOUT_FORMAT_R8G8B8X8_SRGB,
  SCANOUT_SOURCE_LEGACY_VBE_LFB,
  SCANOUT_STATE_GENERATION_BUSY_BIT,
  ScanoutStateIndex,
  SCANOUT_SOURCE_WDDM,
} from "../ipc/scanout_state";
import { CURSOR_STATE_GENERATION_BUSY_BIT, CursorStateIndex } from "../ipc/cursor_state";

// These worker-thread integration tests can be sensitive to scheduling jitter in CI/agent
// sandboxes (especially on newer Node majors). Keep timeouts generous to avoid flakes.
const WORKER_MESSAGE_TIMEOUT_MS = 20_000;
const TEST_TIMEOUT_MS = 70_000;

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

function writeBgrx2x2(dst: Uint8Array, pitchBytes: number): void {
  // 2x2 pixels, rowBytes=8.
  // Quadrants (top-left origin):
  // - TL: red
  // - TR: green
  // - BL: blue
  // - BR: white
  //
  // BGRX bytes with X intentionally 0 to validate alpha=255 policy.
  if (pitchBytes < 8) throw new Error("pitch too small");
  dst.fill(0);
  // Row 0: TL red, TR green.
  dst.set([0x00, 0x00, 0xff, 0x00], 0); // red
  dst.set([0x00, 0xff, 0x00, 0x00], 4); // green
  // Row 1: BL blue, BR white.
  dst.set([0xff, 0x00, 0x00, 0x00], pitchBytes + 0); // blue
  dst.set([0xff, 0xff, 0xff, 0x00], pitchBytes + 4); // white
}

function writeRgbx2x2(dst: Uint8Array, pitchBytes: number): void {
  // Same quadrant pattern as `writeBgrx2x2`, but in RGBX byte order.
  //
  // RGBX bytes with X intentionally 0 to validate alpha=255 policy.
  if (pitchBytes < 8) throw new Error("pitch too small");
  dst.fill(0);
  // Row 0: TL red, TR green.
  dst.set([0xff, 0x00, 0x00, 0x00], 0); // red
  dst.set([0x00, 0xff, 0x00, 0x00], 4); // green
  // Row 1: BL blue, BR white.
  dst.set([0x00, 0x00, 0xff, 0x00], pitchBytes + 0); // blue
  dst.set([0xff, 0xff, 0xff, 0x00], pitchBytes + 4); // white
}

function writeBgra2x2(dst: Uint8Array, pitchBytes: number): void {
  // Same colors as `writeBgrx2x2`, but with distinct alpha values per pixel so tests can assert
  // alpha preservation.
  if (pitchBytes < 8) throw new Error("pitch too small");
  dst.fill(0);
  // Row 0: TL red, TR green.
  dst.set([0x00, 0x00, 0xff, 0x11], 0); // red, A=0x11
  dst.set([0x00, 0xff, 0x00, 0x22], 4); // green, A=0x22
  // Row 1: BL blue, BR white.
  dst.set([0xff, 0x00, 0x00, 0x33], pitchBytes + 0); // blue, A=0x33
  dst.set([0xff, 0xff, 0xff, 0x44], pitchBytes + 4); // white, A=0x44
}

function writeRgba2x2(dst: Uint8Array, pitchBytes: number): void {
  // Same colors as `writeBgra2x2`, but in RGBA byte order.
  if (pitchBytes < 8) throw new Error("pitch too small");
  dst.fill(0);
  // Row 0: TL red, TR green.
  dst.set([0xff, 0x00, 0x00, 0x11], 0); // red, A=0x11
  dst.set([0x00, 0xff, 0x00, 0x22], 4); // green, A=0x22
  // Row 1: BL blue, BR white.
  dst.set([0x00, 0x00, 0xff, 0x33], pitchBytes + 0); // blue, A=0x33
  dst.set([0xff, 0xff, 0xff, 0x44], pitchBytes + 4); // white, A=0x44
}

function writeRgb5652x2(dst: Uint8Array, pitchBytes: number): void {
  // 2x2 pixels, rowBytes=4.
  // Quadrants (top-left origin):
  // - TL: red
  // - TR: green
  // - BL: blue
  // - BR: white
  //
  // RGB565 values (little-endian):
  // - red   = 0xF800 (00 F8)
  // - green = 0x07E0 (E0 07)
  // - blue  = 0x001F (1F 00)
  // - white = 0xFFFF (FF FF)
  if (pitchBytes < 4) throw new Error("pitch too small");
  dst.fill(0);
  // Row 0: red, green.
  dst.set([0x00, 0xf8, 0xe0, 0x07], 0);
  // Row 1: blue, white.
  dst.set([0x1f, 0x00, 0xff, 0xff], pitchBytes);
}

describe("workers/gpu-worker WDDM scanout readback", () => {
  it("reads BGRX scanout from guest RAM (honors pitch, forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16; // larger than rowBytes (8)
    const basePaddr = 0x1000;
    const requiredBytes = pitchBytes * height;

    views.guestU8.fill(0);
    writeBgrx2x2(views.guestU8.subarray(basePaddr, basePaddr + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads B5G6R5 scanout from guest RAM even when last-row pitch padding is out of bounds (expands to RGBA8)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 8; // padded (srcRowBytes=4)
    const srcRowBytes = width * 2;
    const requiredBytes = pitchBytes * (height - 1) + srcRowBytes;
    // Place the surface at the end of guest RAM so unused pitch padding after the last row would
    // be out of bounds if the worker incorrectly required `pitchBytes * height`.
    const basePaddr = views.guestU8.byteLength - requiredBytes;

    views.guestU8.fill(0);
    writeRgb5652x2(views.guestU8.subarray(basePaddr, basePaddr + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B5G6R5,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads B5G5R5A1 scanout from the shared VRAM aperture even when last-row pitch padding is out of bounds (expands alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 64,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 8; // padded (srcRowBytes=4)
    const srcRowBytes = width * 2;
    const requiredBytes = pitchBytes * (height - 1) + srcRowBytes;
    if (requiredBytes > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for B5G5R5A1 surface");
    }
    // Place the surface at the end of the VRAM SAB so the unused pitch padding after the last row
    // would be out of bounds if the readback path incorrectly required `pitchBytes * height`.
    const vramOffset = views.vramU8.byteLength - requiredBytes;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    views.vramU8.fill(0);
    // Two rows of pixels:
    // Row 0: red with alpha=1 (0xFC00), red with alpha=0 (0x7C00).
    views.vramU8.set([0x00, 0xfc, 0x00, 0x7c], vramOffset);
    // Row 1: green with alpha=1 (0x83E0), blue with alpha=0 (0x001F).
    views.vramU8.set([0xe0, 0x83, 0x1f, 0x00], vramOffset + pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B5G5R5A1,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
        vramBasePaddr: VRAM_BASE_PADDR,
        vramSizeBytes: segments.vram!.byteLength,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red A=1, red A=0.
        0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00,
        // Row 1: green A=1, blue A=0.
        0x00, 0xff, 0x00, 0xff, 0x00, 0x00, 0xff, 0x00,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRX scanout from guest RAM with a byte-granular pitch (forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    // 2x2 pixels => rowBytes=8. Use a non-4-byte-aligned pitch to exercise the byte fallback path.
    const pitchBytes = 9;
    const basePaddr = 0x3000; // aligned base address
    const requiredBytes = pitchBytes * height;

    views.guestU8.fill(0);
    writeBgrx2x2(views.guestU8.subarray(basePaddr, basePaddr + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads RGBX scanout from guest RAM (honors pitch, forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16; // larger than rowBytes (8)
    const basePaddr = 0x2000;
    const requiredBytes = pitchBytes * height;

    views.guestU8.fill(0);
    writeRgbx2x2(views.guestU8.subarray(basePaddr, basePaddr + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_R8G8B8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRA scanout from guest RAM (honors pitch, preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16; // larger than rowBytes (8)
    // Use an unaligned base address to force the byte-fallback swizzle path.
    const basePaddr = 0x1001;
    const requiredBytes = pitchBytes * height;

    views.guestU8.fill(0);
    writeBgra2x2(views.guestU8.subarray(basePaddr, basePaddr + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8A8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0x11, 0x00, 0xff, 0x00, 0x22,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0x33, 0xff, 0xff, 0xff, 0x44,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("decodes sRGB BGRA scanout from guest RAM (linearizes, preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 1;
    const height = 1;
    const pitchBytes = 4;
    // Use an unaligned base address to force the byte-fallback swizzle path.
    const basePaddr = 0x2001;

    // BGRA pixel with R=0x80 and A=0x11 in an sRGB format.
    // After swizzle + sRGB->linear decode: R ~= 0x37, alpha preserved.
    views.guestU8.fill(0);
    views.guestU8.set([0x00, 0x00, 0x80, 0x11], basePaddr);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8A8_SRGB,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([0x37, 0x00, 0x00, 0x11]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("decodes sRGB X8 scanout formats from guest RAM (linearizes, forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 1;
    const height = 1;
    const pitchBytes = 4;

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      // Case 1: BGRX sRGB (R byte is in the 3rd position).
      const basePaddrBgrx = 0x3000;
      views.guestU8.fill(0);
      views.guestU8.set([0x00, 0x00, 0x80, 0x00], basePaddrBgrx);
      publishScanoutState(views.scanoutStateI32!, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: basePaddrBgrx >>> 0,
        basePaddrHi: 0,
        width,
        height,
        pitchBytes,
        format: SCANOUT_FORMAT_B8G8R8X8_SRGB,
      });

      {
        const requestId = 1;
        const shotPromise = waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
            (msg as { type?: unknown }).type === "screenshot" &&
            (msg as { requestId?: unknown }).requestId === requestId,
          WORKER_MESSAGE_TIMEOUT_MS,
        );
        worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });
        const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
        expect(shot.width).toBe(width);
        expect(shot.height).toBe(height);
        expect(Array.from(new Uint8Array(shot.rgba8))).toEqual([0x37, 0x00, 0x00, 0xff]);
      }

      // Case 2: RGBX sRGB (R byte is in the 1st position).
      const basePaddrRgbx = 0x4000;
      views.guestU8.set([0x80, 0x00, 0x00, 0x00], basePaddrRgbx);
      publishScanoutState(views.scanoutStateI32!, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: basePaddrRgbx >>> 0,
        basePaddrHi: 0,
        width,
        height,
        pitchBytes,
        format: SCANOUT_FORMAT_R8G8B8X8_SRGB,
      });

      {
        const requestId = 2;
        const shotPromise = waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
            (msg as { type?: unknown }).type === "screenshot" &&
            (msg as { requestId?: unknown }).requestId === requestId,
          WORKER_MESSAGE_TIMEOUT_MS,
        );
        worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });
        const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
        expect(shot.width).toBe(width);
        expect(shot.height).toBe(height);
        expect(Array.from(new Uint8Array(shot.rgba8))).toEqual([0x37, 0x00, 0x00, 0xff]);
      }
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRX scanout from the shared VRAM aperture when base_paddr points into BAR1", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const vramOffset = 0x1000;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;
    const requiredBytes = pitchBytes * height;

    views.vramU8.fill(0);
    writeBgrx2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("decodes sRGB BGRX scanout from the shared VRAM aperture (linearizes, forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 1;
    const height = 1;
    const pitchBytes = 4;
    const vramOffset = 0x2000;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    // BGRX pixel with R=0x80 in an sRGB format (X byte intentionally 0).
    // After swizzle + sRGB->linear decode: R ~= 0x37.
    views.vramU8.fill(0);
    views.vramU8.set([0x00, 0x00, 0x80, 0x00], vramOffset);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8_SRGB,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([0x37, 0x00, 0x00, 0xff]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("decodes sRGB BGRA scanout from the shared VRAM aperture (linearizes, preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 1;
    const height = 1;
    const pitchBytes = 4;
    const vramOffset = 0x2100;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    // BGRA pixel with R=0x80 and A=0x11 in an sRGB format.
    // After swizzle + sRGB->linear decode: R ~= 0x37, alpha preserved.
    views.vramU8.fill(0);
    views.vramU8.set([0x00, 0x00, 0x80, 0x11], vramOffset);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8A8_SRGB,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([0x37, 0x00, 0x00, 0x11]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRX scanout from VRAM when the WorkerInitMessage vramBasePaddr is overridden", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const vramOffset = 0x1000;
    const vramBasePaddr = (VRAM_BASE_PADDR + 0x10000) >>> 0;
    const basePaddr = (vramBasePaddr + vramOffset) >>> 0;
    const requiredBytes = pitchBytes * height;

    views.vramU8.fill(0);
    writeBgrx2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
        vramBasePaddr,
        vramSizeBytes: segments.vram!.byteLength,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRX scanout from the shared VRAM aperture even when last-row pitch padding is out of bounds", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 64,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const rowBytes = width * 4;
    // Only the last row's pixel bytes must be present; the unused pitch padding after the last row
    // is not required for readback.
    const requiredReadBytes = pitchBytes * (height - 1) + rowBytes;
    const vramOffset = views.vramU8.byteLength - requiredReadBytes;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    views.vramU8.fill(0);
    writeBgrx2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredReadBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads RGBX scanout from the shared VRAM aperture when base_paddr points into BAR1", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const vramOffset = 0x2000;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;
    const requiredBytes = pitchBytes * height;

    views.vramU8.fill(0);
    writeRgbx2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_R8G8B8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads RGBA scanout from the shared VRAM aperture when base_paddr points into BAR1 (preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const vramOffset = 0x3000;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;
    const requiredBytes = pitchBytes * height;

    views.vramU8.fill(0);
    writeRgba2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_R8G8B8A8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0x11, 0x00, 0xff, 0x00, 0x22,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0x33, 0xff, 0xff, 0xff, 0x44,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRA scanout from the shared VRAM aperture when base_paddr points into BAR1 (preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const vramOffset = 0x1000;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;
    const requiredBytes = pitchBytes * height;

    views.vramU8.fill(0);
    writeBgra2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8A8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        0xff, 0x00, 0x00, 0x11, 0x00, 0xff, 0x00, 0x22, 0x00, 0x00, 0xff, 0x33, 0xff, 0xff, 0xff, 0x44,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads BGRA scanout from the shared VRAM aperture with an unaligned base_paddr (byte fallback preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const rowBytes = width * 4;
    const requiredReadBytes = pitchBytes * (height - 1) + rowBytes;
    const vramOffset = 0x1001; // unaligned => forces byte fallback swizzle path
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    views.vramU8.fill(0);
    writeBgra2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredReadBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8A8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
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
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0x11, 0x00, 0xff, 0x00, 0x22,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0x33, 0xff, 0xff, 0xff, 0x44,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("returns a stub screenshot for legacy VBE LFB scanout when base_paddr is 0", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 2,
      height: 2,
      pitchBytes: 16,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
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
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      expect(Array.from(new Uint8Array(shot.rgba8))).toEqual([0x00, 0x00, 0x00, 0xff]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads legacy VBE LFB scanout from guest RAM (honors pitch, forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16; // larger than rowBytes (8)
    const basePaddr = 0x3000;
    const requiredBytes = pitchBytes * height;

    views.guestU8.fill(0);
    writeBgrx2x2(views.guestU8.subarray(basePaddr, basePaddr + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
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
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads legacy VBE LFB scanout from the shared VRAM aperture when base_paddr points into BAR1", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 1 * 1024 * 1024,
    });
    const views = createSharedMemoryViews(segments);

    const width = 2;
    const height = 2;
    const pitchBytes = 16;
    const vramOffset = 0x4000;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;
    const requiredBytes = pitchBytes * height;

    views.vramU8.fill(0);
    writeBgrx2x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
      basePaddrLo: basePaddr,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "gpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vram: segments.vram,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(width);
      expect(shot.height).toBe(height);

      const px = new Uint8Array(shot.rgba8);
      expect(Array.from(px)).toEqual([
        // Row 0: red, green.
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("returns a stub screenshot quickly when scanout/cursor seqlock busy bits are stuck", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const scanoutWords = views.scanoutStateI32!;
    const cursorWords = views.cursorStateI32!;

    const basePaddr = 0x1000;
    Atomics.store(scanoutWords, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_WDDM | 0);
    Atomics.store(scanoutWords, ScanoutStateIndex.BASE_PADDR_LO, basePaddr | 0);
    Atomics.store(scanoutWords, ScanoutStateIndex.BASE_PADDR_HI, 0);
    Atomics.store(scanoutWords, ScanoutStateIndex.WIDTH, 1);
    Atomics.store(scanoutWords, ScanoutStateIndex.HEIGHT, 1);
    Atomics.store(scanoutWords, ScanoutStateIndex.PITCH_BYTES, 4);
    Atomics.store(scanoutWords, ScanoutStateIndex.FORMAT, SCANOUT_FORMAT_B8G8R8X8 | 0);
    // Wedge the busy bit (simulate a crashed writer holding the seqlock).
    Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, (SCANOUT_STATE_GENERATION_BUSY_BIT | 1) | 0);
    Atomics.store(cursorWords, CursorStateIndex.GENERATION, (CURSOR_STATE_GENERATION_BUSY_BIT | 1) | 0);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
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
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        WORKER_MESSAGE_TIMEOUT_MS,
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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        WORKER_MESSAGE_TIMEOUT_MS,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        WORKER_MESSAGE_TIMEOUT_MS,
      );
      worker.postMessage({
        protocol: GPU_PROTOCOL_NAME,
        protocolVersion: GPU_PROTOCOL_VERSION,
        type: "screenshot",
        requestId,
        includeCursor: true,
      });

      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      expect(Array.from(new Uint8Array(shot.rgba8))).toEqual([0, 0, 0, 255]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);
}); 

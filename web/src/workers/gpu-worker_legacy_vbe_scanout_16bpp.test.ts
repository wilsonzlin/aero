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
  SCANOUT_SOURCE_LEGACY_VBE_LFB,
} from "../ipc/scanout_state";

const WORKER_MESSAGE_TIMEOUT_MS = 15_000;
const TEST_TIMEOUT_MS = 40_000;

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

describe("workers/gpu-worker legacy VBE scanout (16bpp)", () => {
  it("reads legacy VBE 16bpp scanout from guest RAM (expands to RGBA8)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Case 1: B5G6R5 2x2, placed at the end of guest RAM so the unused pitch padding after the
    // last row would be out of bounds if the worker incorrectly required `pitchBytes * height`.
    const rgb565Width = 2;
    const rgb565Height = 2;
    const rgb565PitchBytes = 8; // padded (rowBytes=4)
    const rgb565RowBytes = rgb565Width * 2;
    const rgb565RequiredBytes = rgb565PitchBytes * (rgb565Height - 1) + rgb565RowBytes;
    const rgb565BasePaddr = views.guestU8.byteLength - rgb565RequiredBytes;

    views.guestU8.fill(0);
    writeRgb5652x2(views.guestU8.subarray(rgb565BasePaddr, rgb565BasePaddr + rgb565RequiredBytes), rgb565PitchBytes);
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
      basePaddrLo: rgb565BasePaddr >>> 0,
      basePaddrHi: 0,
      width: rgb565Width,
      height: rgb565Height,
      pitchBytes: rgb565PitchBytes,
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

      // Screenshot B5G6R5.
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
        expect(shot.width).toBe(rgb565Width);
        expect(shot.height).toBe(rgb565Height);
        expect(Array.from(new Uint8Array(shot.rgba8))).toEqual([
          // Row 0: red, green.
          0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
          // Row 1: blue, white.
          0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]);
      }

      // Case 2: B5G5R5A1 2x1.
      const b5g5r5a1Width = 2;
      const b5g5r5a1Height = 1;
      const b5g5r5a1PitchBytes = 4;
      const b5g5r5a1BasePaddr = 0x2000;

      // Two pixels: red with alpha=1 (0xFC00), red with alpha=0 (0x7C00).
      views.guestU8.set([0x00, 0xfc, 0x00, 0x7c], b5g5r5a1BasePaddr);
      publishScanoutState(views.scanoutStateI32!, {
        source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
        basePaddrLo: b5g5r5a1BasePaddr >>> 0,
        basePaddrHi: 0,
        width: b5g5r5a1Width,
        height: b5g5r5a1Height,
        pitchBytes: b5g5r5a1PitchBytes,
        format: SCANOUT_FORMAT_B5G5R5A1,
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
        expect(shot.width).toBe(b5g5r5a1Width);
        expect(shot.height).toBe(b5g5r5a1Height);
        expect(Array.from(new Uint8Array(shot.rgba8))).toEqual([
          // Pixel 0: A=1
          0xff, 0x00, 0x00, 0xff,
          // Pixel 1: A=0
          0xff, 0x00, 0x00, 0x00,
        ]);
      }
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads B5G6R5 legacy VBE scanout from the VRAM aperture even when last-row pitch padding is out of bounds", async () => {
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
    const pitchBytes = 8; // padded (rowBytes=4)
    const rowBytes = width * 2;
    const requiredBytes = pitchBytes * (height - 1) + rowBytes;
    if (requiredBytes > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for RGB565 surface");
    }
    // Place the surface at the end of the VRAM SAB so the unused pitch padding after the last row
    // would be out of bounds if the readback path incorrectly required `pitchBytes * height`.
    const vramOffset = views.vramU8.byteLength - requiredBytes;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    views.vramU8.fill(0);
    writeRgb5652x2(views.vramU8.subarray(vramOffset, vramOffset + requiredBytes), pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
      basePaddrLo: basePaddr,
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
        0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff,
        // Row 1: blue, white.
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
      ]);
    } finally {
      await worker.terminate();
    }
  }, TEST_TIMEOUT_MS);

  it("reads B5G5R5A1 legacy VBE scanout from the VRAM aperture even when last-row pitch padding is out of bounds", async () => {
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
    const pitchBytes = 8; // padded (rowBytes=4)
    const rowBytes = width * 2;
    const requiredBytes = pitchBytes * (height - 1) + rowBytes;
    if (requiredBytes > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for B5G5R5A1 surface");
    }
    const vramOffset = views.vramU8.byteLength - requiredBytes;
    const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;

    views.vramU8.fill(0);
    // Two rows of pixels:
    // Row 0: red with alpha=1 (0xFC00), red with alpha=0 (0x7C00).
    views.vramU8.set([0x00, 0xfc, 0x00, 0x7c], vramOffset);
    // Row 1: green with alpha=1 (0x83E0), blue with alpha=0 (0x001F).
    views.vramU8.set([0xe0, 0x83, 0x1f, 0x00], vramOffset + pitchBytes);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
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
});

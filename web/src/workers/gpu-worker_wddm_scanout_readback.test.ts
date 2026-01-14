import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { VRAM_BASE_PADDR } from "../arch/guest_phys";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8A8, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";

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

describe("workers/gpu-worker WDDM scanout readback", () => {
  it("reads BGRX scanout from guest RAM (honors pitch, forces alpha=255)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
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
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        10_000,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        10_000,
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
  }, 20_000);

  it("reads BGRX scanout from the shared VRAM aperture when base_paddr points into BAR1", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
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
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        10_000,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        10_000,
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
  }, 20_000);

  it("reads BGRA scanout from the shared VRAM aperture when base_paddr points into BAR1 (preserves alpha)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
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
        vgaFramebuffer: segments.sharedFramebuffer,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

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
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        10_000,
      );

      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        10_000,
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
  }, 20_000);
});

import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { VRAM_BASE_PADDR } from "../arch/guest_phys";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
import { AerogpuCmdWriter } from "../../../emulator/protocol/aerogpu/aerogpu_cmd";
import { AerogpuFormat } from "../../../emulator/protocol/aerogpu/aerogpu_pci";

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

describe("workers/gpu-worker WDDM scanout screenshot refresh", () => {
  it("captures updated VRAM-backed scanout bytes even when requested between ticks", async () => {
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

    const vramOffset = 0x1000;
    if (vramOffset + 4 > views.vramU8.byteLength) {
      throw new Error("vram buffer too small for scanout pixel");
    }

    const writeBgrxPixel = (b: number, g: number, r: number) => {
      views.vramU8.set([b & 0xff, g & 0xff, r & 0xff, 0x00], vramOffset);
    };

    // Initial scanout pixel: BGRX -> RGBA 11 22 33 FF after swizzle + alpha policy.
    writeBgrxPixel(0x33, 0x22, 0x11);

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
        vramSizeBytes: segments.vram.byteLength,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

      // Control-plane init (sets up rings + status).
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

      // Seed an AeroGPU-presented frame so the screenshot code proves it prefers scanout bytes
      // when scanoutState.base_paddr points into VRAM.
      const writer = new AerogpuCmdWriter();
      const texHandle = 1;
      writer.createTexture2d(texHandle, 0, AerogpuFormat.R8G8B8A8Unorm, 1, 1, 1, 1, 0, 0, 0);
      // RGBA = AA BB CC DD -> u32 0xddccbbaa (little-endian).
      writer.uploadResource(texHandle, 0n, new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
      writer.setRenderTargets([texHandle], 0);
      writer.present(0, 0);
      // `postMessage` transfer lists only accept transferable buffers (`ArrayBuffer`, not
      // `ArrayBufferLike`). Force an `ArrayBuffer`-backed copy so TS 5.9+ doesn't treat this as a
      // potential `SharedArrayBuffer`.
      const cmdStream = writer.finish().slice().buffer;

      const aerogpuRequestId = 100;
      const submitCompletePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "submit_complete" &&
          (msg as { requestId?: unknown }).requestId === aerogpuRequestId,
        10_000,
      );
      worker.postMessage(
        {
          protocol: GPU_PROTOCOL_NAME,
          protocolVersion: GPU_PROTOCOL_VERSION,
          type: "submit_aerogpu",
          requestId: aerogpuRequestId,
          contextId: 0,
          signalFence: 1n,
          cmdStream,
        },
        [cmdStream],
      );
      await submitCompletePromise;

      const basePaddr = (VRAM_BASE_PADDR + vramOffset) >>> 0;
      publishScanoutState(views.scanoutStateI32!, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: basePaddr,
        basePaddrHi: 0,
        width: 1,
        height: 1,
        pitchBytes: 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

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

      const shot1 = await requestScreenshot(1);
      expect(shot1.width).toBe(1);
      expect(shot1.height).toBe(1);
      expect(firstPixelU32(shot1.rgba8)).toBe(0xff332211);

      // Update VRAM scanout contents without sending a tick. The screenshot request should still
      // force a scanout readback so it reflects the latest scanout state.
      writeBgrxPixel(0x66, 0x55, 0x44);
      // Ensure the VRAM write is visible to the worker before we request a screenshot.
      //
      // JS shared-memory writes (e.g. `Uint8Array#set` into a SharedArrayBuffer) are not inherently
      // synchronized across threads. Create a happens-before edge by mutating a value that the GPU
      // worker will read via `Atomics.load()` before it reads scanout bytes.
      //
      // We use `FRAME_SEQ_INDEX` as the sync variable because:
      // - the worker reads it on tick/screenshot paths
      // - the test doesn't care about its semantic value (we're in scanout mode)
      Atomics.store(frameState, FRAME_SEQ_INDEX, (Atomics.load(frameState, FRAME_SEQ_INDEX) + 1) | 0);

      const shot2 = await requestScreenshot(2);
      expect(shot2.width).toBe(1);
      expect(shot2.height).toBe(1);
      expect(firstPixelU32(shot2.rgba8)).toBe(0xff665544);
    } finally {
      await worker.terminate();
    }
  }, 25_000);

  it("captures updated guest-RAM-backed scanout bytes even when requested between ticks", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const basePaddr = 0x1000;
    if (basePaddr + 4 > views.guestU8.byteLength) {
      throw new Error("guest buffer too small for scanout pixel");
    }

    const writeBgrxPixel = (b: number, g: number, r: number) => {
      views.guestU8.set([b & 0xff, g & 0xff, r & 0xff, 0x00], basePaddr);
    };

    // Initial scanout pixel: BGRX -> RGBA 11 22 33 FF after swizzle + alpha policy.
    writeBgrxPixel(0x33, 0x22, 0x11);

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

      // Control-plane init (sets up rings + status).
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

      // Seed an AeroGPU-presented frame so the screenshot code proves it prefers scanout bytes.
      const writer = new AerogpuCmdWriter();
      const texHandle = 1;
      writer.createTexture2d(texHandle, 0, AerogpuFormat.R8G8B8A8Unorm, 1, 1, 1, 1, 0, 0, 0);
      writer.uploadResource(texHandle, 0n, new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
      writer.setRenderTargets([texHandle], 0);
      writer.present(0, 0);
      // `postMessage` transfer lists only accept transferable `ArrayBuffer` (not `SharedArrayBuffer`).
      const cmdStream = writer.finish().slice().buffer;

      const aerogpuRequestId = 100;
      const submitCompletePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "submit_complete" &&
          (msg as { requestId?: unknown }).requestId === aerogpuRequestId,
        10_000,
      );
      worker.postMessage(
        {
          protocol: GPU_PROTOCOL_NAME,
          protocolVersion: GPU_PROTOCOL_VERSION,
          type: "submit_aerogpu",
          requestId: aerogpuRequestId,
          contextId: 0,
          signalFence: 1n,
          cmdStream,
        },
        [cmdStream],
      );
      await submitCompletePromise;

      publishScanoutState(views.scanoutStateI32!, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: basePaddr >>> 0,
        basePaddrHi: 0,
        width: 1,
        height: 1,
        pitchBytes: 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

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

      const shot1 = await requestScreenshot(1);
      expect(shot1.width).toBe(1);
      expect(shot1.height).toBe(1);
      expect(firstPixelU32(shot1.rgba8)).toBe(0xff332211);

      // Update guest RAM scanout contents without sending a tick.
      writeBgrxPixel(0x66, 0x55, 0x44);
      Atomics.store(frameState, FRAME_SEQ_INDEX, (Atomics.load(frameState, FRAME_SEQ_INDEX) + 1) | 0);

      const shot2 = await requestScreenshot(2);
      expect(shot2.width).toBe(1);
      expect(shot2.height).toBe(1);
      expect(firstPixelU32(shot2.rgba8)).toBe(0xff665544);
    } finally {
      await worker.terminate();
    }
  }, 25_000);
});

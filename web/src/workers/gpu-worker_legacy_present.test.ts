import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateSharedMemorySegments } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
} from "../ipc/gpu-protocol";
import {
  layoutFromHeader,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
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

describe("workers/gpu-worker legacy framebuffer plumbing", () => {
  it("presents from sharedFramebuffer via a mock presenter module (no vgaFramebuffer)", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

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

      // Load mock presenter module (installs present()).
      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_module.ts", import.meta.url).href;

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
        options: { wasmModuleUrl },
      });

      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_presenter_loaded", 10_000);

      // Publish a single shared-layout frame with a distinctive first pixel so we can prove which
      // buffer the worker consumed.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const layout = layoutFromHeader(header);

      const slot0 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[0],
        layout.strideBytes * layout.height,
      );
      const slot1 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[1],
        layout.strideBytes * layout.height,
      );

      const dirty0 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[0],
              layout.dirtyWordsPerBuffer,
            );
      const dirty1 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[1],
              layout.dirtyWordsPerBuffer,
            );

      const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
      const back = active ^ 1;
      const backPixels = back === 0 ? slot0 : slot1;
      const backDirty = back === 0 ? dirty0 : dirty1;

      // First pixel = 0x44332211 (little-endian bytes 11 22 33 44).
      backPixels.fill(0);
      backPixels[0] = 0x11;
      backPixels[1] = 0x22;
      backPixels[2] = 0x33;
      backPixels[3] = 0x44;
      if (backDirty) backDirty.fill(0xffffffff);

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

      Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      // Drive a tick to force present().
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const presentMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === true,
        10_000,
      )) as { firstPixel?: number; seq?: number };

      expect(presentMsg.firstPixel).toBe(0x44332211);
      expect(presentMsg.seq).toBe(newSeq >>> 0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("counts dropped presents when the presenter module returns false", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

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

      // Load mock presenter module that drops frames.
      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_drop_module.ts", import.meta.url).href;

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
        options: { wasmModuleUrl },
      });

      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_presenter_loaded", 10_000);

      // Publish a single shared-layout frame with a distinctive first pixel.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const layout = layoutFromHeader(header);

      const slot0 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[0],
        layout.strideBytes * layout.height,
      );
      const slot1 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[1],
        layout.strideBytes * layout.height,
      );

      const dirty0 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[0],
              layout.dirtyWordsPerBuffer,
            );
      const dirty1 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[1],
              layout.dirtyWordsPerBuffer,
            );

      const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
      const back = active ^ 1;
      const backPixels = back === 0 ? slot0 : slot1;
      const backDirty = back === 0 ? dirty0 : dirty1;

      // First pixel = 0x44332211 (little-endian bytes 11 22 33 44).
      backPixels.fill(0);
      backPixels[0] = 0x11;
      backPixels[1] = 0x22;
      backPixels[2] = 0x33;
      backPixels[3] = 0x44;
      if (backDirty) backDirty.fill(0xffffffff);

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

      Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      // Drive a tick to force present().
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const presentMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === false,
        10_000,
      )) as { firstPixel?: number; seq?: number };

      expect(presentMsg.firstPixel).toBe(0x44332211);
      expect(presentMsg.seq).toBe(newSeq >>> 0);

      // The worker should clear the shared framebuffer DIRTY flag even on drops (chosen semantics).
      expect(Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_DIRTY)).toBe(0);
      // Shared frame pacing should still advance back to PRESENTED.
      expect(Atomics.load(frameState, FRAME_STATUS_INDEX)).toBe(FRAME_PRESENTED);

      // Metrics are rate-limited; wait long enough for a metrics post, then tick again so the
      // shared counters are synchronized.
      await new Promise((resolve) => setTimeout(resolve, 300));
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const metricsMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "metrics",
        10_000,
      )) as { framesPresented?: number; framesDropped?: number };

      expect(metricsMsg.framesPresented).toBe(0);
      expect(metricsMsg.framesDropped).toBe(1);
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

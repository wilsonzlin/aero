import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import {
  FramebufferFormat,
  computeSharedFramebufferLayout,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import { AEROGPU_CMD_STREAM_HEADER_SIZE, AEROGPU_CMD_STREAM_MAGIC } from "../../../emulator/protocol/aerogpu/aerogpu_cmd";
import { AEROGPU_ABI_VERSION_U32 } from "../../../emulator/protocol/aerogpu/aerogpu_pci";

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

function createMinimalSharedFramebuffer(): SharedArrayBuffer {
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
  return sharedFramebuffer;
}

function buildEmptyAerogpuCmdStream(): ArrayBuffer {
  const buf = new ArrayBuffer(AEROGPU_CMD_STREAM_HEADER_SIZE);
  const dv = new DataView(buf);
  dv.setUint32(0, AEROGPU_CMD_STREAM_MAGIC, true);
  dv.setUint32(4, AEROGPU_ABI_VERSION_U32, true);
  dv.setUint32(8, AEROGPU_CMD_STREAM_HEADER_SIZE, true);
  dv.setUint32(12, 0, true);
  dv.setUint32(16, 0, true);
  dv.setUint32(20, 0, true);
  return buf;
}

describe("workers/gpu-worker snapshot pause", () => {
  it("does not execute submit_aerogpu while snapshot-paused (submit_complete gated until resume)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 1 * 1024 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
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
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
        vram: segments.vram,
        vramSizeBytes: segments.vram?.byteLength ?? 0,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
      );

      // Pause the worker.
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as any)?.kind === "vm.snapshot.paused" && (msg as any)?.requestId === 1 && (msg as any)?.ok === true,
        5_000,
      );

      // Submit an AeroGPU command stream while paused.
      const cmdStream = buildEmptyAerogpuCmdStream();
      worker.postMessage(
        {
          protocol: GPU_PROTOCOL_NAME,
          protocolVersion: GPU_PROTOCOL_VERSION,
          type: "submit_aerogpu",
          requestId: 7,
          contextId: 0,
          signalFence: 1n,
          cmdStream,
        },
        [cmdStream],
      );

      // Ensure the submission does not complete while paused.
      await expect(
        waitForWorkerMessage(
          worker,
          (msg) => (msg as any)?.type === "submit_complete" && (msg as any)?.requestId === 7,
          100,
        ),
      ).rejects.toThrow(/timed out/i);

      // Resume the worker, then ensure the submission completes.
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as any)?.kind === "vm.snapshot.resumed" && (msg as any)?.requestId === 2 && (msg as any)?.ok === true,
        5_000,
      );

      const complete = await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as any)?.type === "submit_complete" &&
          (msg as any)?.requestId === 7 &&
          (msg as any)?.protocol === GPU_PROTOCOL_NAME &&
          (msg as any)?.protocolVersion === GPU_PROTOCOL_VERSION,
        5_000,
      );
      expect((complete as any).completedFence).toBe(1n);
    } finally {
      await worker.terminate();
    }
  });
});

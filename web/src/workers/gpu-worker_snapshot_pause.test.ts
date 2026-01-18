import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { unrefBestEffort } from "../unrefSafe";
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
  FramebufferFormat,
  computeSharedFramebufferLayout,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import { AEROGPU_CMD_STREAM_HEADER_SIZE, AEROGPU_CMD_STREAM_MAGIC } from "../../../emulator/protocol/aerogpu/aerogpu_cmd";
import { AEROGPU_ABI_VERSION_U32 } from "../../../emulator/protocol/aerogpu/aerogpu_pci";
import { WORKER_THREADS_WEBWORKER_EXEC_ARGV } from "./test_utils/worker_exec_argv";

const GPU_WORKER_EXEC_ARGV = WORKER_THREADS_WEBWORKER_EXEC_ARGV;

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
    unrefBestEffort(timer);

    const onMessage = (msg: unknown) => {
      // Surface runtime worker errors eagerly.
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const maybeMessage = (maybeProtocol as { message?: unknown }).message;
        const errMsg = typeof maybeMessage === "string" ? maybeMessage : "";
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
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: GPU_WORKER_EXEC_ARGV,
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
        // WorkerInitMessage requires a VGA framebuffer SAB even though this test never uses it.
        // Reuse the shared framebuffer region to keep the init minimal.
        vgaFramebuffer: segments.sharedFramebuffer,
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
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { requestId?: unknown }).requestId === 1 &&
          (msg as { ok?: unknown }).ok === true,
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
          (msg) =>
            (msg as { type?: unknown }).type === "submit_complete" && (msg as { requestId?: unknown }).requestId === 7,
          100,
        ),
      ).rejects.toThrow(/timed out/i);

      // Resume the worker, then ensure the submission completes.
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { requestId?: unknown }).requestId === 2 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );

      const complete = await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "submit_complete" &&
          (msg as { requestId?: unknown }).requestId === 7 &&
          (msg as { protocol?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { protocolVersion?: unknown }).protocolVersion === GPU_PROTOCOL_VERSION,
        5_000,
      );
      expect((complete as { completedFence?: unknown }).completedFence).toBe(1n);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("waits for in-flight tick/present work before acknowledging snapshot pause", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: GPU_WORKER_EXEC_ARGV,
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
        // WorkerInitMessage requires a VGA framebuffer SAB even though this test never uses it.
        // Reuse the shared framebuffer region to keep the init minimal.
        vgaFramebuffer: segments.sharedFramebuffer,
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

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_delay_module.ts", import.meta.url).href;

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

      // Mark a shared framebuffer frame as dirty so the next tick triggers a present pass.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const nextSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, nextSeq);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);

      Atomics.store(frameState, FRAME_SEQ_INDEX, nextSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });
      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_present_started", 10_000);

      // Snapshot pause should wait until the async present() has finished before acknowledging.
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });

      await expect(
        waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { kind?: unknown }).kind === "vm.snapshot.paused" &&
            (msg as { requestId?: unknown }).requestId === 1 &&
            (msg as { ok?: unknown }).ok === true,
          100,
        ),
      ).rejects.toThrow(/timed out/i);

      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_present_finished", 10_000);

      await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { requestId?: unknown }).requestId === 1 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("waits for in-flight screenshot work before acknowledging snapshot pause", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: GPU_WORKER_EXEC_ARGV,
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
        // WorkerInitMessage requires a VGA framebuffer SAB even though this test never uses it.
        // Reuse the shared framebuffer region to keep the init minimal.
        vgaFramebuffer: segments.sharedFramebuffer,
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

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_delay_module.ts", import.meta.url).href;

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

      // Mark a shared framebuffer frame as dirty so the screenshot request forces a tick/present.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const nextSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, nextSeq);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);

      Atomics.store(frameState, FRAME_SEQ_INDEX, nextSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      worker.postMessage({
        protocol: GPU_PROTOCOL_NAME,
        protocolVersion: GPU_PROTOCOL_VERSION,
        type: "screenshot",
        requestId: 42,
      });

      // Ensure the screenshot has triggered an in-flight present() (it will resolve after a delay).
      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_present_started", 10_000);

      // Snapshot pause should wait until the screenshot handler finishes before acknowledging.
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });

      await expect(
        waitForWorkerMessage(
          worker,
          (msg) => {
            const m = msg as { kind?: unknown; requestId?: unknown; ok?: unknown } | null | undefined;
            return m?.kind === "vm.snapshot.paused" && m?.requestId === 1 && m?.ok === true;
          },
          100,
        ),
      ).rejects.toThrow(/timed out/i);

      await waitForWorkerMessage(
        worker,
        (msg) => {
          const m = msg as { type?: unknown; requestId?: unknown } | null | undefined;
          return m?.type === "screenshot" && m?.requestId === 42;
        },
        10_000,
      );

      await waitForWorkerMessage(
        worker,
        (msg) => {
          const m = msg as { kind?: unknown; requestId?: unknown; ok?: unknown } | null | undefined;
          return m?.kind === "vm.snapshot.paused" && m?.requestId === 1 && m?.ok === true;
        },
        5_000,
      );
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("waits for in-flight telemetry polling before acknowledging snapshot pause", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: GPU_WORKER_EXEC_ARGV,
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
        vram: segments.vram,
        vramSizeBytes: segments.vram?.byteLength ?? 0,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
      );

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_telemetry_delay_module.ts", import.meta.url).href;

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

      // Wait for the telemetry hook to start (runs on an interval).
      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_telemetry_started", 10_000);

      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });

      await expect(
        waitForWorkerMessage(
          worker,
          (msg) => {
            const m = msg as { kind?: unknown; requestId?: unknown; ok?: unknown } | null | undefined;
            return m?.kind === "vm.snapshot.paused" && m?.requestId === 1 && m?.ok === true;
          },
          100,
        ),
      ).rejects.toThrow(/timed out/i);

      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_telemetry_finished", 10_000);
      await waitForWorkerMessage(
        worker,
        (msg) => {
          const m = msg as { kind?: unknown; requestId?: unknown; ok?: unknown } | null | undefined;
          return m?.kind === "vm.snapshot.paused" && m?.requestId === 1 && m?.ok === true;
        },
        5_000,
      );
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("does not disable shared-state globals if a resume races with an in-flight pause", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: GPU_WORKER_EXEC_ARGV,
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
        vram: segments.vram,
        vramSizeBytes: segments.vram?.byteLength ?? 0,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
      );

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_globals_delay_module.ts", import.meta.url).href;

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

      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );

      const publishFrame = () => {
        const nextSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
        Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, nextSeq);
        Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);

        Atomics.store(frameState, FRAME_SEQ_INDEX, nextSeq);
        Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
      };

      // Trigger a long-running tick/present.
      publishFrame();
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const firstGlobals = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present_globals",
        10_000,
      )) as { scanoutOk?: boolean; cursorOk?: boolean };
      expect(firstGlobals.scanoutOk).toBe(true);
      expect(firstGlobals.cursorOk).toBe(true);

      // Begin snapshot pause, but immediately send a resume before the in-flight present completes
      // (simulates coordinator timeout + best-effort resume).
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { requestId?: unknown }).requestId === 2 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );

      // Ensure the first present completes (so the paused attempt would have a chance to disable
      // guest/shared-state globals after resume if it were buggy).
      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_present_finished", 10_000);
      await new Promise((resolve) => setTimeout(resolve, 0));

      // Drive another present pass and ensure scanout/cursor globals are still available.
      publishFrame();
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });
      const secondGlobals = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present_globals",
        10_000,
      )) as { scanoutOk?: boolean; cursorOk?: boolean };
      expect(secondGlobals.scanoutOk).toBe(true);
      expect(secondGlobals.cursorOk).toBe(true);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("keeps shared-state globals disabled when init runs after snapshot pause (pause before init)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: createMinimalSharedFramebuffer(),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: GPU_WORKER_EXEC_ARGV,
    } as unknown as WorkerOptions);

    try {
      // Pause the worker before sending its runtime init message.
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { requestId?: unknown }).requestId === 1 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );

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
        vram: segments.vram,
        vramSizeBytes: segments.vram?.byteLength ?? 0,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
      );

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_globals_probe_module.ts", import.meta.url).href;

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

      const probeImport = (await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "mock_presenter_globals_probe" &&
          (msg as { phase?: unknown }).phase === "import",
        10_000,
      )) as { scanoutOk?: boolean; cursorOk?: boolean };
      // Still paused: globals should be disabled.
      expect(probeImport.scanoutOk).toBe(false);
      expect(probeImport.cursorOk).toBe(false);

      // Resume and ensure the globals are restored before present().
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { requestId?: unknown }).requestId === 2 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );

      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const nextSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, nextSeq);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);

      Atomics.store(frameState, FRAME_SEQ_INDEX, nextSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const probePresent = (await waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "mock_presenter_globals_probe" &&
          (msg as { phase?: unknown }).phase === "present",
        10_000,
      )) as { scanoutOk?: boolean; cursorOk?: boolean };
      expect(probePresent.scanoutOk).toBe(true);
      expect(probePresent.cursorOk).toBe(true);
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

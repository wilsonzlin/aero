import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { unrefBestEffort } from "../unrefSafe";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
import { AerogpuCmdWriter, AEROGPU_PRESENT_FLAG_VSYNC } from "../../../emulator/protocol/aerogpu/aerogpu_cmd";
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

function buildVsyncPresentCmdStream(): ArrayBuffer {
  const writer = new AerogpuCmdWriter();
  writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
  return writer.finish().slice().buffer;
}

describe("workers/gpu-worker vsync submit completion when WDDM scanout is disabled", () => {
  it("does not block submit_complete on tick when ScanoutState publishes the WDDM disabled descriptor", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Disabled WDDM descriptor (base/width/height/pitch=0). In this state the runtime stops
    // vblank/tick pacing; vsync-paced completions must therefore be treated as immediate.
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: SCANOUT_FORMAT_B8G8R8X8,
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
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };
      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        20_000,
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
        20_000,
      );

      const cmdStream = buildVsyncPresentCmdStream();

      const requestId = 1;
      const completePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "submit_complete" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        5_000,
      );
      worker.postMessage(
        {
          protocol: GPU_PROTOCOL_NAME,
          protocolVersion: GPU_PROTOCOL_VERSION,
          type: "submit_aerogpu",
          requestId,
          contextId: 0,
          signalFence: 1n,
          cmdStream,
        },
        [cmdStream],
      );

      // If this were treated as a real vsync-paced submission, it would stall until a `tick`
      // message arrives. In the disabled scanout state, it must complete without any ticks.
      const complete = (await completePromise) as { completedFence?: unknown };
      expect(complete.completedFence).toBe(1n);
    } finally {
      await worker.terminate();
    }
  }, 60_000);

  it("does not block submit_complete on tick for the WDDM placeholder descriptor (base_paddr=0 with non-zero geometry)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Placeholder descriptor (host-side AeroGPU path): base_paddr=0 but geometry is non-zero.
    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8X8,
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
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };
      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        20_000,
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
        20_000,
      );

      const cmdStream = buildVsyncPresentCmdStream();

      const requestId = 2;
      const completePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "submit_complete" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        5_000,
      );
      worker.postMessage(
        {
          protocol: GPU_PROTOCOL_NAME,
          protocolVersion: GPU_PROTOCOL_VERSION,
          type: "submit_aerogpu",
          requestId,
          contextId: 0,
          signalFence: 2n,
          cmdStream,
        },
        [cmdStream],
      );

      const complete = (await completePromise) as { completedFence?: unknown };
      expect(complete.completedFence).toBe(2n);
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

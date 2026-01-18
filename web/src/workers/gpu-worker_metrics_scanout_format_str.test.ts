import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { unrefBestEffort } from "../unrefSafe";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import {
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  isGpuWorkerMessageBase,
  type GpuRuntimeMetricsMessage,
  type GpuRuntimeStatsMessage,
} from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM, wrapScanoutState } from "../ipc/scanout_state.ts";
import { aerogpuFormatToString } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";
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
        const rawMsg = (maybeProtocol as { message?: unknown }).message;
        const errMsg = typeof rawMsg === "string" ? rawMsg : "";
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

describe("workers/gpu-worker metrics scanout snapshot", () => {
  it("includes scanout.format_str in metrics/stats messages", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
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
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
      };

      // Control-plane init (sets up rings + status).
      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        20_000,
      );

      // Runtime init (headless).
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
        options: {},
      });

      await waitForWorkerMessage(
        worker,
        (msg) => isGpuWorkerMessageBase(msg) && (msg as { type?: unknown }).type === "ready",
        20_000,
      );

      // Publish a deterministic scanout descriptor so the metrics snapshot includes a known format.
      const scanoutWords = wrapScanoutState(segments.scanoutState!, segments.scanoutStateOffsetBytes ?? 0);
      publishScanoutState(scanoutWords, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: 0x1000,
        basePaddrHi: 0,
        width: 64,
        height: 64,
        pitchBytes: 64 * 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

      // Metrics are rate-limited (250ms). Wait long enough so `performance.now()` inside the worker
      // crosses the interval, then tick once and assert the emitted metrics include scanout telemetry.
      await new Promise((resolve) => setTimeout(resolve, 300));
      const metricsPromise = waitForWorkerMessage(
        worker,
        (msg) => {
          if (!isGpuWorkerMessageBase(msg)) return false;
          const m = msg as { type?: unknown; scanout?: unknown };
          return m.type === "metrics" && !!m.scanout;
        },
        20_000,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const metricsMsg = (await metricsPromise) as GpuRuntimeMetricsMessage;
      expect(metricsMsg.scanout).toBeTruthy();
      expect(metricsMsg.scanout?.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
      expect(metricsMsg.scanout?.format_str).toBe(aerogpuFormatToString(SCANOUT_FORMAT_B8G8R8X8));

      // Stats messages are emitted by the telemetry poller and should include the same scanout snapshot.
      const statsMsg = (await waitForWorkerMessage(
        worker,
        (msg) => {
          if (!isGpuWorkerMessageBase(msg)) return false;
          const m = msg as { type?: unknown; scanout?: unknown };
          return m.type === "stats" && !!m.scanout;
        },
        20_000,
      )) as GpuRuntimeStatsMessage;
      expect(statsMsg.scanout).toBeTruthy();
      expect(statsMsg.scanout?.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
      expect(statsMsg.scanout?.format_str).toBe(aerogpuFormatToString(SCANOUT_FORMAT_B8G8R8X8));
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

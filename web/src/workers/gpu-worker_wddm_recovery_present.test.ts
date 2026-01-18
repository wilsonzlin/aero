import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { unrefBestEffort } from "../unrefSafe";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import {
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  type GpuRuntimeStatsMessage,
} from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8A8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
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

describe("workers/gpu-worker WDDM scanout recovery", () => {
  it("keeps presenting after device recovery when scanout is WDDM-owned and the legacy framebuffer is idle", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Publish a valid WDDM scanout descriptor pointing at a known BGRA pixel in guest RAM.
    const basePaddr = 0x1000;
    views.guestU8.fill(0);
    // BGRA bytes -> RGBA [11 22 33 44] after swizzle (alpha preserved).
    views.guestU8.set([0x33, 0x22, 0x11, 0x44], basePaddr);

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_B8G8R8A8,
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

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_device_lost_module.ts", import.meta.url).href;

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

      // Wait until the mock module is imported (so `presentFn` is installed).
      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_presenter_loaded", 20_000);

      // Tick #1: should still attempt a present even though FRAME_STATUS=PRESENTED, because scanout is WDDM.
      // The mock presenter throws webgpu_device_lost on the first call to trigger recovery.
      const firstPresentCall = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present_call" && (msg as { callCount?: unknown }).callCount === 1,
        10_000,
      );

      const readyAfterRecovery = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
        20_000,
      );
      const deviceLostEvents = waitForWorkerMessage(
        worker,
        (msg) => {
           const m = msg as { protocol?: unknown; type?: unknown; events?: unknown[] } | undefined;
           if (m?.protocol !== GPU_PROTOCOL_NAME || m.type !== "events") return false;
           const events = Array.isArray(m.events) ? m.events : [];
           return events.some((ev) => {
             const e = ev as { category?: unknown; details?: unknown } | null | undefined;
             if (e?.category !== "DeviceLost") return false;
             const details = e.details;
             if (!details || typeof details !== "object") return false;
             const scanout = (details as { scanout?: unknown }).scanout;
             if (!scanout || typeof scanout !== "object") return false;
             const formatStr = (scanout as { format_str?: unknown }).format_str;
             return typeof formatStr === "string";
           });
         },
         10_000,
       );

      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

       await firstPresentCall;
       await readyAfterRecovery;
       const eventsMsg = (await deviceLostEvents) as { events?: unknown[] };
       const deviceLost = (eventsMsg.events ?? []).find(
         (ev) => (ev as { category?: unknown } | null | undefined)?.category === "DeviceLost",
       ) as { details?: unknown } | undefined;
       expect(deviceLost).toBeTruthy();
       if (!deviceLost) throw new Error("expected DeviceLost event");
       expect(deviceLost.details).toMatchObject({
         scanout: {
           format: SCANOUT_FORMAT_B8G8R8A8,
           format_str: aerogpuFormatToString(SCANOUT_FORMAT_B8G8R8A8),
         },
      });

      // Tick #2: should present successfully without requiring a legacy framebuffer DIRTY event.
      const secondPresent = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === true,
        10_000,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });
      await secondPresent;

      // Confirm recovery counters were attributed to WDDM scanout and the worker is still reporting
      // `outputSource=wddm_scanout` (i.e. it did not require a shared framebuffer DIRTY to resume).
      const stats = (await waitForWorkerMessage(
        worker,
        (msg) => {
          const m = msg as Partial<GpuRuntimeStatsMessage> | undefined;
          return (
            m?.protocol === GPU_PROTOCOL_NAME &&
            m?.type === "stats" &&
            m.outputSource === "wddm_scanout" &&
            m.counters?.recoveries_succeeded === 1
          );
        },
        10_000,
      )) as GpuRuntimeStatsMessage;

      expect(stats.counters.recoveries_attempted).toBe(1);
      expect(stats.counters.recoveries_succeeded).toBe(1);
      expect(stats.counters.recoveries_attempted_wddm).toBe(1);
      expect(stats.counters.recoveries_succeeded_wddm).toBe(1);

      // Screenshot should reflect the WDDM scanout pixel bytes.
      const requestId = 1;
      const shotPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { protocol?: unknown; type?: unknown; requestId?: unknown }).protocol === GPU_PROTOCOL_NAME &&
          (msg as { type?: unknown }).type === "screenshot" &&
          (msg as { requestId?: unknown }).requestId === requestId,
        10_000,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "screenshot", requestId });
      const shot = (await shotPromise) as { width: number; height: number; rgba8: ArrayBuffer };
      expect(shot.width).toBe(1);
      expect(shot.height).toBe(1);
      const px = new Uint8Array(shot.rgba8);
      const firstPixel =
        px.byteLength >= 4
          ? (((px[0] ?? 0) | ((px[1] ?? 0) << 8) | ((px[2] ?? 0) << 16) | ((px[3] ?? 0) << 24)) >>> 0)
          : 0;
      // WDDM scanout readback preserves alpha for BGRA formats.
      expect(firstPixel).toBe(0x44332211);
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

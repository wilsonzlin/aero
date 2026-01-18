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
} from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
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

describe("workers/gpu-worker webgpu_uncaptured_error handling", () => {
  it("emits a structured Validation event but does not send a fatal worker ERROR (no restart)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Ensure the frame scheduler tick path runs a present pass even if the legacy shared framebuffer
    // is idle, by setting scanout source to WDDM (with a placeholder base_paddr=0).
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

    const observedMessages: unknown[] = [];
    worker.on("message", (msg) => observedMessages.push(msg));

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
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        20_000,
      );

      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_uncaptured_error_module.ts", import.meta.url).href;

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

      const firstPresentCall = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present_call" && (msg as { callCount?: unknown }).callCount === 1,
        10_000,
      );

      const eventsPromise = waitForWorkerMessage(
        worker,
        (msg) => {
          const m = msg as { protocol?: unknown; type?: unknown; events?: unknown[] } | undefined;
          if (m?.protocol !== GPU_PROTOCOL_NAME || m.type !== "events") return false;
          const events = Array.isArray(m.events) ? m.events : [];
          return events.some(
            (ev) =>
              (ev as { category?: unknown; message?: unknown } | null | undefined)?.category === "Validation" &&
              String((ev as { message?: unknown }).message).includes("simulated uncaptured error"),
          );
        },
        10_000,
      );

      // Tick #1: should attempt a present even though FRAME_STATUS=PRESENTED, because scanout is WDDM.
      // The mock presenter throws webgpu_uncaptured_error on the first call, which should only
      // produce an `events` message (not a fatal worker ERROR).
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      await firstPresentCall;
      const eventsMsg = (await eventsPromise) as { events?: unknown[] };
      const validationEvent = (eventsMsg.events ?? []).find(
        (ev) => (ev as { category?: unknown } | null | undefined)?.category === "Validation",
      ) as { message?: unknown } | undefined;
      if (!validationEvent) throw new Error("expected Validation event");
      expect(String(validationEvent.message)).toContain("simulated uncaptured error");

      // Tick #2: mock presenter should continue presenting successfully after the uncaptured error.
      const secondPresent = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === true,
        10_000,
      );
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });
      await secondPresent;

      // Ensure the GPU worker did not forward the uncaptured error as a legacy `type:"error"` message.
      expect(
        observedMessages.some(
          (msg) => {
            const m = msg as { protocol?: unknown; type?: unknown } | null | undefined;
            return m?.protocol === GPU_PROTOCOL_NAME && m?.type === "error";
          },
        ),
      ).toBe(false);
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

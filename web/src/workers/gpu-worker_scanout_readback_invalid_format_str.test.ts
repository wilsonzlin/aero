import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { publishScanoutState, SCANOUT_SOURCE_WDDM } from "../ipc/scanout_state";
import { aerogpuFormatToString, type AerogpuFormat } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";

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

describe("workers/gpu-worker scanout readback invalid diagnostics", () => {
  it("includes scanout.format_str in ScanoutReadback events for unsupported formats", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const basePaddr = 0x1000;
    const unsupportedFormat = 123456;

    publishScanoutState(views.scanoutStateI32!, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: basePaddr >>> 0,
      basePaddrHi: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: unsupportedFormat as unknown as AerogpuFormat,
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

      // Control-plane init (sets up rings + status).
      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
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
        (msg) => (msg as { protocol?: unknown; type?: unknown }).protocol === GPU_PROTOCOL_NAME && (msg as { type?: unknown }).type === "ready",
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
              (ev as { category?: unknown; message?: unknown } | null | undefined)?.category === "ScanoutReadback" &&
              String((ev as { message?: unknown }).message).includes("unsupported format"),
           );
         },
         10_000,
       );

      // Drive a tick so the worker attempts scanout readback and emits the diagnostic.
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

       const eventsMsg = (await eventsPromise) as { events?: unknown[] };
       const scanoutEvent = (eventsMsg.events ?? []).find(
         (ev) => (ev as { category?: unknown } | null | undefined)?.category === "ScanoutReadback",
       ) as { severity?: unknown; message?: unknown; details?: unknown } | undefined;
       expect(scanoutEvent).toBeTruthy();
       if (!scanoutEvent) throw new Error("expected ScanoutReadback event");
       expect(scanoutEvent.severity).toBe("warn");
       expect(String(scanoutEvent.message)).toContain("unsupported format");
       expect(scanoutEvent.details).toMatchObject({
         scanout: {
          format: unsupportedFormat,
          format_str: aerogpuFormatToString(unsupportedFormat),
        },
      });
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

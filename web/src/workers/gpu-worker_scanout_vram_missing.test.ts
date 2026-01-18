import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { unrefBestEffort } from "../unrefSafe";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION, isGpuWorkerMessageBase } from "../ipc/gpu-protocol";
import {
  publishScanoutState,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_WDDM,
  wrapScanoutState,
} from "../ipc/scanout_state.ts";
import { VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
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

describe("workers/gpu-worker scanout VRAM missing diagnostics", () => {
  it("emits a structured Scanout event and a coordinator ERROR when WDDM scanout points into VRAM without a shared VRAM SAB", async () => {
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

    const vramBasePaddr = VRAM_BASE_PADDR >>> 0;
    const vramSizeBytes = 0x2000;
    const scanoutBasePaddr = (vramBasePaddr + 0x1000) >>> 0;
    const expectedSnippet = "WDDM scanout points into the VRAM aperture";

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
        // Intentionally omit `vram` but still advertise a VRAM aperture.
        vramBasePaddr,
        vramSizeBytes,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        20_000,
      );

      // GPU-protocol init (headless, no canvas).
      const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
      worker.postMessage({
        protocol: GPU_PROTOCOL_NAME,
        protocolVersion: GPU_PROTOCOL_VERSION,
        type: "init",
        sharedFrameState,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        options: {},
      });

      // Publish a WDDM scanout descriptor that points into the VRAM aperture.
      const scanoutWords = wrapScanoutState(segments.scanoutState!, segments.scanoutStateOffsetBytes);
      publishScanoutState(scanoutWords, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: scanoutBasePaddr,
        basePaddrHi: 0,
        width: 64,
        height: 64,
        pitchBytes: 64 * 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

      const eventsPromise = waitForWorkerMessage(worker, (msg) => {
        if (!isGpuWorkerMessageBase(msg)) return false;
        const m = msg as { type?: unknown; events?: unknown } | undefined;
        if (m?.type !== "events") return false;
        const events = Array.isArray(m.events) ? m.events : [];
        return events.some(
          (ev) =>
            (ev as { category?: unknown; message?: unknown } | null | undefined)?.category === "Scanout" &&
            String((ev as { message?: unknown }).message).includes(expectedSnippet),
        );
      }, 20_000);

      const errorPromise = waitForWorkerMessage(worker, (msg) => {
        const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
        if (maybeProtocol?.type !== MessageType.ERROR) return false;
        const rawMsg = (maybeProtocol as { message?: unknown }).message;
        return typeof rawMsg === "string" && rawMsg.includes(expectedSnippet);
      }, 20_000);

      // Drive a tick to trigger `presentOnce()` which performs the VRAM-missing scanout guard.
      worker.postMessage({
        protocol: GPU_PROTOCOL_NAME,
        protocolVersion: GPU_PROTOCOL_VERSION,
        type: "tick",
        frameTimeMs: 0,
      });

      const [eventsMsgRaw, errorMsgRaw] = await Promise.all([eventsPromise, errorPromise]);

      const eventsMsg = eventsMsgRaw as { events?: unknown[] };
      const scanoutEvent = (eventsMsg.events ?? []).find(
        (ev) => (ev as { category?: unknown } | null | undefined)?.category === "Scanout",
      ) as { message?: unknown; severity?: unknown; details?: unknown } | undefined;
      expect(scanoutEvent).toBeTruthy();
      if (!scanoutEvent) throw new Error("expected Scanout event");
      expect(String(scanoutEvent.message)).toContain(expectedSnippet);
      expect(scanoutEvent.severity).toBe("error");
      expect(scanoutEvent.details).toMatchObject({
        vram_base_paddr: `0x${vramBasePaddr.toString(16)}`,
        vram_size_bytes: vramSizeBytes,
        scanout: {
          format: SCANOUT_FORMAT_B8G8R8X8,
          format_str: aerogpuFormatToString(SCANOUT_FORMAT_B8G8R8X8),
        },
      });

      const errorMsg = errorMsgRaw as ProtocolMessage & { message?: string };
      expect(errorMsg.type).toBe(MessageType.ERROR);
      expect(errorMsg.role).toBe("gpu");
      expect(errorMsg.message).toContain(expectedSnippet);
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

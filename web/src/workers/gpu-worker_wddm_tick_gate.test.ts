import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateSharedMemorySegments, createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import {
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
} from "../ipc/gpu-protocol";
import { SharedFramebufferHeaderIndex, SHARED_FRAMEBUFFER_HEADER_U32_LEN } from "../ipc/shared-layout";
import {
  publishScanoutState,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_SOURCE_WDDM,
  SCANOUT_STATE_U32_LEN,
} from "../ipc/scanout_state";

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

describe("workers/gpu-worker WDDM tick gating", () => {
  it("clears legacy shared framebuffer dirty on tick when scanout is WDDM-owned even if FRAME_STATUS is PRESENTED", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 2, vramMiB: 0 });
    const views = createSharedMemoryViews(segments);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./gpu-worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Runtime init (SharedArrayBuffers + guest RAM + scanout state).
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
        10_000,
      );

      // GPU-protocol init (frame pacing state + shared framebuffer handle).
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
        // No canvas: headless mode is sufficient for validating tick gating + dirty clearing.
      });

      const fbHeader = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );

      const scanoutWords = new Int32Array(segments.scanoutState!, segments.scanoutStateOffsetBytes ?? 0, SCANOUT_STATE_U32_LEN);

      // 1) Legacy scanout: tick should be ignored while FRAME_STATUS is PRESENTED.
      publishScanoutState(scanoutWords, {
        source: SCANOUT_SOURCE_LEGACY_TEXT,
        basePaddrLo: 0,
        basePaddrHi: 0,
        width: 0,
        height: 0,
        pitchBytes: 0,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

      Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });
      expect(Atomics.wait(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1, 250)).toBe("timed-out");
      expect(Atomics.load(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY)).toBe(1);

      // 2) WDDM scanout: tick must run a present pass even though FRAME_STATUS stayed PRESENTED,
      // clearing the legacy shared framebuffer dirty flag (prevents "flash back" and producer stalls).
      const basePaddr = 0x1000;
      views.guestU8.set([0, 0, 255, 0], basePaddr); // BGRX = red, X byte 0.
      publishScanoutState(scanoutWords, {
        source: SCANOUT_SOURCE_WDDM,
        basePaddrLo: basePaddr >>> 0,
        basePaddrHi: 0,
        width: 1,
        height: 1,
        pitchBytes: 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

      Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 1 });
      const waitResult = Atomics.wait(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1, 5_000);
      expect(waitResult === "ok" || waitResult === "not-equal").toBe(true);
      expect(Atomics.load(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY)).toBe(0);
      expect(Atomics.load(frameState, FRAME_STATUS_INDEX)).toBe(FRAME_PRESENTED);
    } finally {
      await worker.terminate();
    }
  }, 30_000);
});


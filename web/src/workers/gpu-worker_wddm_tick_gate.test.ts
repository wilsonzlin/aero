import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import {
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
} from "../ipc/gpu-protocol";
import {
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  computeSharedFramebufferLayout,
} from "../ipc/shared-layout";
import {
  publishScanoutState,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_SOURCE_LEGACY_VBE_LFB,
  SCANOUT_SOURCE_WDDM,
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

describe("workers/gpu-worker WDDM tick gating", () => {
  it("clears legacy shared framebuffer dirty on tick when scanout is ScanoutState-owned even if FRAME_STATUS is PRESENTED", async () => {
    const fbLayout = computeSharedFramebufferLayout(1, 1, 4, FramebufferFormat.RGBA8, 0);
    const sharedFramebuffer = new SharedArrayBuffer(fbLayout.totalBytes);
    const fbHeader = new Int32Array(sharedFramebuffer, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.WIDTH, fbLayout.width);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.HEIGHT, fbLayout.height);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.STRIDE_BYTES, fbLayout.strideBytes);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FORMAT, fbLayout.format);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILE_SIZE, fbLayout.tileSize);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILES_X, fbLayout.tilesX);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILES_Y, fbLayout.tilesY);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, fbLayout.dirtyWordsPerBuffer);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FLAGS, 0);

    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
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
        20_000,
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

      const scanoutWords = views.scanoutStateI32!;

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

      // The worker may clear the legacy shared framebuffer dirty flag before it flips the
      // frame pacing status back to PRESENTED (tick path is async). Wait until the status
      // transitions away from PRESENTING so the assertion is not racy across Node versions.
      const statusWait = Atomics.wait(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTING, 5_000);
      expect(statusWait === "ok" || statusWait === "not-equal").toBe(true);
      expect(Atomics.load(frameState, FRAME_STATUS_INDEX)).toBe(FRAME_PRESENTED);

      // 3) Legacy VBE LFB scanout: tick must also run a present pass even though FRAME_STATUS stayed
      // PRESENTED, clearing the legacy shared framebuffer dirty flag. This mirrors the behavior used
      // by VBE mode while booting (ScanoutState owns output).
      const vbeBasePaddr = 0x2000;
      views.guestU8.set([0, 255, 0, 0], vbeBasePaddr); // BGRX = green, X byte 0.
      publishScanoutState(scanoutWords, {
        source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
        basePaddrLo: vbeBasePaddr >>> 0,
        basePaddrHi: 0,
        width: 1,
        height: 1,
        pitchBytes: 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });

      Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 2 });
      const vbeWait = Atomics.wait(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1, 5_000);
      expect(vbeWait === "ok" || vbeWait === "not-equal").toBe(true);
      expect(Atomics.load(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY)).toBe(0);

      const vbeStatusWait = Atomics.wait(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTING, 5_000);
      expect(vbeStatusWait === "ok" || vbeStatusWait === "not-equal").toBe(true);
      expect(Atomics.load(frameState, FRAME_STATUS_INDEX)).toBe(FRAME_PRESENTED);
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

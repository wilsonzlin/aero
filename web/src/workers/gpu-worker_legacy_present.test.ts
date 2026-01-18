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
  type GpuRuntimeStatsMessage,
} from "../ipc/gpu-protocol";
import { SCANOUT_SOURCE_WDDM, SCANOUT_STATE_GENERATION_BUSY_BIT, ScanoutStateIndex, wrapScanoutState } from "../ipc/scanout_state";
import { CURSOR_STATE_GENERATION_BUSY_BIT, CursorStateIndex, wrapCursorState } from "../ipc/cursor_state";
import {
  FramebufferFormat,
  layoutFromHeader,
  computeSharedFramebufferLayout,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
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

async function waitForAtomicValue(
  arr: Int32Array,
  index: number,
  expected: number,
  timeoutMs: number,
): Promise<void> {
  const start = Date.now();
  // Poll rather than using `Atomics.wait()` so we don't block vitest's event loop.
  while (Date.now() - start < timeoutMs) {
    const value = Atomics.load(arr, index);
    if (value === expected) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  const last = Atomics.load(arr, index);
  throw new Error(
    `timed out after ${timeoutMs}ms waiting for Atomics[${index}] to become ${expected} (last=${last})`,
  );
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

function allocateTestSegments() {
  const sharedFramebuffer = createMinimalSharedFramebuffer();
  return allocateHarnessSharedMemorySegments({
    guestRamBytes: 64 * 1024,
    sharedFramebuffer,
    sharedFramebufferOffsetBytes: 0,
    ioIpcBytes: 0,
    vramBytes: 0,
  });
}

describe("workers/gpu-worker legacy framebuffer plumbing", () => {
  it("presents from sharedFramebuffer via a mock presenter module (vgaFramebuffer aliases sharedFramebuffer)", async () => {
    const segments = allocateTestSegments();

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

      // Load mock presenter module (installs present()).
      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_module.ts", import.meta.url).href;

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

      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "mock_presenter_loaded", 20_000);

      // Publish a single shared-layout frame with a distinctive first pixel so we can prove which
      // buffer the worker consumed.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const layout = layoutFromHeader(header);

      const slot0 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[0],
        layout.strideBytes * layout.height,
      );
      const slot1 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[1],
        layout.strideBytes * layout.height,
      );

      const dirty0 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[0],
              layout.dirtyWordsPerBuffer,
            );
      const dirty1 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[1],
              layout.dirtyWordsPerBuffer,
            );

      const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
      const back = active ^ 1;
      const backPixels = back === 0 ? slot0 : slot1;
      const backDirty = back === 0 ? dirty0 : dirty1;

      // First pixel = 0x44332211 (little-endian bytes 11 22 33 44).
      backPixels.fill(0);
      backPixels[0] = 0x11;
      backPixels[1] = 0x22;
      backPixels[2] = 0x33;
      backPixels[3] = 0x44;
      if (backDirty) backDirty.fill(0xffffffff);

      const newSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
      Atomics.store(
        header,
        back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
        newSeq,
      );
      Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

      Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      // Drive a tick to force present().
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const presentMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === true,
        10_000,
      )) as { firstPixel?: number; seq?: number };

      expect(presentMsg.firstPixel).toBe(0x44332211);
      expect(presentMsg.seq).toBe(newSeq >>> 0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("counts dropped presents when the presenter module returns false", async () => {
    const segments = allocateTestSegments();

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
        10_000,
      );

      // Load mock presenter module that drops frames.
      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_drop_module.ts", import.meta.url).href;

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

      // Publish a single shared-layout frame with a distinctive first pixel.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const layout = layoutFromHeader(header);

      const slot0 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[0],
        layout.strideBytes * layout.height,
      );
      const slot1 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[1],
        layout.strideBytes * layout.height,
      );

      const dirty0 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[0],
              layout.dirtyWordsPerBuffer,
            );
      const dirty1 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[1],
              layout.dirtyWordsPerBuffer,
            );

      const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
      const back = active ^ 1;
      const backPixels = back === 0 ? slot0 : slot1;
      const backDirty = back === 0 ? dirty0 : dirty1;

      // First pixel = 0x44332211 (little-endian bytes 11 22 33 44).
      backPixels.fill(0);
      backPixels[0] = 0x11;
      backPixels[1] = 0x22;
      backPixels[2] = 0x33;
      backPixels[3] = 0x44;
      if (backDirty) backDirty.fill(0xffffffff);

      const newSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
      Atomics.store(
        header,
        back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
        newSeq,
      );
      Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

      Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      // Drive a tick to force present().
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const presentMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === false,
        10_000,
      )) as { firstPixel?: number; seq?: number };

      expect(presentMsg.firstPixel).toBe(0x44332211);
      expect(presentMsg.seq).toBe(newSeq >>> 0);

      // `mock_present` is posted from inside the present() function, which can race with the worker's
      // subsequent dirty-flag + pacing updates. Wait for those to settle before asserting.
      await waitForAtomicValue(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0, 5_000);
      await waitForAtomicValue(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED, 5_000);

      // Metrics are rate-limited; wait long enough for a metrics post, then tick again so the
      // shared counters are synchronized.
      await new Promise((resolve) => setTimeout(resolve, 300));
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const metricsMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "metrics",
        10_000,
      )) as { framesPresented?: number; framesDropped?: number };

      expect(metricsMsg.framesPresented).toBe(0);
      expect(metricsMsg.framesDropped).toBe(1);

      // Stats are posted periodically by the telemetry poller; ensure present outcome is
      // reflected in the worker counters too (presents_attempted increments, presents_succeeded does not).
      const statsMsg = (await waitForWorkerMessage(
        worker,
        (msg) => {
          const m = msg as Partial<GpuRuntimeStatsMessage> | undefined;
          return (
            m?.protocol === GPU_PROTOCOL_NAME &&
            m?.type === "stats" &&
            typeof m.counters?.presents_attempted === "number" &&
            m.counters.presents_attempted >= 1
          );
        },
        10_000,
      )) as GpuRuntimeStatsMessage;

      expect(statsMsg.counters.presents_attempted).toBe(1);
      expect(statsMsg.counters.presents_succeeded).toBe(0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("does not hang if scanout/cursor seqlock busy bit is stuck", async () => {
    const segments = allocateTestSegments();

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
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      };

      // Control-plane init (sets up rings + status).
      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "gpu",
        10_000,
      );

      // Load mock presenter module (installs present()).
      const wasmModuleUrl = new URL("./test_workers/gpu_mock_presenter_module.ts", import.meta.url).href;

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

      // Force scanout/cursor generation busy bits and never clear them.
      const scanoutWords = wrapScanoutState(segments.scanoutState!, segments.scanoutStateOffsetBytes ?? 0);
      Atomics.store(scanoutWords, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_WDDM | 0);
      // Mark this as the WDDM "placeholder" descriptor (base_paddr=0 but non-zero geometry) so the
      // worker is allowed to fall back to the legacy shared framebuffer while the scanout seqlock is
      // stuck (busy bit never clears).
      Atomics.store(scanoutWords, ScanoutStateIndex.WIDTH, 1);
      Atomics.store(scanoutWords, ScanoutStateIndex.HEIGHT, 1);
      Atomics.store(scanoutWords, ScanoutStateIndex.PITCH_BYTES, 4);
      Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, (SCANOUT_STATE_GENERATION_BUSY_BIT | 1) | 0);

      const cursorWords = wrapCursorState(segments.cursorState!, segments.cursorStateOffsetBytes ?? 0);
      Atomics.store(cursorWords, CursorStateIndex.GENERATION, (CURSOR_STATE_GENERATION_BUSY_BIT | 1) | 0);

      // Publish a shared-layout frame with a distinctive first pixel so we can prove which
      // buffer the worker consumed.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const layout = layoutFromHeader(header);

      const slot0 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[0],
        layout.strideBytes * layout.height,
      );
      const slot1 = new Uint8Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes + layout.framebufferOffsets[1],
        layout.strideBytes * layout.height,
      );

      const dirty0 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[0],
              layout.dirtyWordsPerBuffer,
            );
      const dirty1 =
        layout.dirtyWordsPerBuffer === 0
          ? null
          : new Uint32Array(
              segments.sharedFramebuffer,
              segments.sharedFramebufferOffsetBytes + layout.dirtyOffsets[1],
              layout.dirtyWordsPerBuffer,
            );

      const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
      const back = active ^ 1;
      const backPixels = back === 0 ? slot0 : slot1;
      const backDirty = back === 0 ? dirty0 : dirty1;

      // First pixel = 0x44332211 (little-endian bytes 11 22 33 44).
      backPixels.fill(0);
      backPixels[0] = 0x11;
      backPixels[1] = 0x22;
      backPixels[2] = 0x33;
      backPixels[3] = 0x44;
      if (backDirty) backDirty.fill(0xffffffff);

      const newSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
      Atomics.store(
        header,
        back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
        newSeq,
      );
      Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
      Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);

      Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

      // Drive a tick to force present(). This would hang previously because the worker tried to
      // snapshot scanout/cursor state in unbounded `while(true)` loops.
      worker.postMessage({ protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION, type: "tick", frameTimeMs: 0 });

      const presentMsg = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "mock_present" && (msg as { ok?: unknown }).ok === true,
        5_000,
      )) as { firstPixel?: number; seq?: number };

      expect(presentMsg.firstPixel).toBe(0x44332211);
      expect(presentMsg.seq).toBe(newSeq >>> 0);
    } finally {
      await worker.terminate();
    }
  }, 60_000);
});

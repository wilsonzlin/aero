import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { InputEventType } from "../input/event_queue";
import { encodeCommand } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import {
  STATUS_INTS,
  STATUS_OFFSET_BYTES,
  StatusIndex,
  ringRegionsForWorker,
  type SharedMemorySegments,
} from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { emptySetBootDisksMessage, type SetBootDisksMessage } from "../runtime/boot_disks_protocol";

async function waitForWorkerMessage(worker: Worker, predicate: (msg: unknown) => boolean, timeoutMs: number): Promise<unknown> {
  return new Promise((resolve, reject) => {
    // Worker thread startup + module evaluation can be slow under heavy CI load.
    // Add slack to keep these integration tests from flaking when the suite is
    // running in parallel.
    const effectiveTimeoutMs = timeoutMs * 2;
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${effectiveTimeoutMs}ms waiting for worker message`));
    }, effectiveTimeoutMs);
    (timer as unknown as { unref?: () => void }).unref?.();

    const onMessage = (msg: unknown) => {
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const maybeMessage = (maybeProtocol as { message?: unknown }).message;
        const errMsg = typeof maybeMessage === "string" ? maybeMessage : "";
        reject(new Error(`worker reported error${errMsg ? `: ${errMsg}` : ""}`));
        return;
      }
      let matched = false;
      try {
        matched = predicate(msg);
      } catch (err) {
        cleanup();
        reject(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      if (!matched) return;
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

function makeConfig(extra: Partial<AeroConfig> = {}): AeroConfig {
  return {
    ...extra,
    guestMemoryMiB: extra.guestMemoryMiB ?? 1,
    vramMiB: extra.vramMiB ?? 16,
    enableWorkers: extra.enableWorkers ?? true,
    enableWebGPU: extra.enableWebGPU ?? false,
    activeDiskImage: extra.activeDiskImage ?? null,
    logLevel: extra.logLevel ?? "info",
    proxyUrl: extra.proxyUrl ?? null,
  };
}

function makeInit(segments: SharedMemorySegments): WorkerInitMessage {
  return {
    kind: "init",
    role: "cpu",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    vram: segments.vram,
    vramBasePaddr: segments.vram ? VRAM_BASE_PADDR : undefined,
    vramSizeBytes: segments.vram ? segments.vram.byteLength : undefined,
    vgaFramebuffer: segments.sharedFramebuffer,
    scanoutState: segments.scanoutState,
    scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    cursorState: segments.cursorState,
    cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
    // Mirror the coordinator's shared-memory preference: shared guest RAM implies threaded build.
    wasmVariant: "threaded",
  };
}

function allocateTestSegments(): SharedMemorySegments {
  return allocateHarnessSharedMemorySegments({
    guestRamBytes: 64 * 1024,
    sharedFramebuffer: new SharedArrayBuffer(8),
    sharedFramebufferOffsetBytes: 0,
    ioIpcBytes: 0,
    vramBytes: 0,
  });
}

describe("workers/machine_cpu.worker (worker_threads)", () => {
  it("boots via config.update + init and reaches READY", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(emptySetBootDisksMessage());
      worker.postMessage(makeInit(segments));

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("does not spin the halted run loop after a wakeRunLoop clears the wake promise (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);
    const runSliceCounterIndex = 63;
    Atomics.store(status, runSliceCounterIndex, 0);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const regions = ringRegionsForWorker("cpu");
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      if (!commandRing.tryPush(encodeCommand({ kind: "nop", seq: 1 }))) {
        throw new Error("Failed to push nop command into command ring.");
      }

      let baseline = 0;
      const deadline = Date.now() + 1000;
      while (baseline === 0 && Date.now() < deadline) {
        await new Promise((resolve) => {
          const timer = setTimeout(resolve, 20);
          (timer as unknown as { unref?: () => void }).unref?.();
        });
        baseline = Atomics.load(status, runSliceCounterIndex) >>> 0;
      }
      expect(baseline).toBeGreaterThan(0);

      const ack2 = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; version?: unknown }).kind === "config.ack" &&
          (msg as { kind?: unknown; version?: unknown }).version === 2,
        10_000,
      );
      worker.postMessage({
        kind: "config.update",
        version: 2,
        config: makeConfig(),
      });
      await ack2;

      const afterWake = Atomics.load(status, runSliceCounterIndex) >>> 0;
      await new Promise((resolve) => {
        const timer = setTimeout(resolve, 500);
        (timer as unknown as { unref?: () => void }).unref?.();
      });
      const end = Atomics.load(status, runSliceCounterIndex) >>> 0;
      const diff = (end - afterWake) >>> 0;

      expect(diff).toBeLessThan(2_000);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("initializes input diagnostics status fields on init (clears stale values)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    // Simulate a previous runtime writing these fields into shared status memory.
    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, 1); // usb
    Atomics.store(status, StatusIndex.IoInputMouseBackend, 2); // virtio
    Atomics.store(status, StatusIndex.IoInputVirtioKeyboardDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputVirtioMouseDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbKeyboardOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbMouseOk, 1);
    Atomics.store(status, StatusIndex.IoInputKeyboardHeldCount, 7);
    Atomics.store(status, StatusIndex.IoInputMouseButtonsHeldMask, 5);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyUs, 123);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyEwmaUs, 456);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyMaxUs, 789);
    Atomics.store(status, StatusIndex.IoInputEventLatencyAvgUs, 111);
    Atomics.store(status, StatusIndex.IoInputEventLatencyEwmaUs, 222);
    Atomics.store(status, StatusIndex.IoInputEventLatencyMaxUs, 333);
    Atomics.store(status, StatusIndex.IoInputBatchCounter, 99);
    Atomics.store(status, StatusIndex.IoInputEventCounter, 199);
    Atomics.store(status, StatusIndex.IoInputBatchReceivedCounter, 299);
    Atomics.store(status, StatusIndex.IoInputBatchDropCounter, 399);
    Atomics.store(status, StatusIndex.IoKeyboardBackendSwitchCounter, 499);
    Atomics.store(status, StatusIndex.IoMouseBackendSwitchCounter, 599);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      expect(Atomics.load(status, StatusIndex.IoInputKeyboardBackend)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputMouseBackend)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputVirtioKeyboardDriverOk)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputVirtioMouseDriverOk)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputUsbKeyboardOk)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputUsbMouseOk)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputBatchSendLatencyUs)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputBatchSendLatencyEwmaUs)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputBatchSendLatencyMaxUs)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputEventLatencyAvgUs)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputEventLatencyEwmaUs)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputEventLatencyMaxUs)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputBatchCounter)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputEventCounter)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputBatchDropCounter)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoKeyboardBackendSwitchCounter)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoMouseBackendSwitchCounter)).toBe(0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("publishes virtio-input driver_ok into shared status (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Enable a dummy machine instance with virtio driver_ok probes before init so the first
      // heartbeat publishes status without needing a full 250ms wait.
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine", virtioKeyboardOk: true, virtioMouseOk: false });

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      // Poll for up to 1s to avoid flakes under heavy CI load.
      const deadline = Date.now() + 1000;
      while (Date.now() < deadline) {
        const kbdOk = Atomics.load(status, StatusIndex.IoInputVirtioKeyboardDriverOk);
        const mouseOk = Atomics.load(status, StatusIndex.IoInputVirtioMouseDriverOk);
        if (kbdOk !== 0 || mouseOk !== 0) break;
        await new Promise((resolve) => setTimeout(resolve, 25));
      }

      expect(Atomics.load(status, StatusIndex.IoInputVirtioKeyboardDriverOk)).toBe(1);
      expect(Atomics.load(status, StatusIndex.IoInputVirtioMouseDriverOk)).toBe(0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("forwards AeroGPU fence completion messages into Machine.aerogpu_complete_fence (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const fence = 123n;
      const completion = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; fence?: unknown }).type === "__test.machine_cpu.aerogpu_complete_fence" &&
          (msg as { type?: unknown; fence?: unknown }).fence === fence,
        10_000,
      );
      worker.postMessage({ kind: "aerogpu.complete_fence", fence });
      await completion;
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("drains dummy AeroGPU submissions and posts aerogpu.submit messages", async () => {
    const segments = allocateTestSegments();
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const cmdBytes = new Uint8Array([1, 2, 3]);
      const allocBytes = new Uint8Array([9, 8, 7, 6]);
      const signalFence = 5n;
      const contextId = 42;
      const flags = 3;
      const engineId = 7;

      const submit = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "aerogpu.submit",
        10_000,
      );
      worker.postMessage({
        kind: "__test.machine_cpu.enqueueDummyAerogpuSubmission",
        cmdStream: cmdBytes,
        allocTable: allocBytes,
        signalFence,
        contextId,
        flags,
        engineId,
      });

      const regions = ringRegionsForWorker("cpu");
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      if (!commandRing.tryPush(encodeCommand({ kind: "nop", seq: 1 }))) {
        throw new Error("Failed to push nop command into command ring.");
      }

      const msg = (await submit) as {
        kind: string;
        contextId?: number;
        flags?: number;
        engineId?: number;
        signalFence?: bigint;
        cmdStream?: ArrayBuffer;
        allocTable?: ArrayBuffer;
      };

      expect(msg.kind).toBe("aerogpu.submit");
      expect(msg.contextId).toBe(contextId);
      expect(msg.signalFence).toBe(signalFence);
      expect(msg.flags).toBe(flags);
      expect(msg.engineId).toBe(engineId);

      expect(msg.cmdStream).toBeInstanceOf(ArrayBuffer);
      expect(Array.from(new Uint8Array(msg.cmdStream!))).toEqual(Array.from(cmdBytes));
      expect(msg.allocTable).toBeInstanceOf(ArrayBuffer);
      expect(Array.from(new Uint8Array(msg.allocTable!))).toEqual(Array.from(allocBytes));
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("does not drain AeroGPU submissions (or apply fence completions) until GPU is READY when the bridge is disabled", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);
    Atomics.store(status, StatusIndex.GpuReady, 0);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine", enableAerogpuBridge: false });
      await dummyReady;

      // Start the run loop so `run_slice()` + `drainAerogpuSubmissions()` execute.
      const regions = ringRegionsForWorker("cpu");
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      if (!commandRing.tryPush(encodeCommand({ kind: "nop", seq: 1 }))) {
        throw new Error("Failed to push nop command into command ring.");
      }

      // With the bridge disabled, fence completion messages should be ignored (to avoid
      // accidentally enabling bridge semantics before we can deliver GPU executor completions).
      const earlyFence = 111n;
      worker.postMessage({ kind: "aerogpu.complete_fence", fence: earlyFence });
      await expect(
        waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { type?: unknown; fence?: unknown }).type === "__test.machine_cpu.aerogpu_complete_fence" &&
            (msg as { type?: unknown; fence?: unknown }).fence === earlyFence,
          200,
        ),
      ).rejects.toThrow(/timed out/i);

      // Enqueue a synthetic submission but keep the shared GPU READY flag at 0.
      worker.postMessage({
        kind: "__test.machine_cpu.enqueueDummyAerogpuSubmission",
        cmdStream: new Uint8Array([1, 2, 3]),
        allocTable: new Uint8Array([9, 8, 7]),
        signalFence: 5n,
        contextId: 1,
      });

      // While GPU is not ready, draining should not occur.
      await expect(
        waitForWorkerMessage(
          worker,
          (msg) => (msg as { kind?: unknown }).kind === "aerogpu.submit",
          200,
        ),
      ).rejects.toThrow(/timed out/i);

      // Flip the shared GPU READY flag; the next drain should forward the submission.
      Atomics.store(status, StatusIndex.GpuReady, 1);
      // Wake the ring wait (best-effort) to keep the test snappy under heavy load.
      void commandRing.tryPush(encodeCommand({ kind: "nop", seq: 2 }));

      const submit = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "aerogpu.submit",
        10_000,
      )) as { cmdStream?: unknown; allocTable?: unknown; signalFence?: unknown; contextId?: unknown };

      expect(submit.contextId).toBe(1);
      expect(submit.signalFence).toBe(5n);
      expect(submit.cmdStream).toBeInstanceOf(ArrayBuffer);
      expect(submit.allocTable).toBeInstanceOf(ArrayBuffer);

      // Once draining has enabled bridge semantics, completions should be applied to the Machine.
      const lateFence = 222n;
      const completion = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; fence?: unknown }).type === "__test.machine_cpu.aerogpu_complete_fence" &&
          (msg as { type?: unknown; fence?: unknown }).fence === lateFence,
        10_000,
      );
      worker.postMessage({ kind: "aerogpu.complete_fence", fence: lateFence });
      await completion;
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("queues AeroGPU fence completions while snapshot-paused and flushes them on resume (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const pauseAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await pauseAck;

      // While snapshot-paused, fence completion messages should be queued but not applied to the Machine.
      const fence = 123n;
      worker.postMessage({ kind: "aerogpu.complete_fence", fence });
      await expect(
        waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { type?: unknown; fence?: unknown }).type === "__test.machine_cpu.aerogpu_complete_fence" &&
            (msg as { type?: unknown; fence?: unknown }).fence === fence,
          200,
        ),
      ).rejects.toThrow(/timed out/i);

      const resumedAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 2,
        10_000,
      );
      const completion = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; fence?: unknown }).type === "__test.machine_cpu.aerogpu_complete_fence" &&
          (msg as { type?: unknown; fence?: unknown }).fence === fence,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });

      await Promise.all([resumedAck, completion]);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("does not drain AeroGPU submissions while snapshot-paused and drains them after resume (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);
    Atomics.store(status, StatusIndex.GpuReady, 1);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const pauseAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await pauseAck;

      const cmdBytes = new Uint8Array([1, 2, 3, 4]);
      const allocBytes = new Uint8Array([9, 8, 7]);
      const signalFence = 77n;
      const contextId = 5;

      worker.postMessage({
        kind: "__test.machine_cpu.enqueueDummyAerogpuSubmission",
        cmdStream: cmdBytes,
        allocTable: allocBytes,
        signalFence,
        contextId,
      });

      // Flip `running=true` while paused so the run loop will execute `run_slice()` immediately
      // after resume (and thus reach `drainAerogpuSubmissions()`).
      const regions = ringRegionsForWorker("cpu");
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      void commandRing.tryPush(encodeCommand({ kind: "nop", seq: 1 }));

      // While snapshot-paused, draining should not occur.
      await expect(
        waitForWorkerMessage(
          worker,
          (msg) => (msg as { kind?: unknown }).kind === "aerogpu.submit",
          200,
        ),
      ).rejects.toThrow(/timed out/i);

      const resumedAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 2,
        10_000,
      );
      const submitPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "aerogpu.submit",
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });

      const [submit] = (await Promise.all([submitPromise, resumedAck])) as [
        { cmdStream?: unknown; allocTable?: unknown; signalFence?: unknown; contextId?: unknown },
        unknown,
      ];

      expect(submit.contextId).toBe(contextId);
      expect(submit.signalFence).toBe(signalFence);
      expect(submit.cmdStream).toBeInstanceOf(ArrayBuffer);
      expect(submit.allocTable).toBeInstanceOf(ArrayBuffer);
      expect(Array.from(new Uint8Array(submit.cmdStream as ArrayBuffer))).toEqual(Array.from(cmdBytes));
      expect(Array.from(new Uint8Array(submit.allocTable as ArrayBuffer))).toEqual(Array.from(allocBytes));
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("recycles input batch buffers when requested (even without WASM)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);
    const receivedBase = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
    const droppedBase = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const buf = new ArrayBuffer((2 + 4) * 4);
      const words = new Int32Array(buf);
      words[0] = 1; // count
      words[1] = 0; // timestamp (unused in this test)
      words[2] = InputEventType.KeyScancode;
      words[3] = 0; // event timestamp
      words[4] = 0x1c; // packed scancode bytes
      words[5] = 1; // len
      const expected = Array.from(words);
      const expectedByteLength = buf.byteLength;

      const recycledPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );

      worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);

      const recycled = (await recycledPromise) as { buffer?: unknown };
      const recycledBuf = recycled.buffer;
      if (!(recycledBuf instanceof ArrayBuffer)) {
        throw new Error("expected in:input-batch-recycle to carry an ArrayBuffer");
      }
      if (recycledBuf.byteLength !== expectedByteLength) {
        throw new Error(`expected recycled buffer byteLength=${expectedByteLength}, got ${recycledBuf.byteLength}`);
      }
      const got = Array.from(new Int32Array(recycledBuf));
      if (got.join(",") !== expected.join(",")) {
        throw new Error(`unexpected recycled buffer contents: got [${got.join(",")}] expected [${expected.join(",")}]`);
      }

      const receivedAfter = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
      const droppedAfter = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;
      if (receivedAfter - receivedBase !== 1) {
        throw new Error(`expected IoInputBatchReceivedCounter to increase by 1, got ${receivedAfter - receivedBase}`);
      }
      if (droppedAfter - droppedBase !== 0) {
        throw new Error(`expected IoInputBatchDropCounter to not change, got ${droppedAfter - droppedBase}`);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("updates input latency telemetry when processing batches (dummy machine, no WASM)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      // Enable a dummy machine instance inside the worker so input batches are processed even without WASM.
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });

      const receivedBase = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
      const processedBase = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      const eventsBase = Atomics.load(status, StatusIndex.IoInputEventCounter) >>> 0;
      const droppedBase = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;

      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      const tsUs = (nowUs - 1_000_000) >>> 0; // 1s in the past (u32 wrap-safe), avoids zero-latency edge cases.

      const buf = new ArrayBuffer((2 + 4) * 4);
      const words = new Int32Array(buf);
      words[0] = 1; // count
      words[1] = tsUs | 0; // batch send timestamp
      words[2] = InputEventType.KeyScancode;
      words[3] = tsUs | 0; // event timestamp
      words[4] = 0x1c; // packed scancode bytes
      words[5] = 1; // len

      const recycledPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );
      worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
      await recycledPromise;

      expect((Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0) - receivedBase).toBe(1);
      expect((Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0) - processedBase).toBe(1);
      expect((Atomics.load(status, StatusIndex.IoInputEventCounter) >>> 0) - eventsBase).toBe(1);
      expect((Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0) - droppedBase).toBe(0);

      const batchLatency = Atomics.load(status, StatusIndex.IoInputBatchSendLatencyUs) >>> 0;
      const batchLatencyEwma = Atomics.load(status, StatusIndex.IoInputBatchSendLatencyEwmaUs) >>> 0;
      const batchLatencyMax = Atomics.load(status, StatusIndex.IoInputBatchSendLatencyMaxUs) >>> 0;
      const eventLatencyAvg = Atomics.load(status, StatusIndex.IoInputEventLatencyAvgUs) >>> 0;
      const eventLatencyEwma = Atomics.load(status, StatusIndex.IoInputEventLatencyEwmaUs) >>> 0;
      const eventLatencyMax = Atomics.load(status, StatusIndex.IoInputEventLatencyMaxUs) >>> 0;

      expect(batchLatency).not.toBe(0);
      expect(batchLatencyEwma).toBe(batchLatency);
      expect(batchLatencyMax).toBe(batchLatency);

      expect(eventLatencyAvg).not.toBe(0);
      expect(eventLatencyEwma).toBe(eventLatencyAvg);
      expect(eventLatencyMax).toBe(eventLatencyAvg);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("updates held-state telemetry for keyboard HID usages and mouse buttons (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });

      expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0).toBe(0);

      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      const tsUs = (nowUs - 1_000_000) >>> 0;

      async function sendBatch(buffer: ArrayBuffer): Promise<void> {
        const recycled = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle", 10_000);
        worker.postMessage({ type: "in:input-batch", buffer, recycle: true }, [buffer]);
        await recycled;
      }

      // Press one key.
      {
        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1;
        words[1] = tsUs | 0;
        words[2] = InputEventType.KeyHidUsage;
        words[3] = tsUs | 0;
        words[4] = 0x04 | (1 << 8); // usage=4 pressed
        words[5] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(1);
        expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0).toBe(0);
      }

      // Press the same key again (should not double count).
      {
        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1;
        words[1] = tsUs | 0;
        words[2] = InputEventType.KeyHidUsage;
        words[3] = tsUs | 0;
        words[4] = 0x04 | (1 << 8);
        words[5] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(1);
      }

      // Press another key.
      {
        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1;
        words[1] = tsUs | 0;
        words[2] = InputEventType.KeyHidUsage;
        words[3] = tsUs | 0;
        words[4] = 0x05 | (1 << 8); // usage=5 pressed
        words[5] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(2);
      }

      // Hold mouse buttons with an all-ones bitmask; the worker should mask down to the lower 5 bits.
      {
        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1;
        words[1] = tsUs | 0;
        words[2] = InputEventType.MouseButtons;
        words[3] = tsUs | 0;
        words[4] = 0xff;
        words[5] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(2);
        expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0).toBe(0x1f);
      }

      // Release one key.
      {
        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1;
        words[1] = tsUs | 0;
        words[2] = InputEventType.KeyHidUsage;
        words[3] = tsUs | 0;
        words[4] = 0x04; // usage=4 released
        words[5] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(1);
        expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0).toBe(0x1f);
      }

      // Mouse buttons up.
      {
        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1;
        words[1] = tsUs | 0;
        words[2] = InputEventType.MouseButtons;
        words[3] = tsUs | 0;
        words[4] = 0x00;
        words[5] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(1);
        expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0).toBe(0);
      }

      // Release the remaining key and send a redundant release (should not underflow).
      {
        const buf = new ArrayBuffer((2 + 4 * 2) * 4);
        const words = new Int32Array(buf);
        words[0] = 2;
        words[1] = tsUs | 0;
        words[2] = InputEventType.KeyHidUsage;
        words[3] = tsUs | 0;
        words[4] = 0x04; // redundant release
        words[5] = 0;
        words[6] = InputEventType.KeyHidUsage;
        words[7] = tsUs | 0;
        words[8] = 0x05; // usage=5 released
        words[9] = 0;
        await sendBatch(buf);
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) >>> 0).toBe(0);
        expect(Atomics.load(status, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0).toBe(0);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("does not force-inject PS/2 scancodes after an unknown Consumer Control release when the USB keyboard backend is active (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({
        kind: "__test.machine_cpu.enableDummyMachine",
        virtioKeyboardOk: false,
        virtioMouseOk: false,
        usbKeyboardOk: true,
        enableInputSpy: true,
      });
      await dummyReady;

      let injected = false;
      const onMessage = (msg: unknown) => {
        if ((msg as { kind?: unknown }).kind === "__test.machine_cpu.inject_key_scancode_bytes") {
          injected = true;
        }
      };
      worker.on("message", onMessage);
      try {
        const nowUs = Math.round(performance.now() * 1000) >>> 0;

        // 2 events: unknown consumer release (page 0x0C) + an unrelated PS/2 scancode.
        // Regression: we should not "arm" forced PS/2 scancode delivery for consumer releases, or it can
        // be misapplied to unrelated KeyScancode events later in the batch.
        const buf = new ArrayBuffer((2 + 4 * 2) * 4);
        const words = new Int32Array(buf);
        words[0] = 2;
        words[1] = nowUs | 0;

        words[2] = InputEventType.HidUsage16;
        words[3] = nowUs | 0;
        words[4] = 0x000c; // usagePage=0x0c, pressed=0
        words[5] = 0x00e9; // volume up

        words[6] = InputEventType.KeyScancode;
        words[7] = nowUs | 0;
        words[8] = 0x1cf0; // break for make=0x1c => [0xf0, 0x1c]
        words[9] = 2;

        const recycledPromise = waitForWorkerMessage(
          worker,
          (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
          10_000,
        );
        worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
        await recycledPromise;

        // Sanity-check that the worker actually considered USB viable.
        // (Backend status is updated as a side-effect of processing the input batch.)
        expect(Atomics.load(status, StatusIndex.IoInputKeyboardBackend)).toBe(1); // usb

        expect(injected).toBe(false);
      } finally {
        worker.off("message", onMessage);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("force-injects PS/2 scancodes after an unknown keyboard key release when the USB backend is active (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({
        kind: "__test.machine_cpu.enableDummyMachine",
        virtioKeyboardOk: false,
        virtioMouseOk: false,
        usbKeyboardOk: true,
        enableInputSpy: true,
      });
      await dummyReady;

      const injectedPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.inject_key_scancode_bytes",
        10_000,
      );

      const nowUs = Math.round(performance.now() * 1000) >>> 0;

      // Unknown KeyHidUsage release (usage=4), followed by the matching PS/2 break scancode bytes.
      // This simulates a post-snapshot restore release where host-side held-key state was reset.
      const buf = new ArrayBuffer((2 + 4 * 2) * 4);
      const words = new Int32Array(buf);
      words[0] = 2;
      words[1] = nowUs | 0;

      words[2] = InputEventType.KeyHidUsage;
      words[3] = nowUs | 0;
      words[4] = 0x04; // usage=4 released (unknown)
      words[5] = 0;

      words[6] = InputEventType.KeyScancode;
      words[7] = nowUs | 0;
      words[8] = 0x1cf0; // break for make=0x1c => [0xf0, 0x1c]
      words[9] = 2;

      const recycledPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );

      worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
      await Promise.all([injectedPromise, recycledPromise]);

      // Sanity-check that the worker actually considered USB viable (even though we forced a PS/2
      // scancode break injection).
      expect(Atomics.load(status, StatusIndex.IoInputKeyboardBackend)).toBe(1); // usb
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("selects the USB backend when PS/2 i8042 is unavailable even before USB HID is configured (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({
        kind: "__test.machine_cpu.enableDummyMachine",
        ps2Available: false,
        virtioKeyboardOk: false,
        virtioMouseOk: false,
      });
      await dummyReady;

      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      // Use any non-empty batch to trigger backend re-evaluation + status publishing.
      const buf = new ArrayBuffer((2 + 4) * 4);
      const words = new Int32Array(buf);
      words[0] = 1;
      words[1] = nowUs | 0;
      words[2] = InputEventType.KeyHidUsage;
      words[3] = nowUs | 0;
      words[4] = 0x04 | (1 << 8); // usage=4 pressed
      words[5] = 0;

      const recycledPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );
      worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
      await recycledPromise;

      expect(Atomics.load(status, StatusIndex.IoInputKeyboardBackend)).toBe(1); // usb
      expect(Atomics.load(status, StatusIndex.IoInputMouseBackend)).toBe(1); // usb
      // Device is routed over USB because PS/2 is absent, but it still isn't configured by the guest yet.
      expect(Atomics.load(status, StatusIndex.IoInputUsbKeyboardOk)).toBe(0);
      expect(Atomics.load(status, StatusIndex.IoInputUsbMouseOk)).toBe(0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("counts clamped input batch claims as drops (dummy machine)", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });

      const droppedBase = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;
      const processedBase = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      const eventsBase = Atomics.load(status, StatusIndex.IoInputEventCounter) >>> 0;

      // Buffer contains 1 event, but claims 2 in the header; validateInputBatchBuffer should clamp it.
      const buf = new ArrayBuffer((2 + 4) * 4);
      const words = new Int32Array(buf);
      words[0] = 2; // claimed count (too large for buffer)
      words[1] = 0;
      words[2] = InputEventType.KeyScancode;
      words[3] = 0;
      words[4] = 0x1c;
      words[5] = 1;

      const recycledPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );
      worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
      await recycledPromise;

      expect((Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0) - processedBase).toBe(1);
      expect((Atomics.load(status, StatusIndex.IoInputEventCounter) >>> 0) - eventsBase).toBe(1);
      expect((Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0) - droppedBase).toBe(1);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("queues input batches while snapshot-paused and flushes them on resume", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);
    const receivedBase = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
    const droppedBase = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const messages: unknown[] = [];
      const onMessage = (msg: unknown) => {
        messages.push(msg);
      };
      worker.on("message", onMessage);
      try {
        const pause1Ack = waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { kind?: unknown; requestId?: unknown; ok?: unknown }).kind === "vm.snapshot.paused" &&
            (msg as { kind?: unknown; requestId?: unknown; ok?: unknown }).requestId === 1,
          10_000,
        );
        worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
        await pause1Ack;

        messages.length = 0;

        const buf = new ArrayBuffer((2 + 4) * 4);
        const words = new Int32Array(buf);
        words[0] = 1; // count
        words[1] = 0; // timestamp (unused in this test)
        words[2] = InputEventType.KeyScancode;
        words[3] = 0; // event timestamp
        words[4] = 0x1c; // packed scancode bytes
        words[5] = 1; // len
        const expected = Array.from(words);
        const expectedByteLength = buf.byteLength;

        worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);

        const pause2Ack = waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
            (msg as { kind?: unknown; requestId?: unknown }).requestId === 2,
          10_000,
        );
        worker.postMessage({ kind: "vm.snapshot.pause", requestId: 2 });
        await pause2Ack;

        if (messages.some((msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle")) {
          throw new Error("expected input batch buffers to not recycle while vm.snapshot.pause is active");
        }

        const recycledPromise = waitForWorkerMessage(
          worker,
          (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
          10_000,
        );
        const resumedPromise = waitForWorkerMessage(
          worker,
          (msg) =>
            (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.resumed" &&
            (msg as { kind?: unknown; requestId?: unknown }).requestId === 3,
          10_000,
        );
        worker.postMessage({ kind: "vm.snapshot.resume", requestId: 3 });

        const [recycled] = (await Promise.all([recycledPromise, resumedPromise])) as [unknown, unknown];
        const recycledBuf = (recycled as { buffer?: unknown }).buffer;
        if (!(recycledBuf instanceof ArrayBuffer)) {
          throw new Error("expected in:input-batch-recycle to carry an ArrayBuffer");
        }
        if (recycledBuf.byteLength !== expectedByteLength) {
          throw new Error(`expected recycled buffer byteLength=${expectedByteLength}, got ${recycledBuf.byteLength}`);
        }
        const got = Array.from(new Int32Array(recycledBuf));
        if (got.join(",") !== expected.join(",")) {
          throw new Error(`unexpected recycled buffer contents: got [${got.join(",")}] expected [${expected.join(",")}]`);
        }

        const receivedAfter = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
        const droppedAfter = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;
        if (receivedAfter - receivedBase !== 1) {
          throw new Error(`expected IoInputBatchReceivedCounter to increase by 1, got ${receivedAfter - receivedBase}`);
        }
        if (droppedAfter - droppedBase !== 0) {
          throw new Error(`expected IoInputBatchDropCounter to not change, got ${droppedAfter - droppedBase}`);
        }
      } finally {
        worker.off("message", onMessage);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("enforces vm.snapshot.pause before machine snapshot RPCs and reports missing WASM", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const notPausedPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.machine.saveToOpfs", requestId: 1, path: "state/test.snap" });
      const notPaused = (await notPausedPromise) as { ok?: unknown; error?: unknown };
      if (notPaused.ok !== false) {
        throw new Error("expected vm.snapshot.machine.saved to return ok=false when VM is not paused");
      }
      const notPausedErr = (notPaused.error as { message?: unknown } | undefined)?.message;
      if (typeof notPausedErr !== "string" || !notPausedErr.toLowerCase().includes("paused")) {
        throw new Error(`expected not-paused error message to mention pause, got: ${String(notPausedErr)}`);
      }

      const pauseAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 2,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 2 });
      await pauseAck;

      const missingWasmPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 3,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.machine.saveToOpfs", requestId: 3, path: "state/test.snap" });
      const missingWasm = (await missingWasmPromise) as { ok?: unknown; error?: unknown };
      if (missingWasm.ok !== false) {
        throw new Error("expected vm.snapshot.machine.saved to return ok=false when WASM is unavailable");
      }
      const missingWasmErr = (missingWasm.error as { message?: unknown } | undefined)?.message;
      if (typeof missingWasmErr !== "string" || !missingWasmErr.toLowerCase().includes("wasm")) {
        throw new Error(`expected missing-WASM error message to mention WASM, got: ${String(missingWasmErr)}`);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("enforces vm.snapshot.pause before machine restore RPCs and reports missing WASM", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const notPausedPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.restored" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.machine.restoreFromOpfs", requestId: 1, path: "state/test.snap" });
      const notPaused = (await notPausedPromise) as { ok?: unknown; error?: unknown };
      if (notPaused.ok !== false) {
        throw new Error("expected vm.snapshot.machine.restored to return ok=false when VM is not paused");
      }
      const notPausedErr = (notPaused.error as { message?: unknown } | undefined)?.message;
      if (typeof notPausedErr !== "string" || !notPausedErr.toLowerCase().includes("paused")) {
        throw new Error(`expected not-paused error message to mention pause, got: ${String(notPausedErr)}`);
      }

      const pauseAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 2,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 2 });
      await pauseAck;

      const missingWasmPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.restored" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 3,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.machine.restoreFromOpfs", requestId: 3, path: "state/test.snap" });
      const missingWasm = (await missingWasmPromise) as { ok?: unknown; error?: unknown };
      if (missingWasm.ok !== false) {
        throw new Error("expected vm.snapshot.machine.restored to return ok=false when WASM is unavailable");
      }
      const missingWasmErr = (missingWasm.error as { message?: unknown } | undefined)?.message;
      if (typeof missingWasmErr !== "string" || !missingWasmErr.toLowerCase().includes("wasm")) {
        throw new Error(`expected missing-WASM error message to mention WASM, got: ${String(missingWasmErr)}`);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("processes multiple queued machine snapshot save requests (no WASM)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const pauseAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await pauseAck;

      const save1 = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 10,
        10_000,
      );
      const save2 = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 11,
        10_000,
      );

      worker.postMessage({ kind: "vm.snapshot.machine.saveToOpfs", requestId: 10, path: "state/a.snap" });
      worker.postMessage({ kind: "vm.snapshot.machine.saveToOpfs", requestId: 11, path: "state/b.snap" });

      const [res1, res2] = (await Promise.all([save1, save2])) as [any, any];
      for (const res of [res1, res2]) {
        if (res.ok !== false) {
          throw new Error("expected vm.snapshot.machine.saved to return ok=false when WASM is unavailable");
        }
        const message = (res.error as { message?: unknown } | undefined)?.message;
        if (typeof message !== "string" || !message.toLowerCase().includes("wasm")) {
          throw new Error(`expected missing-WASM error message to mention WASM, got: ${String(message)}`);
        }
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("drops excess input batches while snapshot-paused and recycles them immediately", async () => {
    const segments = allocateTestSegments();
    const status = new Int32Array(segments.control, STATUS_OFFSET_BYTES, STATUS_INTS);
    const receivedBase = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
    const droppedBase = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const pauseAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await pauseAck;

      // Fill the pause queue (~4 MiB).
      const big = new ArrayBuffer(4 * 1024 * 1024);
      const bigWords = new Int32Array(big);
      bigWords[0] = 0; // count=0, so flush won't do work.
      bigWords[1] = 1111; // sentinel
      worker.postMessage({ type: "in:input-batch", buffer: big, recycle: true }, [big]);

      // This small batch should exceed the queue limit and be recycled immediately (before resume).
      const small = new ArrayBuffer((2 + 4) * 4);
      const smallWords = new Int32Array(small);
      smallWords[0] = 0;
      smallWords[1] = 2222; // sentinel

      const recycledWhilePaused = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );
      worker.postMessage({ type: "in:input-batch", buffer: small, recycle: true }, [small]);

      const firstRecycle = (await recycledWhilePaused) as { buffer?: unknown };
      const firstRecycleBuf = firstRecycle.buffer;
      if (!(firstRecycleBuf instanceof ArrayBuffer)) {
        throw new Error("expected in:input-batch-recycle to carry an ArrayBuffer");
      }
      const firstRecycleWords = new Int32Array(firstRecycleBuf);
      if (firstRecycleWords[1] !== 2222) {
        throw new Error(
          `expected the dropped buffer (sentinel=2222) to be recycled while paused, got sentinel=${firstRecycleWords[1]}`,
        );
      }

      const receivedAfterDrop = Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0;
      const droppedAfterDrop = Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0;
      if (receivedAfterDrop - receivedBase !== 2) {
        throw new Error(`expected IoInputBatchReceivedCounter to increase by 2, got ${receivedAfterDrop - receivedBase}`);
      }
      if (droppedAfterDrop - droppedBase !== 1) {
        throw new Error(`expected IoInputBatchDropCounter to increase by 1, got ${droppedAfterDrop - droppedBase}`);
      }

      const recycledOnResume = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );
      const resumedAck = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 2,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });

      const [secondRecycle] = (await Promise.all([recycledOnResume, resumedAck])) as [unknown, unknown];
      const secondRecycleBuf = (secondRecycle as { buffer?: unknown }).buffer;
      if (!(secondRecycleBuf instanceof ArrayBuffer)) {
        throw new Error("expected in:input-batch-recycle to carry an ArrayBuffer");
      }
      const secondRecycleWords = new Int32Array(secondRecycleBuf);
      if (secondRecycleWords[1] !== 1111) {
        throw new Error(
          `expected the queued buffer (sentinel=1111) to be recycled on resume, got sentinel=${secondRecycleWords[1]}`,
        );
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (boot device selection)", () => {
  function makeLocalHddMeta(): any {
    return {
      source: "local",
      id: "hdd0",
      name: "hdd0",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "hdd0.img",
      sizeBytes: 1024 * 1024,
      createdAtMs: 0,
    };
  }

  function makeLocalCdMeta(): any {
    return {
      source: "local",
      id: "cd0",
      name: "cd0",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "cd0.iso",
      sizeBytes: 2048,
      createdAtMs: 0,
    };
  }

  it("selects CD boot when install media is present", async () => {
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const msgPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "cdrom",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd: makeLocalHddMeta(),
        cd: makeLocalCdMeta(),
      } satisfies SetBootDisksMessage);

      const msg = await msgPromise;
      expect(msg.bootDevice).toBe("cdrom");
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("selects HDD boot when install media is absent", async () => {
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const msgPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "hdd",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0" },
        hdd: makeLocalHddMeta(),
        cd: null,
      } satisfies SetBootDisksMessage);

      const msg = await msgPromise;
      expect(msg.bootDevice).toBe("hdd");
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("honors explicit bootDevice preference when provided", async () => {
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const msgPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "hdd",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd: makeLocalHddMeta(),
        cd: makeLocalCdMeta(),
        bootDevice: "hdd",
      } satisfies SetBootDisksMessage);

      const msg = await msgPromise;
      expect(msg.bootDevice).toBe("hdd");
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("honors explicit CD boot when mounts select a CD even if CD metadata is missing", async () => {
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const msgPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "cdrom",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd: makeLocalHddMeta(),
        cd: null,
        bootDevice: "cdrom",
      } satisfies SetBootDisksMessage);

      const msg = await msgPromise;
      expect(msg.bootDevice).toBe("cdrom");
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("ignores explicit bootDevice values when the corresponding device is absent", async () => {
    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const msgPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "hdd",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      // bootDevice=cdrom is ignored because no CD is attached.
      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0" },
        hdd: makeLocalHddMeta(),
        cd: null,
        bootDevice: "cdrom",
      } satisfies SetBootDisksMessage);

      const msg = await msgPromise;
      expect(msg.bootDevice).toBe("hdd");
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (active boot device reporting)", () => {
  it("reports the firmware-selected active boot device after reset (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const activePromise = waitForWorkerMessage(
        worker,
        (msg) => {
          const maybe = msg as { type?: unknown; bootDevice?: unknown };
          const bootDevice = maybe.bootDevice;
          return (
            maybe.type === "machineCpu.bootDeviceActive" && (bootDevice === "cdrom" || bootDevice === "hdd")
          );
        },
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      const cdMeta: any = {
        source: "local",
        id: "cd0",
        name: "cd0",
        backend: "opfs",
        kind: "cd",
        format: "iso",
        fileName: "cd0.iso",
        sizeBytes: 2048,
        createdAtMs: 0,
      };

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { cdId: "cd0" },
        hdd: null,
        cd: cdMeta,
      } satisfies SetBootDisksMessage);

      const msg = await activePromise;
      expect(msg.bootDevice).toBe("cdrom");
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (CD-first boot policy)", () => {
  it("keeps boot drive as HDD0 while booting from CD when both devices are present (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine", enableBootDriveSpy: true });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));

      await workerReady;

      const setBootDrivePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; drive?: unknown }).kind === "__test.machine_cpu.setBootDrive" &&
          (msg as { kind?: unknown; drive?: unknown }).drive === 0x80,
        10_000,
      ) as Promise<{ kind: string; drive: number }>;

      const activePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceActive" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "cdrom",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      const bootConfigPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDrive?: unknown; cdBootDrive?: unknown; bootFromCdIfPresent?: unknown }).type === "machineCpu.bootConfig" &&
          (msg as { type?: unknown; bootDrive?: unknown }).bootDrive === 0x80 &&
          (msg as { type?: unknown; cdBootDrive?: unknown }).cdBootDrive === 0xe0 &&
          (msg as { type?: unknown; bootFromCdIfPresent?: unknown }).bootFromCdIfPresent === true,
        10_000,
      ) as Promise<{ type: string; bootDrive: number; cdBootDrive: number; bootFromCdIfPresent: boolean }>;

      const hddMeta: any = {
        source: "local",
        id: "hdd0",
        name: "hdd0",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        fileName: "hdd0.img",
        sizeBytes: 1024 * 1024,
        createdAtMs: 0,
      };

      const cdMeta: any = {
        source: "local",
        id: "cd0",
        name: "cd0",
        backend: "opfs",
        kind: "cd",
        format: "iso",
        fileName: "cd0.iso",
        sizeBytes: 2048,
        createdAtMs: 0,
      };

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd: hddMeta,
        cd: cdMeta,
        bootDevice: "cdrom",
      } satisfies SetBootDisksMessage);

      const bootDriveMsg = await setBootDrivePromise;
      expect(bootDriveMsg.drive).toBe(0x80);

      const activeMsg = await activePromise;
      expect(activeMsg.bootDevice).toBe("cdrom");

      const bootConfigMsg = await bootConfigPromise;
      expect(bootConfigMsg.bootDrive).toBe(0x80);
      expect(bootConfigMsg.cdBootDrive).toBe(0xe0);
      expect(bootConfigMsg.bootFromCdIfPresent).toBe(true);
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (guest reset boot policy)", () => {
  it("switches from CD to HDD when the guest requests a reset, even if HDD metadata is missing (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      // Apply a boot disk selection that includes both mount IDs but omits HDD metadata (simulate
      // transient metadata-unavailable scenarios). This should still count as "HDD present" when
      // deciding whether to switch to HDD after the guest reboots.
      const activePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceActive" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "cdrom",
        10_000,
      );

      const cdMeta: any = {
        source: "local",
        id: "cd0",
        name: "cd0",
        backend: "opfs",
        kind: "cd",
        format: "iso",
        fileName: "cd0.iso",
        sizeBytes: 2048,
        createdAtMs: 0,
      };

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd: null,
        cd: cdMeta,
        bootDevice: "cdrom",
      } satisfies SetBootDisksMessage);
      await activePromise;

      // Ask the dummy machine to request a reset on its next `run_slice` call.
      worker.postMessage({ kind: "__test.machine_cpu.setDummyNextRunExitKind", exitKind: "ResetRequested" });

      const selectedPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "hdd",
        10_000,
      );

      // Start the run loop so it will call `run_slice` and observe the reset request.
      const regions = ringRegionsForWorker("cpu");
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      if (!commandRing.tryPush(encodeCommand({ kind: "nop", seq: 1 }))) {
        throw new Error("Failed to push nop command into command ring.");
      }

      const msg = (await selectedPromise) as { type: string; bootDevice: string };
      expect(msg.bootDevice).toBe("hdd");
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (boot drive API compat)", () => {
  it("boots from HDD when bootDevice=hdd and HDD is selected via mounts even if HDD metadata is missing (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine", enableBootDriveSpy: true });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));

      await workerReady;

      const setBootDrivePromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; drive?: unknown }).kind === "__test.machine_cpu.setBootDrive",
        10_000,
      ) as Promise<{ kind: string; drive: number }>;

      const cdMeta: any = {
        source: "local",
        id: "cd0",
        name: "cd0",
        backend: "opfs",
        kind: "cd",
        format: "iso",
        fileName: "cd0.iso",
        sizeBytes: 2048,
        createdAtMs: 0,
      };

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd: null,
        cd: cdMeta,
        bootDevice: "hdd",
      } satisfies SetBootDisksMessage);

      const msg = await setBootDrivePromise;
      expect(msg.drive).toBe(0x80);
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("uses camelCase setBootDrive when booting from CD (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine", enableBootDriveSpy: true });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));

      await workerReady;

      const setBootDrivePromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.setBootDrive",
        10_000,
      ) as Promise<{ kind: string; drive: number }>;

      const cdMeta: any = {
        source: "local",
        id: "cd0",
        name: "cd0",
        backend: "opfs",
        kind: "cd",
        format: "iso",
        fileName: "cd0.iso",
        sizeBytes: 2048,
        createdAtMs: 0,
      };

      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: { cdId: "cd0" },
        hdd: null,
        cd: cdMeta,
      } satisfies SetBootDisksMessage);

      const msg = await setBootDrivePromise;
      expect(msg.drive).toBe(0xe0);
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (network API compat)", () => {
  it("uses camelCase attach/detach network methods when proxyUrl toggles (dummy machine)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 8,
      vramBytes: 0,
    });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine", enableNetworkSpy: true });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      const attached = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.attachL2TunnelFromIoIpcSab",
        10_000,
      ) as Promise<{ kind: string; byteLength: number }>;

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig({ proxyUrl: "https://gateway.example.com" }),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const attachMsg = await attached;
      expect(attachMsg.byteLength).toBe(8);

      const detached = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.detachNetwork",
        10_000,
      );
      worker.postMessage({
        kind: "config.update",
        version: 2,
        config: makeConfig(),
      });
      await detached;
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

describe("workers/machine_cpu.worker (snapshot restore boot device reporting)", () => {
  it("reports active boot device after a successful machine snapshot restore (dummy machine)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const dummyReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "__test.machine_cpu.dummyMachineEnabled",
        10_000,
      );
      worker.postMessage({ kind: "__test.machine_cpu.enableDummyMachine" });
      await dummyReady;

      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );
      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const restoredPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "machine.snapshot.restored" &&
          (msg as { kind?: unknown; requestId?: unknown }).requestId === 1,
        10_000,
      ) as Promise<{ kind: string; requestId: number; ok: boolean }>;
      const activePromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceActive" &&
          (msg as { type?: unknown; bootDevice?: unknown }).bootDevice === "cdrom",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;
      const bootConfigPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown; bootDrive?: unknown; cdBootDrive?: unknown; bootFromCdIfPresent?: unknown }).type ===
            "machineCpu.bootConfig" &&
          (msg as { bootDrive?: unknown }).bootDrive === 0x80 &&
          (msg as { cdBootDrive?: unknown }).cdBootDrive === 0xe0 &&
          (msg as { bootFromCdIfPresent?: unknown }).bootFromCdIfPresent === false,
        10_000,
      ) as Promise<{ type: string; bootDrive: number; cdBootDrive: number; bootFromCdIfPresent: boolean }>;

      worker.postMessage({ kind: "machine.snapshot.restoreFromOpfs", requestId: 1, path: "state/test.snap" });

      const [restored, active, bootConfig] = await Promise.all([restoredPromise, activePromise, bootConfigPromise]);
      expect(restored.ok).toBe(true);
      expect(active.bootDevice).toBe("cdrom");
      expect(bootConfig.bootDrive).toBe(0x80);
      expect(bootConfig.cdBootDrive).toBe(0xe0);
      expect(bootConfig.bootFromCdIfPresent).toBe(false);
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});

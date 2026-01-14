import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { InputEventType } from "../input/event_queue";
import { STATUS_INTS, STATUS_OFFSET_BYTES, StatusIndex, allocateSharedMemorySegments, type SharedMemorySegments } from "../runtime/shared_layout";
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
        const errMsg = typeof (maybeProtocol as { message?: unknown }).message === "string" ? (maybeProtocol as any).message : "";
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

describe("workers/machine_cpu.worker (worker_threads)", () => {
  it("boots via config.update + init and reaches READY", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

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

  it("recycles input batch buffers when requested (even without WASM)", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
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

  it("queues input batches while snapshot-paused and flushes them on resume", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
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
          (msg) => (msg as { kind?: unknown; requestId?: unknown; ok?: unknown }).kind === "vm.snapshot.paused" && (msg as any).requestId === 1,
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
          (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" && (msg as any).requestId === 2,
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
          (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.resumed" && (msg as any).requestId === 3,
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
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" && (msg as any).requestId === 1,
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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" && (msg as any).requestId === 2,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 2 });
      await pauseAck;

      const missingWasmPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" && (msg as any).requestId === 3,
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
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.restored" && (msg as any).requestId === 1,
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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" && (msg as any).requestId === 2,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 2 });
      await pauseAck;

      const missingWasmPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.restored" && (msg as any).requestId === 3,
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
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" && (msg as any).requestId === 1,
        10_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await pauseAck;

      const save1 = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" && (msg as any).requestId === 10,
        10_000,
      );
      const save2 = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.machine.saved" && (msg as any).requestId === 11,
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
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });
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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.paused" && (msg as any).requestId === 1,
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
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "vm.snapshot.resumed" && (msg as any).requestId === 2,
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
        (msg) => (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" && (msg as any).bootDevice === "cdrom",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      worker.postMessage({
        type: "setBootDisks",
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
        (msg) => (msg as { type?: unknown; bootDevice?: unknown }).type === "machineCpu.bootDeviceSelected" && (msg as any).bootDevice === "hdd",
        10_000,
      ) as Promise<{ type: string; bootDevice: string }>;

      worker.postMessage({
        type: "setBootDisks",
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
});

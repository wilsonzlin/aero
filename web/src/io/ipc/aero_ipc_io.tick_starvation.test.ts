import { describe, expect, it } from "vitest";

import { once } from "node:events";
import { Worker, type WorkerOptions } from "node:worker_threads";

import { createIpcBuffer, openRingByKind } from "../../ipc/ipc.ts";
import { queueKind, ringCtrl } from "../../ipc/layout.ts";
import { encodeCommand } from "../../ipc/protocol.ts";
import { makeNodeWorkerExecArgv } from "../../test_utils/worker_threads_exec_argv";
import { unrefBestEffort } from "../../unrefSafe";

import { AeroIpcIoServer, type AeroIpcIoDispatchTarget } from "./aero_ipc_io.ts";

const WORKER_READY_TIMEOUT_MS = 10_000;
const TICK_TIMEOUT_MS = 10_000;
const SHUTDOWN_TIMEOUT_MS = 10_000;
const WORKER_EXEC_ARGV = makeNodeWorkerExecArgv();

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`timed out after ${timeoutMs}ms: ${label}`)), timeoutMs);
    unrefBestEffort(timer);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function sleep0(): Promise<void> {
  await new Promise((resolve) => {
    const timer = setTimeout(resolve, 0);
    unrefBestEffort(timer);
  });
}

function makeMalformedMmioWrite(dataLen: number): Uint8Array {
  // mmioWrite wire format:
  //   u16 tag (0x0101)
  //   u32 id
  //   u64 addr
  //   u32 len
  //   [len bytes]
  //
  // We deliberately set `len` larger than the actual payload so decodeCommand reads and slices
  // the data, then fails with "trailing bytes".
  const actualLen = dataLen >>> 0;
  const declaredLen = (actualLen + 0x1000) >>> 0;
  const bytes = new Uint8Array(2 + 4 + 8 + 4 + actualLen);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let off = 0;
  view.setUint16(off, 0x0101, true);
  off += 2;
  view.setUint32(off, 1, true);
  off += 4;
  // addr u64 = 0
  view.setUint32(off, 0, true);
  off += 4;
  view.setUint32(off, 0, true);
  off += 4;
  view.setUint32(off, declaredLen, true);
  off += 4;
  bytes.fill(0xaa, off);
  return bytes;
}

describe("io/ipc/aero_ipc_io tick fairness", () => {
  it("runAsync ticks while draining (no tick starvation under sustained command traffic)", async () => {
    const { buffer: ipcBuffer, queues } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);
    const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
    const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

    const cmdBytes = encodeCommand({ kind: "portWrite", id: 1, port: 0, size: 1, value: 0 });
    const cmdOffset = queues.find((q) => q.kind === queueKind.CMD)?.offsetBytes;
    if (cmdOffset == null) throw new Error("CMD ring not found in IPC buffer");
    const cmdCtrl = new Int32Array(ipcBuffer, cmdOffset, ringCtrl.WORDS);

    let tickCount = 0;
    let tickSawCmdData = false;
    let busyAcc = 0;

    const target: AeroIpcIoDispatchTarget = {
      portRead: () => 0,
      portWrite: () => {
        // Light deterministic work to keep the drain loop running long enough for
        // tick deadlines to elapse.
        for (let i = 0; i < 1000; i++) busyAcc = (busyAcc + i) | 0;
      },
      mmioRead: () => 0,
      mmioWrite: () => {},
      tick: () => {
        tickCount++;
        const head = Atomics.load(cmdCtrl, ringCtrl.HEAD);
        const tail = Atomics.load(cmdCtrl, ringCtrl.TAIL_COMMIT);
        if (head !== tail) tickSawCmdData = true;
      },
    };

    const server = new AeroIpcIoServer(cmdQ, evtQ, target, {
      tickIntervalMs: 1,
      emitEvent: () => {},
    });

    // Keep the ring non-empty from the first iteration.
    while (cmdQ.tryPush(cmdBytes)) {}

    const abort = new AbortController();
    const serverTask = server.runAsync({ signal: abort.signal, yieldEveryNCommands: 64 });

    try {
      await withTimeout(
        (async () => {
          while (!tickSawCmdData) {
            // Top up to keep the drain loop hot until at least one tick fires while data is still queued.
            while (cmdQ.tryPush(cmdBytes)) {}
            await sleep0();
          }
        })(),
        TICK_TIMEOUT_MS,
        "tick() while draining (runAsync)",
      );
    } finally {
      abort.abort();
      await withTimeout(serverTask, SHUTDOWN_TIMEOUT_MS, "server.runAsync() shutdown");
    }

    expect(tickCount).toBeGreaterThan(0);
    expect(tickSawCmdData).toBe(true);
    // Ensure command handling did non-trivial work (prevents JIT from optimizing away the busy loop).
    expect(busyAcc).not.toBe(0);
  });

  it("runAsync ticks while draining malformed commands (decode failures can't starve ticks)", async () => {
    const { buffer: ipcBuffer, queues } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);
    const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
    const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

    const cmdOffset = queues.find((q) => q.kind === queueKind.CMD)?.offsetBytes;
    if (cmdOffset == null) throw new Error("CMD ring not found in IPC buffer");
    const cmdCtrl = new Int32Array(ipcBuffer, cmdOffset, ringCtrl.WORDS);

    const malformed = makeMalformedMmioWrite(8 * 1024);

    let tickCount = 0;
    let tickSawCmdData = false;

    const target: AeroIpcIoDispatchTarget = {
      portRead: () => 0,
      portWrite: () => {},
      mmioRead: () => 0,
      mmioWrite: () => {},
      tick: () => {
        tickCount++;
        const head = Atomics.load(cmdCtrl, ringCtrl.HEAD);
        const tail = Atomics.load(cmdCtrl, ringCtrl.TAIL_COMMIT);
        if (head !== tail) tickSawCmdData = true;
      },
    };

    const server = new AeroIpcIoServer(cmdQ, evtQ, target, {
      tickIntervalMs: 1,
      emitEvent: () => {},
    });

    // Fill the ring up front so the server never blocks on an empty ring unless
    // it yields.
    while (cmdQ.tryPush(malformed)) {}

    const abort = new AbortController();
    const serverTask = server.runAsync({ signal: abort.signal, yieldEveryNCommands: 64 });

    try {
      await withTimeout(
        (async () => {
          while (!tickSawCmdData) {
            while (cmdQ.tryPush(malformed)) {}
            await sleep0();
          }
        })(),
        TICK_TIMEOUT_MS,
        "tick() while draining malformed commands (runAsync)",
      );
    } finally {
      abort.abort();
      await withTimeout(serverTask, SHUTDOWN_TIMEOUT_MS, "server.runAsync() shutdown (malformed)");
    }

    expect(tickCount).toBeGreaterThan(0);
    expect(tickSawCmdData).toBe(true);
  });

  it("run ticks while draining (no tick starvation under sustained command traffic)", async () => {
    const { buffer: ipcBuffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);

    const tickCounterSab = new SharedArrayBuffer(4);
    // 2x i32:
    //  - [0] set to 1 if any tick() fired while cmd ring still had pending data
    //  - [1] optional sink for the worker to store a checksum / busy accumulator (prevents
    //        the command handler from being optimized away as "pure work" in JIT)
    const tickSawCmdDataSab = new SharedArrayBuffer(8);
    const tickCounter = new Int32Array(tickCounterSab);
    const tickSawCmdData = new Int32Array(tickSawCmdDataSab);

    const worker = new Worker(new URL("./test_workers/aipc_tick_starvation_run_worker.ts", import.meta.url), {
      type: "module",
      workerData: {
        ipcBuffer,
        tickCounter: tickCounterSab,
        tickSawCmdData: tickSawCmdDataSab,
        tickIntervalMs: 1,
        // Make command handling non-trivial so the ring stays hot while we flood.
        workIters: 2000,
      },
      execArgv: WORKER_EXEC_ARGV,
    } as unknown as WorkerOptions);

    let exited = false;
    try {
      await withTimeout(once(worker, "message") as Promise<[any]>, WORKER_READY_TIMEOUT_MS, "server worker ready");

      const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
      const cmdBytes = encodeCommand({ kind: "portWrite", id: 1, port: 0, size: 1, value: 0 });

      // Start with a full ring so the server immediately enters the drain loop.
      while (cmdQ.tryPush(cmdBytes)) {}

      await withTimeout(
        (async () => {
          while (Atomics.load(tickSawCmdData, 0) !== 1) {
            // Keep the ring non-empty by pushing whenever space becomes available.
            // Batch pushes so time checks don't dominate and the producer stays ahead.
            for (let i = 0; i < 1024; i++) cmdQ.tryPush(cmdBytes);
            await sleep0();
          }
        })(),
        TICK_TIMEOUT_MS,
        "tick() while draining (run)",
      );

      expect(Atomics.load(tickCounter, 0)).toBeGreaterThan(0);
      expect(Atomics.load(tickSawCmdData, 0)).toBe(1);

      // Gracefully stop the blocking server loop so the worker can exit cleanly.
      const shutdownBytes = encodeCommand({ kind: "shutdown" });
      while (!cmdQ.tryPush(shutdownBytes)) {
        await sleep0();
      }
      await withTimeout(once(worker, "exit") as Promise<[number]>, SHUTDOWN_TIMEOUT_MS, "server worker exit");
      exited = true;
    } finally {
      if (!exited) await worker.terminate();
    }
  });

  it("run ticks while draining malformed commands (decode failures can't starve ticks)", async () => {
    const { buffer: ipcBuffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 19 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);

    const tickCounterSab = new SharedArrayBuffer(4);
    const tickSawCmdDataSab = new SharedArrayBuffer(8);
    const tickCounter = new Int32Array(tickCounterSab);
    const tickSawCmdData = new Int32Array(tickSawCmdDataSab);

    const worker = new Worker(new URL("./test_workers/aipc_tick_starvation_run_worker.ts", import.meta.url), {
      type: "module",
      workerData: {
        ipcBuffer,
        tickCounter: tickCounterSab,
        tickSawCmdData: tickSawCmdDataSab,
        tickIntervalMs: 1,
        // Work is driven by malformed decode cost, not handler cost.
        workIters: 0,
      },
      execArgv: WORKER_EXEC_ARGV,
    } as unknown as WorkerOptions);

    const malformed = makeMalformedMmioWrite(8 * 1024);

    let exited = false;
    try {
      await withTimeout(
        once(worker, "message") as Promise<[any]>,
        WORKER_READY_TIMEOUT_MS,
        "server worker ready (malformed)",
      );

      const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);

      // Pre-fill so the server starts draining immediately.
      while (cmdQ.tryPush(malformed)) {}

      await withTimeout(
        (async () => {
          while (Atomics.load(tickSawCmdData, 0) !== 1) {
            for (let i = 0; i < 256; i++) cmdQ.tryPush(malformed);
            await sleep0();
          }
        })(),
        TICK_TIMEOUT_MS,
        "tick() while draining malformed commands (run)",
      );

      expect(Atomics.load(tickCounter, 0)).toBeGreaterThan(0);
      expect(Atomics.load(tickSawCmdData, 0)).toBe(1);

      // Shut down the server loop.
      const shutdownBytes = encodeCommand({ kind: "shutdown" });
      while (!cmdQ.tryPush(shutdownBytes)) {
        await sleep0();
      }
      await withTimeout(
        once(worker, "exit") as Promise<[number]>,
        SHUTDOWN_TIMEOUT_MS,
        "server worker exit (malformed)",
      );
      exited = true;
    } finally {
      if (!exited) await worker.terminate();
    }
  });
});

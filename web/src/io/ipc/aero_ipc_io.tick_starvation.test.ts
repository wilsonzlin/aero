import { describe, expect, it } from "vitest";

import { once } from "node:events";
import { Worker, type WorkerOptions } from "node:worker_threads";

import { createIpcBuffer, openRingByKind } from "../../ipc/ipc.ts";
import { queueKind, ringCtrl } from "../../ipc/layout.ts";
import { encodeCommand } from "../../ipc/protocol.ts";

import { AeroIpcIoServer, type AeroIpcIoDispatchTarget } from "./aero_ipc_io.ts";

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`timed out after ${timeoutMs}ms: ${label}`)), timeoutMs);
    (timer as unknown as { unref?: () => void }).unref?.();
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
    (timer as unknown as { unref?: () => void }).unref?.();
  });
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

    const end = nowMs() + 50;
    while (nowMs() < end) {
      // Top up to ensure the drain loop never terminates.
      while (cmdQ.tryPush(cmdBytes)) {}
      await sleep0();
    }

    abort.abort();
    await withTimeout(serverTask, 2000, "server.runAsync() shutdown");

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
      execArgv: ["--experimental-strip-types"],
    } as unknown as WorkerOptions);

    let exited = false;
    try {
      await withTimeout(once(worker, "message") as Promise<[any]>, 2000, "server worker ready");

      const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
      const cmdBytes = encodeCommand({ kind: "portWrite", id: 1, port: 0, size: 1, value: 0 });

      // Start with a full ring so the server immediately enters the drain loop.
      while (cmdQ.tryPush(cmdBytes)) {}

      const end = nowMs() + 50;
      while (nowMs() < end) {
        // Keep the ring non-empty by pushing whenever space becomes available.
        // Batch pushes so time checks don't dominate and the producer stays ahead.
        for (let i = 0; i < 1024; i++) cmdQ.tryPush(cmdBytes);
      }

      expect(Atomics.load(tickCounter, 0)).toBeGreaterThan(0);
      expect(Atomics.load(tickSawCmdData, 0)).toBe(1);

      // Gracefully stop the blocking server loop so the worker can exit cleanly.
      const shutdownBytes = encodeCommand({ kind: "shutdown" });
      while (!cmdQ.tryPush(shutdownBytes)) {
        await sleep0();
      }
      await withTimeout(once(worker, "exit") as Promise<[number]>, 4000, "server worker exit");
      exited = true;
    } finally {
      if (!exited) await worker.terminate();
    }
  });
});

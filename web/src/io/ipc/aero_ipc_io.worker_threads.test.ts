import { describe, expect, it } from "vitest";

import { once } from "node:events";
import { Worker } from "node:worker_threads";

import { createIpcBuffer } from "../../ipc/ipc.ts";
import { queueKind } from "../../ipc/layout.ts";

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  return Promise.race([
    promise,
    new Promise<T>((_resolve, reject) => {
      setTimeout(() => reject(new Error(`timed out after ${timeoutMs}ms: ${label}`)), timeoutMs);
    }),
  ]);
}

describe("io/ipc/aero_ipc_io (worker_threads)", () => {
  it("completes a portRead/portWrite roundtrip over a shared AIPC buffer", async () => {
    const { buffer: ipcBuffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);

    const ioWorker = new Worker(new URL("./test_workers/aipc_io_server_worker.ts", import.meta.url), {
      type: "module",
      workerData: { ipcBuffer, tickIntervalMs: 1 },
      execArgv: ["--experimental-strip-types"],
    });

    const cpuWorker = new Worker(new URL("./test_workers/aipc_cpu_roundtrip_worker.ts", import.meta.url), {
      type: "module",
      workerData: { ipcBuffer },
      execArgv: ["--experimental-strip-types"],
    });

    try {
      const [result] = (await withTimeout(once(cpuWorker, "message") as Promise<[any]>, 2000, "cpu worker result")) as [
        { ok: boolean; status64?: number; cmdByte?: number; error?: string },
      ];

      expect(result.ok).toBe(true);
      expect(result.error).toBeUndefined();
      // i8042 starts with STATUS_SYS (bit2) set and output buffer empty.
      expect((result.status64 ?? 0) & 0x04).toBe(0x04);
      // Default command byte is 0x00.
      expect(result.cmdByte).toBe(0x00);
    } finally {
      await cpuWorker.terminate();
      await ioWorker.terminate();
    }
  });
});


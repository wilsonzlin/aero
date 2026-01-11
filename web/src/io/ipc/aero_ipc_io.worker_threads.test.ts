import { describe, expect, it } from "vitest";

import { once } from "node:events";
import { Worker, type WorkerOptions } from "node:worker_threads";

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
    } as unknown as WorkerOptions);

    const cpuWorker = new Worker(new URL("./test_workers/aipc_cpu_roundtrip_worker.ts", import.meta.url), {
      type: "module",
      workerData: { ipcBuffer },
      execArgv: ["--experimental-strip-types"],
    } as unknown as WorkerOptions);

    try {
      const [result] = (await withTimeout(once(cpuWorker, "message") as Promise<[any]>, 4000, "cpu worker result")) as [
        {
          ok: boolean;
          status64?: number;
          cmdByte?: number;
          kbd?: number[];
          irqEvents?: Array<{ irq: number; level: boolean }>;
          a20Events?: boolean[];
          resetRequests?: number;
          serialBytes?: number[];
          mmio0?: number;
          error?: string;
        },
      ];

      expect(result.ok).toBe(true);
      expect(result.error).toBeUndefined();
      // i8042 starts with STATUS_SYS (bit2) set and output buffer empty.
      expect((result.status64 ?? 0) & 0x04).toBe(0x04);
      // Default command byte is 0x00.
      expect(result.cmdByte).toBe(0x00);

      // Keyboard reset command (0xFF) should return ACK (0xFA) + self-test pass (0xAA).
      expect(result.kbd).toEqual([0xfa, 0xaa]);
      // IRQ1 should pulse high while keyboard bytes are pending.
      expect(result.irqEvents).toEqual([
        { irq: 1, level: true },
        { irq: 1, level: false },
      ]);
      // A20 gate should be enabled when writing output port 0x03.
      expect(result.a20Events).toEqual([true]);
      // Reset request should be surfaced exactly once.
      expect(result.resetRequests).toBe(1);
      // UART serial output should contain "Hi".
      expect(result.serialBytes).toEqual([0x48, 0x69]);
      // MMIO write/read roundtrip.
      expect(result.mmio0).toBe(0x1234_5678);
    } finally {
      await cpuWorker.terminate();
      await ioWorker.terminate();
    }
  });
});

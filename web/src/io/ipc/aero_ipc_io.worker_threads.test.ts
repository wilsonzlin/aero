import { describe, expect, it } from "vitest";

import { once } from "node:events";
import { Worker, type WorkerOptions } from "node:worker_threads";

import { createIpcBuffer } from "../../ipc/ipc.ts";
import { queueKind } from "../../ipc/layout.ts";
import { makeNodeWorkerExecArgv } from "../../test_utils/worker_threads_exec_argv";
import { unrefBestEffort } from "../../unrefSafe";

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

describe("io/ipc/aero_ipc_io (worker_threads)", () => {
  it("completes a portRead/portWrite roundtrip over a shared AIPC buffer", async () => {
    const { buffer: ipcBuffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);
    const vram = new SharedArrayBuffer(0x1000);
    const workerExecArgv = makeNodeWorkerExecArgv();

    const ioWorker = new Worker(new URL("./test_workers/aipc_io_server_worker.ts", import.meta.url), {
      type: "module",
      workerData: { ipcBuffer, tickIntervalMs: 1, vram },
      execArgv: workerExecArgv,
    } as unknown as WorkerOptions);

    const cpuWorker = new Worker(new URL("./test_workers/aipc_cpu_roundtrip_worker.ts", import.meta.url), {
      type: "module",
      workerData: { ipcBuffer, vram },
      execArgv: workerExecArgv,
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
          pciVendorId?: number;
          pciDeviceId?: number;
           pciBar0?: number;
           pciMmio0?: number;
           vramMmio?: number;
           vramBytes?: number[] | null;
           error?: string;
         },
       ];

      expect(result.ok).toBe(true);
      expect(result.error).toBeUndefined();
      // i8042 starts with STATUS_SYS (bit2) set and output buffer empty.
      expect((result.status64 ?? 0) & 0x04).toBe(0x04);
      // Default command byte matches the canonical Rust model: 0x45.
      expect(result.cmdByte).toBe(0x45);

      // Keyboard reset command (0xFF) should return ACK (0xFA) + self-test pass (0xAA).
      expect(result.kbd).toEqual([0xfa, 0xaa]);
      // IRQ1 is edge-triggered: it should pulse once per output byte becoming available.
      expect(result.irqEvents).toEqual([
        { irq: 1, level: true },
        { irq: 1, level: false },
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

      // PCI test device config + BAR0 MMIO mapping.
      expect(result.pciVendorId).toBe(0x1234);
      expect(result.pciDeviceId).toBe(0x5678);
      expect((result.pciBar0 ?? 0) & 0xf).toBe(0); // BAR0 is mmio32 so low 4 bits are 0.
      expect(result.pciMmio0).toBe(0xcafe_babe);

      // VRAM-backed BAR1-style MMIO range.
      expect(result.vramMmio).toBe(0xdead_beef);
      expect(result.vramBytes).toEqual([0xef, 0xbe, 0xad, 0xde]);
    } finally {
      await cpuWorker.terminate();
      await ioWorker.terminate();
    }
  }, 15000);
});

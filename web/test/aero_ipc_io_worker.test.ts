import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { alignUp, ringCtrl } from "../src/ipc/layout.ts";
import { PCI_MMIO_BASE } from "../src/arch/guest_phys.ts";
import { WORKER_EXEC_ARGV } from "./_helpers/worker_exec_argv.ts";

function createCmdEvtSharedBuffer(cmdCapBytes: number, evtCapBytes: number): { sab: SharedArrayBuffer; cmdOffset: number; evtOffset: number } {
  const cmdOffset = 0;
  const evtOffset = alignUp(cmdOffset + ringCtrl.BYTES + cmdCapBytes, 4);
  const totalBytes = evtOffset + ringCtrl.BYTES + evtCapBytes;

  const sab = new SharedArrayBuffer(totalBytes);
  new Int32Array(sab, cmdOffset, ringCtrl.WORDS).set([0, 0, 0, cmdCapBytes]);
  new Int32Array(sab, evtOffset, ringCtrl.WORDS).set([0, 0, 0, evtCapBytes]);

  return { sab, cmdOffset, evtOffset };
}

async function terminateWorkers(workers: Worker[]): Promise<void> {
  for (const w of workers) w.unref();
  const done = Promise.allSettled(workers.map((w) => w.terminate()));
  await Promise.race([done, new Promise<void>((resolve) => setTimeout(resolve, 2000))]);
}

test("AIPC I/O worker: i8042 port I/O + IRQ signalling", async () => {
  const { sab, cmdOffset, evtOffset } = createCmdEvtSharedBuffer(1 << 16, 1 << 16);

  const ioWorker = new Worker(new URL("./workers/aero_ipc_io_server_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, devices: ["i8042"], tickIntervalMs: 1 },
    execArgv: WORKER_EXEC_ARGV,
  });

  const cpuWorker = new Worker(new URL("./workers/aero_ipc_cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, scenario: "i8042" },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      {
        ok: boolean;
        statusBefore: number;
        statusMid: number;
        statusAfter: number;
        bytes: number[];
        irqEvents: Array<{ irq: number; level: boolean }>;
      },
    ];

    assert.equal(result.ok, true);
    assert.deepEqual(result.bytes, [0xfa, 0xaa]);
    assert.equal(result.statusBefore & 0x01, 0x01);
    assert.equal(result.statusMid & 0x01, 0x01);
    assert.equal(result.statusAfter & 0x01, 0x00);
    // i8042 IRQs are modeled as edge-triggered pulses for the legacy PIC: one pulse per byte
    // loaded into the output buffer.
    assert.deepEqual(result.irqEvents, [
      { irq: 1, level: true },
      { irq: 1, level: false },
      { irq: 1, level: true },
      { irq: 1, level: false },
    ]);
  } finally {
    await terminateWorkers([cpuWorker, ioWorker]);
  }
});

test("AIPC I/O worker: i8042 output port toggles A20 + requests reset", async () => {
  const { sab, cmdOffset, evtOffset } = createCmdEvtSharedBuffer(1 << 16, 1 << 16);

  const ioWorker = new Worker(new URL("./workers/aero_ipc_io_server_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, devices: ["i8042"], tickIntervalMs: 1 },
    execArgv: WORKER_EXEC_ARGV,
  });

  const cpuWorker = new Worker(new URL("./workers/aero_ipc_cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, scenario: "i8042_output_port" },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      {
        ok: boolean;
        outPort: number;
        a20Events: boolean[];
        resetRequests: number;
        irqEvents: Array<{ irq: number; level: boolean }>;
      },
    ];

    assert.equal(result.ok, true);
    assert.deepEqual(result.a20Events, [true, false]);
    assert.equal(result.resetRequests, 1);
    assert.equal(result.outPort, 0xa9);
    assert.deepEqual(result.irqEvents, []);
  } finally {
    await terminateWorkers([cpuWorker, ioWorker]);
  }
});

test("AIPC I/O worker: PCI config + BAR-backed MMIO dispatch", async () => {
  const { sab, cmdOffset, evtOffset } = createCmdEvtSharedBuffer(1 << 17, 1 << 17);

  const ioWorker = new Worker(new URL("./workers/aero_ipc_io_server_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, devices: ["pci_test"], tickIntervalMs: 1 },
    execArgv: WORKER_EXEC_ARGV,
  });

  const cpuWorker = new Worker(new URL("./workers/aero_ipc_cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, scenario: "pci_test" },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      { ok: boolean; idDword: number; bar0: number; mmioReadback: number },
    ];

    assert.equal(result.ok, true);
    assert.equal(result.idDword >>> 0, 0x5678_1234);
    assert.equal(result.bar0 >>> 0, PCI_MMIO_BASE);
    assert.equal(result.mmioReadback >>> 0, 0x1234_5678);
  } finally {
    await terminateWorkers([cpuWorker, ioWorker]);
  }
});

test("AIPC I/O worker: 16550 UART emits serial output bytes", async () => {
  const { sab, cmdOffset, evtOffset } = createCmdEvtSharedBuffer(1 << 16, 1 << 16);

  const ioWorker = new Worker(new URL("./workers/aero_ipc_io_server_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, devices: ["uart16550"], tickIntervalMs: 1 },
    execArgv: WORKER_EXEC_ARGV,
  });

  const cpuWorker = new Worker(new URL("./workers/aero_ipc_cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, scenario: "uart16550" },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      { ok: boolean; lsrBefore: number; lsrAfter: number; serialBytes: number[] },
    ];

    assert.equal(result.ok, true);
    assert.equal(result.lsrBefore & 0x60, 0x60);
    assert.equal(result.lsrAfter & 0x60, 0x60);
    assert.deepEqual(result.serialBytes, [0x48, 0x69]);
  } finally {
    await terminateWorkers([cpuWorker, ioWorker]);
  }
});

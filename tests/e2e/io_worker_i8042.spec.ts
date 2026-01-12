import { expect, test } from "@playwright/test";

import { PCI_MMIO_BASE } from "../../web/src/arch/guest_phys";

test("CPU↔IO AIPC: i8042 port I/O roundtrip in browser workers", async ({ page }) => {
  await page.goto("http://127.0.0.1:5173/", { waitUntil: "load" });

  const result = await page.evaluate(async () => {
    if (!globalThis.crossOriginIsolated || typeof SharedArrayBuffer === "undefined") {
      throw new Error("test requires crossOriginIsolated + SharedArrayBuffer");
    }

    const { createIpcBuffer } = await import("/web/src/ipc/ipc.ts");
    const { queueKind } = await import("/web/src/ipc/layout.ts");

    const { buffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);

    const cpuWorkerCode = `
      import { openRingByKind } from "${location.origin}/web/src/ipc/ipc.ts";
      import { queueKind } from "${location.origin}/web/src/ipc/layout.ts";
      import { AeroIpcIoClient } from "${location.origin}/web/src/io/ipc/aero_ipc_io.ts";

      self.onmessage = (ev) => {
        const { ipcBuffer } = ev.data;
        const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
        const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

        const irqEvents = [];
        const io = new AeroIpcIoClient(cmdQ, evtQ, {
          onIrq: (irq, level) => irqEvents.push({ irq, level }),
        });

        io.portWrite(0x64, 1, 0x60);
        io.portWrite(0x60, 1, 0x01);
        io.portWrite(0x60, 1, 0xff);

        const statusBefore = io.portRead(0x64, 1);
        const b0 = io.portRead(0x60, 1);
        const statusMid = io.portRead(0x64, 1);
        const b1 = io.portRead(0x60, 1);
        const statusAfter = io.portRead(0x64, 1);

        self.postMessage({ statusBefore, statusMid, statusAfter, bytes: [b0, b1], irqEvents });
      };
    `;

    const cpuUrl = URL.createObjectURL(new Blob([cpuWorkerCode], { type: "text/javascript" }));

    const ioWorker = new Worker(new URL("/web/src/workers/io_aipc.worker.ts", location.href), { type: "module" });
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    // Note: revoking immediately is safe once the Worker has fetched the script,
    // but keep it simple and revoke after completion.

    ioWorker.postMessage({ type: "init", ipcBuffer: buffer, devices: ["i8042"], tickIntervalMs: 1 });
    cpuWorker.postMessage({ ipcBuffer: buffer });

    const cpuResult = await Promise.race([
      new Promise((resolve, reject) => {
        cpuWorker.onmessage = (ev) => resolve(ev.data);
        cpuWorker.onerror = (err) => reject(err);
      }),
      new Promise((_, reject) => setTimeout(() => reject(new Error("timeout waiting for CPU result")), 2000)),
    ]);

    cpuWorker.terminate();
    ioWorker.terminate();
    URL.revokeObjectURL(cpuUrl);

    return cpuResult as {
      statusBefore: number;
      statusMid: number;
      statusAfter: number;
      bytes: number[];
      irqEvents: Array<{ irq: number; level: boolean }>;
    };
  });

  expect(result.bytes).toEqual([0xfa, 0xaa]);
  expect(result.statusBefore & 0x01).toBe(0x01);
  expect(result.statusMid & 0x01).toBe(0x01);
  expect(result.statusAfter & 0x01).toBe(0x00);
  expect(result.irqEvents).toEqual([
    // i8042 models IRQ1 as an edge-triggered pulse each time the head output byte changes.
    // Reset returns two bytes (ACK + BAT OK), so we expect two pulses.
    { irq: 1, level: true },
    { irq: 1, level: false },
    { irq: 1, level: true },
    { irq: 1, level: false },
  ]);
});

test("CPU↔IO AIPC: PCI config + BAR-backed MMIO dispatch in browser workers", async ({ page }) => {
  await page.goto("http://127.0.0.1:5173/", { waitUntil: "load" });

  const result = await page.evaluate(async () => {
    if (!globalThis.crossOriginIsolated || typeof SharedArrayBuffer === "undefined") {
      throw new Error("test requires crossOriginIsolated + SharedArrayBuffer");
    }

    const { createIpcBuffer } = await import("/web/src/ipc/ipc.ts");
    const { queueKind } = await import("/web/src/ipc/layout.ts");

    const { buffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 17 },
      { kind: queueKind.EVT, capacityBytes: 1 << 17 },
    ]);

    const cpuWorkerCode = `
      import { openRingByKind } from "${location.origin}/web/src/ipc/ipc.ts";
      import { queueKind } from "${location.origin}/web/src/ipc/layout.ts";
      import { AeroIpcIoClient } from "${location.origin}/web/src/io/ipc/aero_ipc_io.ts";

      self.onmessage = (ev) => {
        const { ipcBuffer } = ev.data;
        const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
        const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

        const io = new AeroIpcIoClient(cmdQ, evtQ);

        io.portWrite(0x0cf8, 4, 0x8000_0000);
        const idDword = io.portRead(0x0cfc, 4);

        io.portWrite(0x0cf8, 4, 0x8000_0010);
        const bar0 = io.portRead(0x0cfc, 4);

        // Enable memory space decoding (PCI command bit1) so the BAR-backed MMIO region is active.
        io.portWrite(0x0cf8, 4, 0x8000_0004);
        io.portWrite(0x0cfc, 2, 0x0002);

        const base = BigInt(bar0 >>> 0);
        io.mmioWrite(base + 0n, 4, 0x1234_5678);
        const mmioReadback = io.mmioRead(base + 0n, 4);

        self.postMessage({ idDword, bar0, mmioReadback });
      };
    `;

    const cpuUrl = URL.createObjectURL(new Blob([cpuWorkerCode], { type: "text/javascript" }));

    const ioWorker = new Worker(new URL("/web/src/workers/io_aipc.worker.ts", location.href), { type: "module" });
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    ioWorker.postMessage({ type: "init", ipcBuffer: buffer, devices: ["pci_test"], tickIntervalMs: 1 });
    cpuWorker.postMessage({ ipcBuffer: buffer });

    const cpuResult = await Promise.race([
      new Promise((resolve, reject) => {
        cpuWorker.onmessage = (ev) => resolve(ev.data);
        cpuWorker.onerror = (err) => reject(err);
      }),
      new Promise((_, reject) => setTimeout(() => reject(new Error("timeout waiting for CPU result")), 2000)),
    ]);

    cpuWorker.terminate();
    ioWorker.terminate();
    URL.revokeObjectURL(cpuUrl);

    return cpuResult as { idDword: number; bar0: number; mmioReadback: number };
  });

  expect(result.idDword >>> 0).toBe(0x5678_1234);
  expect(result.bar0 >>> 0).toBe(PCI_MMIO_BASE);
  expect(result.mmioReadback >>> 0).toBe(0x1234_5678);
});

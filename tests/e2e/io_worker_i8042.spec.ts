import { expect, test } from "@playwright/test";

test("CPU↔IO AIPC: i8042 port I/O roundtrip in browser workers", async ({ page }) => {
  await page.goto("http://127.0.0.1:5173/", { waitUntil: "load" });

  const result = await page.evaluate(async () => {
    if (!globalThis.crossOriginIsolated || typeof SharedArrayBuffer === "undefined") {
      throw new Error("test requires crossOriginIsolated + SharedArrayBuffer");
    }

    const { alignUp, ringCtrl } = await import("/web/src/ipc/layout.ts");

    const CMD_CAP = 1 << 16;
    const EVT_CAP = 1 << 16;

    const cmdOffset = 0;
    const evtOffset = alignUp(cmdOffset + ringCtrl.BYTES + CMD_CAP, 4);
    const totalBytes = evtOffset + ringCtrl.BYTES + EVT_CAP;

    const sab = new SharedArrayBuffer(totalBytes);
    new Int32Array(sab, cmdOffset, ringCtrl.WORDS).set([0, 0, 0, CMD_CAP]);
    new Int32Array(sab, evtOffset, ringCtrl.WORDS).set([0, 0, 0, EVT_CAP]);

    const ioWorkerCode = `
      import { RingBuffer } from "/web/src/ipc/ring_buffer.ts";
      import { encodeEvent } from "/web/src/ipc/protocol.ts";
      import { DeviceManager } from "/web/src/io/device_manager.ts";
      import { I8042Controller } from "/web/src/io/devices/i8042.ts";
      import { AeroIpcIoServer } from "/web/src/io/ipc/aero_ipc_io.ts";

      self.onmessage = (ev) => {
        const { sab, cmdOffset, evtOffset, tickIntervalMs } = ev.data;
        const cmdQ = new RingBuffer(sab, cmdOffset);
        const evtQ = new RingBuffer(sab, evtOffset);

        const irqSink = {
          raiseIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: irq & 0xff })),
          lowerIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: irq & 0xff })),
        };

        const mgr = new DeviceManager(irqSink);
        const i8042 = new I8042Controller(mgr.irqSink);
        mgr.registerPortIo(0x0060, 0x0060, i8042);
        mgr.registerPortIo(0x0064, 0x0064, i8042);

        new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();
      };
    `;

    const cpuWorkerCode = `
      import { RingBuffer } from "/web/src/ipc/ring_buffer.ts";
      import { AeroIpcIoClient } from "/web/src/io/ipc/aero_ipc_io.ts";

      self.onmessage = (ev) => {
        const { sab, cmdOffset, evtOffset } = ev.data;
        const cmdQ = new RingBuffer(sab, cmdOffset);
        const evtQ = new RingBuffer(sab, evtOffset);

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

    const ioUrl = URL.createObjectURL(new Blob([ioWorkerCode], { type: "text/javascript" }));
    const cpuUrl = URL.createObjectURL(new Blob([cpuWorkerCode], { type: "text/javascript" }));

    const ioWorker = new Worker(ioUrl, { type: "module" });
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    // Note: revoking immediately is safe once the Worker has fetched the script,
    // but keep it simple and revoke after completion.

    ioWorker.postMessage({ sab, cmdOffset, evtOffset, tickIntervalMs: 1 });
    cpuWorker.postMessage({ sab, cmdOffset, evtOffset });

    const cpuResult = await Promise.race([
      new Promise((resolve, reject) => {
        cpuWorker.onmessage = (ev) => resolve(ev.data);
        cpuWorker.onerror = (err) => reject(err);
      }),
      new Promise((_, reject) => setTimeout(() => reject(new Error("timeout waiting for CPU result")), 2000)),
    ]);

    cpuWorker.terminate();
    ioWorker.terminate();
    URL.revokeObjectURL(ioUrl);
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
    { irq: 1, level: true },
    { irq: 1, level: false },
  ]);
});


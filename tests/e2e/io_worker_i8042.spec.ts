import { expect, test } from "@playwright/test";

import { PCI_MMIO_BASE } from "../../web/src/arch/guest_phys";

test("CPU↔IO AIPC: i8042 port I/O roundtrip in browser workers", async ({ page }) => {
  await page.goto("/", { waitUntil: "load" });

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

    // WebKit can fail to load large module workers directly via `new Worker(httpUrl, { type: "module" })`
    // (it emits an `error` event without useful details). Wrap the module entrypoint in a tiny
    // blob-based module worker and import the real worker from there for cross-browser stability.
    const ioWorkerEntrypoint = new URL("/web/src/workers/io_aipc.worker.ts", location.href).toString();
    const ioWorkerWrapperUrl = URL.createObjectURL(
      new Blob(
        [
          `\n            (async () => {\n              const MAX_ERROR_CHARS = 512;\n              const fallbackFormatErr = (err) => {\n                const msg = err instanceof Error ? err.message : err;\n                return String(msg ?? \"Error\")\n                  .replace(/[\\x00-\\x1F\\x7F]/g, \" \")\n                  .replace(/\\s+/g, \" \")\n                  .trim()\n                  .slice(0, MAX_ERROR_CHARS);\n              };\n\n              let formatErr = fallbackFormatErr;\n              try {\n                const mod = await import(\"/web/src/text.ts\");\n                const formatOneLineUtf8 = mod?.formatOneLineUtf8;\n                if (typeof formatOneLineUtf8 === \"function\") {\n                  formatErr = (err) => {\n                    const msg = err instanceof Error ? err.message : err;\n                    return formatOneLineUtf8(String(msg ?? \"\"), 512) || \"Error\";\n                  };\n                }\n              } catch {\n                // ignore: keep fallbackFormatErr\n              }\n\n              try {\n                await import(${JSON.stringify(ioWorkerEntrypoint)});\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_imported\" }), 0);\n              } catch (err) {\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_import_failed\", message: formatErr(err) }), 0);\n              }\n            })();\n          `,
        ],
        { type: "text/javascript" },
      ),
    );
    const ioWorker = new Worker(ioWorkerWrapperUrl, { type: "module" });
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    // Note: revoking immediately is safe once the Worker has fetched the script,
    // but keep it simple and revoke after completion.

    // Avoid dropping early messages on WebKit by waiting until the imported worker module has run.
    await new Promise<void>((resolve, reject) => {
      let timer = 0;
      const cleanup = () => {
        if (timer) clearTimeout(timer);
        ioWorker.removeEventListener("message", messageHandler);
        ioWorker.removeEventListener("error", errorHandler);
        ioWorker.removeEventListener("messageerror", messageErrorHandler);
      };

      const messageHandler = (ev: MessageEvent): void => {
        const data = ev.data as { type?: unknown; message?: unknown } | undefined;
        if (!data) return;
        if (data.type === "__aero_io_worker_imported") {
          cleanup();
          resolve();
          return;
        }
        if (data.type === "__aero_io_worker_import_failed") {
          cleanup();
          reject(
            new Error(`io_aipc.worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`),
          );
        }
      };
      const errorHandler = (err: Event) => {
        cleanup();
        const e = err as any;
        const message =
          typeof e?.message === "string"
            ? e.message
            : typeof e?.error?.message === "string"
              ? e.error.message
              : String(err);
        const filename = typeof e?.filename === "string" ? e.filename : "?";
        const lineno = typeof e?.lineno === "number" ? e.lineno : "?";
        const colno = typeof e?.colno === "number" ? e.colno : "?";
        reject(new Error(`io_aipc.worker wrapper error during import: ${message} (${filename}:${lineno}:${colno})`));
      };
      const messageErrorHandler = () => {
        cleanup();
        reject(new Error("io_aipc.worker wrapper messageerror during import"));
      };

      ioWorker.addEventListener("message", messageHandler);
      ioWorker.addEventListener("error", errorHandler);
      ioWorker.addEventListener("messageerror", messageErrorHandler);
      timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for io_aipc.worker import marker"));
      }, 20_000);
      (timer as unknown as { unref?: () => void }).unref?.();
    });

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
    URL.revokeObjectURL(ioWorkerWrapperUrl);

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
  await page.goto("/", { waitUntil: "load" });

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

    // WebKit can fail to load large module workers directly via `new Worker(httpUrl, { type: "module" })`
    // (it emits an `error` event without useful details). Wrap the module entrypoint in a tiny
    // blob-based module worker and import the real worker from there for cross-browser stability.
    const ioWorkerEntrypoint = new URL("/web/src/workers/io_aipc.worker.ts", location.href).toString();
    const ioWorkerWrapperUrl = URL.createObjectURL(
      new Blob(
        [
          `\n            (async () => {\n              const MAX_ERROR_CHARS = 512;\n              const fallbackFormatErr = (err) => {\n                const msg = err instanceof Error ? err.message : err;\n                return String(msg ?? \"Error\")\n                  .replace(/[\\x00-\\x1F\\x7F]/g, \" \")\n                  .replace(/\\s+/g, \" \")\n                  .trim()\n                  .slice(0, MAX_ERROR_CHARS);\n              };\n\n              let formatErr = fallbackFormatErr;\n              try {\n                const mod = await import(\"/web/src/text.ts\");\n                const formatOneLineUtf8 = mod?.formatOneLineUtf8;\n                if (typeof formatOneLineUtf8 === \"function\") {\n                  formatErr = (err) => {\n                    const msg = err instanceof Error ? err.message : err;\n                    return formatOneLineUtf8(String(msg ?? \"\"), 512) || \"Error\";\n                  };\n                }\n              } catch {\n                // ignore: keep fallbackFormatErr\n              }\n\n              try {\n                await import(${JSON.stringify(ioWorkerEntrypoint)});\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_imported\" }), 0);\n              } catch (err) {\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_import_failed\", message: formatErr(err) }), 0);\n              }\n            })();\n          `,
        ],
        { type: "text/javascript" },
      ),
    );
    const ioWorker = new Worker(ioWorkerWrapperUrl, { type: "module" });
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    // Avoid dropping early messages on WebKit by waiting until the imported worker module has run.
    await new Promise<void>((resolve, reject) => {
      let timer = 0;
      const cleanup = () => {
        if (timer) clearTimeout(timer);
        ioWorker.removeEventListener("message", messageHandler);
        ioWorker.removeEventListener("error", errorHandler);
        ioWorker.removeEventListener("messageerror", messageErrorHandler);
      };

      const messageHandler = (ev: MessageEvent): void => {
        const data = ev.data as { type?: unknown; message?: unknown } | undefined;
        if (!data) return;
        if (data.type === "__aero_io_worker_imported") {
          cleanup();
          resolve();
          return;
        }
        if (data.type === "__aero_io_worker_import_failed") {
          cleanup();
          reject(
            new Error(`io_aipc.worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`),
          );
        }
      };
      const errorHandler = (err: Event) => {
        cleanup();
        const e = err as any;
        const message =
          typeof e?.message === "string"
            ? e.message
            : typeof e?.error?.message === "string"
              ? e.error.message
              : String(err);
        const filename = typeof e?.filename === "string" ? e.filename : "?";
        const lineno = typeof e?.lineno === "number" ? e.lineno : "?";
        const colno = typeof e?.colno === "number" ? e.colno : "?";
        reject(new Error(`io_aipc.worker wrapper error during import: ${message} (${filename}:${lineno}:${colno})`));
      };
      const messageErrorHandler = () => {
        cleanup();
        reject(new Error("io_aipc.worker wrapper messageerror during import"));
      };

      ioWorker.addEventListener("message", messageHandler);
      ioWorker.addEventListener("error", errorHandler);
      ioWorker.addEventListener("messageerror", messageErrorHandler);
      timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for io_aipc.worker import marker"));
      }, 20_000);
      (timer as unknown as { unref?: () => void }).unref?.();
    });

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
    URL.revokeObjectURL(ioWorkerWrapperUrl);

    return cpuResult as { idDword: number; bar0: number; mmioReadback: number };
  });

  expect(result.idDword >>> 0).toBe(0x5678_1234);
  expect(result.bar0 >>> 0).toBe(PCI_MMIO_BASE);
  expect(result.mmioReadback >>> 0).toBe(0x1234_5678);
});

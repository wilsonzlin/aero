import { expect, test } from "@playwright/test";

test("PCI bus: multifunction + mmio64 BAR + capability list", async ({ page }) => {
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

      const PCI_ADDR = 0x0cf8;
      const PCI_DATA = 0x0cfc;

      function pciAddr(bus, dev, func, reg) {
        return (0x80000000 | ((bus & 0xff) << 16) | ((dev & 0x1f) << 11) | ((func & 0x07) << 8) | (reg & 0xfc)) >>> 0;
      }

      function pciRead32(io, bus, dev, func, reg) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, reg));
        return io.portRead(PCI_DATA, 4) >>> 0;
      }

      function pciWrite32(io, bus, dev, func, reg, value) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, reg));
        io.portWrite(PCI_DATA, 4, value >>> 0);
      }

      function pciRead8(io, bus, dev, func, off) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, off));
        return io.portRead(PCI_DATA + (off & 3), 1) & 0xff;
      }

      function pciRead16(io, bus, dev, func, off) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, off));
        return io.portRead(PCI_DATA + (off & 2), 2) & 0xffff;
      }

      self.onmessage = (ev) => {
        const { ipcBuffer } = ev.data;
        const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
        const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

        const io = new AeroIpcIoClient(cmdQ, evtQ);

        // IDs for fn0/fn1.
        const idFn0 = pciRead32(io, 0, 0, 0, 0x00);
        const idFn1 = pciRead32(io, 0, 0, 1, 0x00);

        // Header type on fn0 should advertise multi-function.
        const headerTypeFn0 = pciRead8(io, 0, 0, 0, 0x0e);

        // Subsystem IDs.
        const subsysFn0 = pciRead32(io, 0, 0, 0, 0x2c);
        const subsysFn1 = pciRead32(io, 0, 0, 1, 0x2c);

        // 64-bit BAR0 (BAR0 low dword at 0x10, BAR1 high dword at 0x14).
        const bar0Lo = pciRead32(io, 0, 0, 0, 0x10);
        const bar0Hi = pciRead32(io, 0, 0, 0, 0x14);

        // Size probe for 64-bit BAR.
        pciWrite32(io, 0, 0, 0, 0x10, 0xffff_ffff);
        pciWrite32(io, 0, 0, 0, 0x14, 0xffff_ffff);
        const bar0MaskLo = pciRead32(io, 0, 0, 0, 0x10);
        const bar0MaskHi = pciRead32(io, 0, 0, 0, 0x14);

        // Program a >4GiB base to validate 64-bit BAR encoding (including BAR1 high dword)
        // and ensure mmio dispatch works for 64-bit addresses.
        pciWrite32(io, 0, 0, 0, 0x10, 0x0000_0000);
        pciWrite32(io, 0, 0, 0, 0x14, 0x0000_0001);
        const bar0LoSet = pciRead32(io, 0, 0, 0, 0x10);
        const bar0HiSet = pciRead32(io, 0, 0, 0, 0x14);

        // Capability list plumbing.
        const cmdStatus = pciRead32(io, 0, 0, 0, 0x04);
        const status = (cmdStatus >>> 16) & 0xffff;
        const capPtr = pciRead8(io, 0, 0, 0, 0x34);
        const cap0Id = pciRead8(io, 0, 0, 0, capPtr);
        const cap0Next = pciRead8(io, 0, 0, 0, capPtr + 1);
        const cap1Id = pciRead8(io, 0, 0, 0, cap0Next);
        const cap1Next = pciRead8(io, 0, 0, 0, cap0Next + 1);

        // Enable memory decoding and validate that mmio64 mapping works at the 64-bit base.
        io.portWrite(PCI_ADDR, 4, pciAddr(0, 0, 0, 0x04));
        io.portWrite(PCI_DATA, 2, 0x0002);

        const mmioBase = (BigInt(bar0HiSet >>> 0) << 32n) | BigInt(bar0LoSet & 0xffff_fff0);
        io.mmioWrite(mmioBase + 0n, 4, 0x1234_5678);
        const mmioReadback = io.mmioRead(mmioBase + 0n, 4);

        self.postMessage({
          idFn0,
          idFn1,
          headerTypeFn0,
          subsysFn0,
          subsysFn1,
          bar0Lo,
          bar0Hi,
          bar0LoSet,
          bar0HiSet,
          bar0MaskLo,
          bar0MaskHi,
          status,
          capPtr,
          cap0Id,
          cap0Next,
          cap1Id,
          cap1Next,
          mmioReadback,
        });
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
      const timer = setTimeout(() => reject(new Error("Timed out waiting for io_aipc.worker import marker")), 5000);
      const handler = (ev: MessageEvent): void => {
        const data = ev.data as { type?: unknown; message?: unknown } | undefined;
        if (!data) return;
        if (data.type === "__aero_io_worker_imported") {
          clearTimeout(timer);
          ioWorker.removeEventListener("message", handler);
          resolve();
          return;
        }
        if (data.type === "__aero_io_worker_import_failed") {
          clearTimeout(timer);
          ioWorker.removeEventListener("message", handler);
          reject(
            new Error(`io_aipc.worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`),
          );
        }
      };
      ioWorker.addEventListener("message", handler);
    });

    ioWorker.postMessage({ type: "init", ipcBuffer: buffer, devices: ["pci_multifn_test"], tickIntervalMs: 1 });
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
      idFn0: number;
      idFn1: number;
      headerTypeFn0: number;
      subsysFn0: number;
      subsysFn1: number;
      bar0Lo: number;
      bar0Hi: number;
      bar0LoSet: number;
      bar0HiSet: number;
      bar0MaskLo: number;
      bar0MaskHi: number;
      status: number;
      capPtr: number;
      cap0Id: number;
      cap0Next: number;
      cap1Id: number;
      cap1Next: number;
      mmioReadback: number;
    };
  });

  expect(result.idFn0 >>> 0).toBe(0x1052_1af4);
  expect(result.idFn1 >>> 0).toBe(0x1052_1af4);

  // Function 0 must advertise multifunction (bit7).
  expect(result.headerTypeFn0 & 0x80).toBe(0x80);

  // Subsystem IDs are per-function.
  expect(result.subsysFn0 >>> 0).toBe(0x0010_1af4);
  expect(result.subsysFn1 >>> 0).toBe(0x0011_1af4);

  // MMIO64 BAR encoding (type bits + high dword) after programming a >4GiB base.
  expect(result.bar0LoSet >>> 0).toBe(0x0000_0004);
  expect(result.bar0HiSet >>> 0).toBe(0x0000_0001);

  // 64-bit BAR size probe semantics for size=0x4000:
  // mask = 0xffff_ffff_ffff_c000; low dword preserves the 64-bit type bits (0x4).
  expect(result.bar0MaskLo >>> 0).toBe(0xffff_c004);
  expect(result.bar0MaskHi >>> 0).toBe(0xffff_ffff);

  // PCI capability list: status bit4 + pointer at 0x34 + linked list next pointers.
  expect(result.status & 0x10).toBe(0x10);
  expect(result.capPtr).toBe(0x40);
  expect(result.cap0Id).toBe(0x09);
  expect(result.cap0Next).toBe(0x50);
  expect(result.cap1Id).toBe(0x09);
  expect(result.cap1Next).toBe(0x00);

  // BAR-backed MMIO dispatch should work at a 64-bit base.
  expect(result.mmioReadback >>> 0).toBe(0x1234_5678);
});

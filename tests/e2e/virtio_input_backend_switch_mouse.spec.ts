import { expect, test } from "@playwright/test";

import { checkThreadedWasmBundle } from "./util/wasm_bundle";

test(
  "IO worker switches mouse input from PS/2 (i8042 AUX packets) to virtio-input after DRIVER_OK (no duplicates)",
  async ({ page }) => {
  test.setTimeout(45_000);
  await page.goto(`/`, { waitUntil: "load" });

  const bundle = await checkThreadedWasmBundle(page);
  if (!bundle.ok) {
    if (process.env.CI) throw new Error(bundle.message);
    test.skip(true, bundle.message);
  }

  const support = await page.evaluate(() => {
    let wasmThreads = false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      wasmThreads = true;
    } catch {
      wasmThreads = false;
    }
    return {
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
      atomics: typeof Atomics !== "undefined",
      worker: typeof Worker !== "undefined",
      wasmThreads,
    };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics || !support.worker || !support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  const result = await page.evaluate(async () => {
    const { allocateSharedMemorySegments, createSharedMemoryViews, StatusIndex } = await import("/web/src/runtime/shared_layout.ts");
    const { emptySetBootDisksMessage } = await import("/web/src/runtime/boot_disks_protocol.ts");
    const { MessageType } = await import("/web/src/runtime/protocol.ts");

    // This test only needs a tiny guest RAM window for virtqueue descriptors/buffers.
    //
    // Keep allocations small to reduce memory pressure when Playwright runs tests fully-parallel
    // across multiple browsers, and to avoid Firefox structured-clone issues when init messages
    // contain multiple aliased SharedArrayBuffers.
    const segments = allocateSharedMemorySegments({
      guestRamMiB: 1,
      vramMiB: 0,
      ioIpcOptions: { includeNet: false, includeHidIn: false },
      sharedFramebufferLayout: { width: 1, height: 1, tileSize: 0 },
    });
    const views = createSharedMemoryViews(segments);

    const status = views.status;

    // WebKit can fail to load large module workers directly via `new Worker(httpUrl, { type: "module" })`
    // (it emits an `error` event without useful details). Wrap the module entrypoint in a tiny
    // blob-based module worker and import the real worker from there for cross-browser stability.
    const ioWorkerEntrypoint = new URL("/web/src/workers/io.worker.ts", location.href).toString();
    const ioWorkerWrapperUrl = URL.createObjectURL(
      new Blob(
        [
          `\n            (async () => {\n              const MAX_ERROR_CHARS = 512;\n              const fallbackFormatErr = (err) => {\n                const msg = err instanceof Error ? err.message : err;\n                return String(msg ?? \"Error\")\n                  .replace(/[\\x00-\\x1F\\x7F]/g, \" \")\n                  .replace(/\\s+/g, \" \")\n                  .trim()\n                  .slice(0, MAX_ERROR_CHARS);\n              };\n\n              let formatErr = fallbackFormatErr;\n              try {\n                const mod = await import(\"/web/src/text.ts\");\n                const formatOneLineUtf8 = mod?.formatOneLineUtf8;\n                if (typeof formatOneLineUtf8 === \"function\") {\n                  formatErr = (err) => {\n                    const msg = err instanceof Error ? err.message : err;\n                    return formatOneLineUtf8(String(msg ?? \"\"), 512) || \"Error\";\n                  };\n                }\n              } catch {\n                // ignore: keep fallbackFormatErr\n              }\n\n              try {\n                await import(${JSON.stringify(ioWorkerEntrypoint)});\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_imported\" }), 0);\n              } catch (err) {\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_import_failed\", message: formatErr(err) }), 0);\n              }\n            })();\n          `,
        ],
        { type: "text/javascript" },
      ),
    );
    const ioWorker = new Worker(ioWorkerWrapperUrl, { type: "module" });

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
          reject(new Error(`io.worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`));
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
        reject(new Error(`io.worker wrapper error during import: ${message} (${filename}:${lineno}:${colno})`));
      };
      const messageErrorHandler = () => {
        cleanup();
        reject(new Error("io.worker wrapper messageerror during import"));
      };

      ioWorker.addEventListener("message", messageHandler);
      ioWorker.addEventListener("error", errorHandler);
      ioWorker.addEventListener("messageerror", messageErrorHandler);
      timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for io.worker import marker"));
      }, 20_000);
      (timer as unknown as { unref?: () => void }).unref?.();
    });

    let ioWorkerError: string | null = null;
    const onIoWorkerMessage = (ev: MessageEvent) => {
      const data = ev.data as { type?: unknown; role?: unknown; message?: unknown } | undefined;
      if (!data || typeof data !== "object") return;
      if (data.type === MessageType.ERROR && data.role === "io") {
        ioWorkerError = typeof data.message === "string" ? data.message : String(data.message);
      }
    };
    const onIoWorkerError = (ev: Event) => {
      const e = ev as ErrorEvent | undefined;
      ioWorkerError = e?.message || "io.worker error";
    };
    const onIoWorkerMessageError = () => {
      ioWorkerError = "io.worker messageerror";
    };
    ioWorker.addEventListener("message", onIoWorkerMessage);
    ioWorker.addEventListener("error", onIoWorkerError);
    ioWorker.addEventListener("messageerror", onIoWorkerMessageError);

    const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
    const waitFor = async (predicate: () => boolean, timeoutMs: number, name: string) => {
      const deadline = (typeof performance?.now === "function" ? performance.now() : Date.now()) + timeoutMs;
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) < deadline) {
        if (ioWorkerError) throw new Error(`io.worker failed: ${ioWorkerError}`);
        if (predicate()) return;
        await sleep(5);
      }
      throw new Error(`Timed out waiting for ${name}`);
    };

    const waitForIoMessage = (predicate: (data: unknown) => boolean, timeoutMs = 5_000): Promise<unknown> => {
      return new Promise((resolve, reject) => {
        const timer = globalThis.setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for io.worker message after ${timeoutMs}ms.`));
        }, timeoutMs);
        (timer as unknown as { unref?: () => void }).unref?.();

        const onMessage = (ev: MessageEvent<unknown>) => {
          if (ioWorkerError) {
            cleanup();
            reject(new Error(`io.worker failed: ${ioWorkerError}`));
            return;
          }
          if (!predicate(ev.data)) return;
          cleanup();
          resolve(ev.data);
        };
        const onError = (ev: ErrorEvent) => {
          cleanup();
          reject(new Error(ev.message || "io.worker error"));
        };
        const cleanup = () => {
          globalThis.clearTimeout(timer);
          ioWorker.removeEventListener("message", onMessage);
          ioWorker.removeEventListener("error", onError);
        };
        ioWorker.addEventListener("message", onMessage);
        ioWorker.addEventListener("error", onError);
      });
    };

    const sendMouseMoveBatch = async (dx: number, dyPs2: number): Promise<void> => {
      const count = 1;
      const buf = new ArrayBuffer((2 + count * 4) * 4);
      const words = new Int32Array(buf);
      words[0] = count | 0;
      words[1] = 0;
      const base = 2;
      words[base + 0] = 2; // InputEventType.MouseMove
      words[base + 1] = 0;
      words[base + 2] = dx | 0;
      words[base + 3] = dyPs2 | 0;

      const recyclePromise = waitForIoMessage((data) => {
        if (!data || typeof data !== "object") return false;
        return (data as { type?: unknown }).type === "in:input-batch-recycle";
      });
      ioWorker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
      await recyclePromise;
    };

    const sendMouseButtonsBatch = async (buttons: number): Promise<void> => {
      const count = 1;
      const buf = new ArrayBuffer((2 + count * 4) * 4);
      const words = new Int32Array(buf);
      words[0] = count | 0;
      words[1] = 0;
      const base = 2;
      words[base + 0] = 3; // InputEventType.MouseButtons
      words[base + 1] = 0;
      words[base + 2] = buttons | 0;
      words[base + 3] = 0;

      const recyclePromise = waitForIoMessage((data) => {
        if (!data || typeof data !== "object") return false;
        return (data as { type?: unknown }).type === "in:input-batch-recycle";
      });
      ioWorker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);
      await recyclePromise;
    };

    const cpuWorkerCode = `
      import { openRingByKind } from "${location.origin}/web/src/ipc/ipc.ts";
      import { IO_IPC_CMD_QUEUE_KIND, IO_IPC_EVT_QUEUE_KIND } from "${location.origin}/web/src/runtime/shared_layout.ts";
      import { AeroIpcIoClient } from "${location.origin}/web/src/io/ipc/aero_ipc_io.ts";

      const PCI_ADDR = 0x0cf8;
      const PCI_DATA = 0x0cfc;

      const VIRTIO_STATUS_ACKNOWLEDGE = 1;
      const VIRTIO_STATUS_DRIVER = 2;
      const VIRTIO_STATUS_DRIVER_OK = 4;
      const VIRTIO_STATUS_FEATURES_OK = 8;

      const VIRTQ_DESC_F_WRITE = 2;

      const EV_SYN = 0;
      const EV_KEY = 1;
      const EV_REL = 2;
      const SYN_REPORT = 0;
      const REL_X = 0;
      const REL_Y = 1;
      const BTN_LEFT = 0x110;

      const VIRTIO_INPUT_ID_DWORD = 0x1052_1af4;
      const VIRTIO_INPUT_SUBSYS_MOUSE_DWORD = 0x0011_1af4;

      const MAX_ERROR_CHARS = 512;
      const fallbackFormatErr = (err) => {
        const msg = err instanceof Error ? err.message : err;
        return String(msg ?? "Error")
          .replace(/[\\x00-\\x1F\\x7F]/g, " ")
          .replace(/\\s+/g, " ")
          .trim()
          .slice(0, MAX_ERROR_CHARS);
      };

      let formatErr = fallbackFormatErr;
      (async () => {
        try {
          const mod = await import("${location.origin}/web/src/text.ts");
          const formatOneLineUtf8 = mod?.formatOneLineUtf8;
          if (typeof formatOneLineUtf8 === "function") {
            formatErr = (err) => {
              const msg = err instanceof Error ? err.message : err;
              return formatOneLineUtf8(String(msg ?? ""), 512) || "Error";
            };
          }
        } catch {
          // ignore: keep fallbackFormatErr
        }
      })();

      function cfgAddr(bus, dev, func, off) {
        return (0x8000_0000 | ((bus & 0xff) << 16) | ((dev & 0x1f) << 11) | ((func & 0x07) << 8) | (off & 0xfc)) >>> 0;
      }

      let io = null;
      let dv = null;
      let guestBase = 0;

      // Canonical virtio-input multifunction device location (keyboard=fn0, mouse=fn1).
      // NOTE: the codebase uses decimal device numbers (see VIRTIO_INPUT_PCI_DEVICE = 10),
      // hence 0:10.1 is dev=10 (not 0x10).
      const BDF = { bus: 0, dev: 10, func: 1 };

      let bar0Base = 0n;
      let queueSize = 0;
      let descBase = 0;
      let availBase = 0;
      let usedBase = 0;
      let eventBufBase = 0;
      let bufferCount = 0;
      let usedIdxBefore = 0;

      function cfgReadU16(off) {
        io.portWrite(PCI_ADDR, 4, cfgAddr(BDF.bus, BDF.dev, BDF.func, off));
        return io.portRead(PCI_DATA + (off & 3), 2) & 0xffff;
      }

      function cfgReadU32(off) {
        io.portWrite(PCI_ADDR, 4, cfgAddr(BDF.bus, BDF.dev, BDF.func, off));
        return io.portRead(PCI_DATA + (off & 3), 4) >>> 0;
      }

      function cfgWriteU16(off, value) {
        io.portWrite(PCI_ADDR, 4, cfgAddr(BDF.bus, BDF.dev, BDF.func, off));
        io.portWrite(PCI_DATA + (off & 3), 2, value & 0xffff);
      }

      function scanForVirtioInputMouseBdf() {
        for (let dev = 0; dev < 32; dev += 1) {
          for (let func = 0; func < 8; func += 1) {
            io.portWrite(PCI_ADDR, 4, cfgAddr(0, dev, func, 0x00));
            const id = io.portRead(PCI_DATA, 4) >>> 0;
            if (id !== VIRTIO_INPUT_ID_DWORD) continue;
            io.portWrite(PCI_ADDR, 4, cfgAddr(0, dev, func, 0x2c));
            const subsys = io.portRead(PCI_DATA, 4) >>> 0;
            if (subsys !== VIRTIO_INPUT_SUBSYS_MOUSE_DWORD) continue;
            return { bus: 0, device: dev, function: func };
          }
        }
        return null;
      }

      function mmioReadU8(addr) {
        return io.mmioRead(addr, 1) & 0xff;
      }
      function mmioReadU16(addr) {
        return io.mmioRead(addr, 2) & 0xffff;
      }
      function mmioReadU32(addr) {
        return io.mmioRead(addr, 4) >>> 0;
      }
      function mmioWriteU8(addr, value) {
        io.mmioWrite(addr, 1, value & 0xff);
      }
      function mmioWriteU16(addr, value) {
        io.mmioWrite(addr, 2, value & 0xffff);
      }
      function mmioWriteU32(addr, value) {
        io.mmioWrite(addr, 4, value >>> 0);
      }
      function mmioWriteU64(addr, value) {
        mmioWriteU32(addr, Number(value & 0xffff_ffffn));
        mmioWriteU32(addr + 4n, Number((value >> 32n) & 0xffff_ffffn));
      }

      function linear(paddr) {
        return guestBase + (paddr >>> 0);
      }

      function guestWriteU16(paddr, value) {
        dv.setUint16(linear(paddr), value & 0xffff, true);
      }

      function guestWriteU32(paddr, value) {
        dv.setUint32(linear(paddr), value >>> 0, true);
      }

      function guestWriteBytes(paddr, bytes) {
        new Uint8Array(dv.buffer, linear(paddr), bytes.byteLength).set(bytes);
      }

      function guestReadU16(paddr) {
        return dv.getUint16(linear(paddr), true) >>> 0;
      }

      function guestReadU32(paddr) {
        return dv.getUint32(linear(paddr), true) >>> 0;
      }

      function guestReadBytes(paddr, len) {
        return new Uint8Array(dv.buffer, linear(paddr), len).slice();
      }

      function guestWriteDesc(table, index, addr, len, flags, next) {
        const base = table + index * 16;
        dv.setUint32(linear(base + 0), addr >>> 0, true);
        dv.setUint32(linear(base + 4), 0, true);
        dv.setUint32(linear(base + 8), len >>> 0, true);
        dv.setUint16(linear(base + 12), flags & 0xffff, true);
        dv.setUint16(linear(base + 14), next & 0xffff, true);
      }

      function drainI8042(maxBytes = 256) {
        const bytes = [];
        const statuses = [];
        for (let i = 0; i < maxBytes; i += 1) {
          const st = io.portRead(0x64, 1) & 0xff;
          if ((st & 0x01) === 0) break;
          statuses.push(st);
          const b = io.portRead(0x60, 1) & 0xff;
          bytes.push(b);
        }
        return { bytes, statuses };
      }

      function i8042WaitInputReady() {
        for (let i = 0; i < 10_000; i += 1) {
          const st = io.portRead(0x64, 1) & 0xff;
          if ((st & 0x02) === 0) return;
        }
        throw new Error("i8042 input buffer never became ready");
      }

      function i8042WriteCmd(cmd) {
        i8042WaitInputReady();
        io.portWrite(0x64, 1, cmd & 0xff);
      }

      function i8042WriteData(byte) {
        i8042WaitInputReady();
        io.portWrite(0x60, 1, byte & 0xff);
      }

      function i8042WriteAux(byte) {
        i8042WriteCmd(0xd4);
        i8042WriteData(byte);
      }

      function initPs2MouseReporting() {
        // Ensure AUX port enabled and data reporting on, otherwise i8042 drops injected movement in stream mode.
        i8042WriteCmd(0xa8); // enable aux device
        i8042WriteAux(0xf4); // enable data reporting
        drainI8042();
      }

      function decodeEvent(bytes) {
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        return { type_: view.getUint16(0, true) >>> 0, code: view.getUint16(2, true) >>> 0, value: view.getInt32(4, true) | 0 };
      }

      function handleInit() {
        initPs2MouseReporting();
        // Start with a clean output buffer.
        drainI8042();

        const idDword = cfgReadU32(0x00);
        const subsysDword = cfgReadU32(0x2c);
        const virtioOk = idDword === VIRTIO_INPUT_ID_DWORD && subsysDword === VIRTIO_INPUT_SUBSYS_MOUSE_DWORD;
        const foundMouseBdf = virtioOk ? null : scanForVirtioInputMouseBdf();
        if (!virtioOk) {
          return { virtioOk, idDword, subsysDword, foundMouseBdf };
        }

         // Enable memory decoding (PCI command bit1) + bus mastering (bit2) so BAR-backed MMIO is
         // active and the device is allowed to DMA into guest memory (virtqueue reads/writes).
         cfgWriteU16(0x04, cfgReadU16(0x04) | 0x0006);

        const bar0Low = cfgReadU32(0x10);
        const bar0High = cfgReadU32(0x14);
         // Note: avoid bitwise ops on numbers here because JS bitwise ops use signed i32 and can produce a
         // negative value for addresses >= 2^31.
         bar0Base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);

        const commonBase = bar0Base + 0x0000n;
        const deviceStatusBefore = mmioReadU8(commonBase + 0x14n);

        return { virtioOk, idDword, subsysDword, foundMouseBdf, bar0Low, bar0High, deviceStatusBefore };
      }

       function virtioInitAndSetupQueue0() {
         const commonBase = bar0Base + 0x0000n;
         const notifyBase = bar0Base + 0x1000n;

         // Virtio modern init.
         mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE);
         mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

        for (const sel of [0, 1]) {
          mmioWriteU32(commonBase + 0x00n, sel);
          const f = mmioReadU32(commonBase + 0x04n);
          mmioWriteU32(commonBase + 0x08n, sel);
          mmioWriteU32(commonBase + 0x0cn, f);
        }

        // FEATURES_OK: the driver has accepted the feature set.
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);

        // Queue config (eventq = queue 0). Do this before DRIVER_OK (spec-correct).
        descBase = 0x1000;
        availBase = 0x2000;
        usedBase = 0x3000;
        eventBufBase = 0x4000;

        mmioWriteU16(commonBase + 0x16n, 0);
        queueSize = mmioReadU16(commonBase + 0x18n);
        const notifyOff = mmioReadU16(commonBase + 0x1en);

        bufferCount = Math.min(16, queueSize);
        if (bufferCount < 8) bufferCount = queueSize;

        mmioWriteU64(commonBase + 0x20n, BigInt(descBase));
        mmioWriteU64(commonBase + 0x28n, BigInt(availBase));
        mmioWriteU64(commonBase + 0x30n, BigInt(usedBase));
        mmioWriteU16(commonBase + 0x1cn, 1);

        for (let i = 0; i < bufferCount; i += 1) {
          const bufAddr = eventBufBase + i * 8;
          guestWriteBytes(bufAddr, new Uint8Array(8).fill(0xaa));
          guestWriteDesc(descBase, i, bufAddr, 8, VIRTQ_DESC_F_WRITE, 0);
        }

        // Avail ring: flags=0, idx=bufferCount, ring[i]=descriptor index.
        guestWriteU16(availBase + 0, 0);
        guestWriteU16(availBase + 2, bufferCount);
        for (let i = 0; i < bufferCount; i += 1) {
          guestWriteU16(availBase + 4 + i * 2, i);
        }

        // Used ring: flags=0, idx=0.
        guestWriteU16(usedBase + 0, 0);
        guestWriteU16(usedBase + 2, 0);
        for (let i = 0; i < bufferCount; i += 1) {
          guestWriteU32(usedBase + 4 + i * 8 + 0, 0);
          guestWriteU32(usedBase + 4 + i * 8 + 4, 0);
        }

        // DRIVER_OK: the driver is ready; the device can now start processing queues.
        mmioWriteU8(
          commonBase + 0x14n,
          VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK,
        );
        const deviceStatusAfter = mmioReadU8(commonBase + 0x14n);

        // Notify queue 0 (notify_off_multiplier is fixed to 4 in contract v1).
        mmioWriteU16(notifyBase + BigInt((notifyOff >>> 0) * 4), 0);
        usedIdxBefore = guestReadU16(usedBase + 2);

        return { deviceStatusAfter, queueSize, notifyOff, usedIdxBefore };
      }

      function collectAfterPhase2() {
        const i8042 = drainI8042();
        const startIdx = usedIdxBefore;

        const deadline = (typeof performance?.now === "function" ? performance.now() : Date.now()) + 2_000;
        let usedIdxAfter = guestReadU16(usedBase + 2);
        while (usedIdxAfter === usedIdxBefore && (typeof performance?.now === "function" ? performance.now() : Date.now()) < deadline) {
          usedIdxAfter = guestReadU16(usedBase + 2);
        }
        if (usedIdxAfter === usedIdxBefore) {
          // Provide some diagnostics to make failures actionable.
          let deviceStatus = 0;
          let pciCommand = 0;
          let queueEnable = 0;
          let bar0Low = 0;
          let bar0High = 0;
          try {
            const commonBase = bar0Base + 0x0000n;
            deviceStatus = mmioReadU8(commonBase + 0x14n);
            queueEnable = mmioReadU16(commonBase + 0x1cn);
          } catch {
            deviceStatus = 0;
            queueEnable = 0;
          }
          try {
            pciCommand = cfgReadU16(0x04);
          } catch {
            pciCommand = 0;
          }
          try {
            bar0Low = cfgReadU32(0x10);
            bar0High = cfgReadU32(0x14);
          } catch {
            bar0Low = 0;
            bar0High = 0;
          }
          throw new Error(
            [
              "Timed out waiting for virtio-input used.idx to advance after injection.",
              "usedIdxBefore=" + usedIdxBefore + " usedIdxAfter=" + usedIdxAfter,
              "bar0Base=0x" + bar0Base.toString(16) + " bar0Low=0x" + bar0Low.toString(16) + " bar0High=0x" + bar0High.toString(16),
              "pciCommand=0x" +
                pciCommand.toString(16) +
                " deviceStatus=0x" +
                deviceStatus.toString(16) +
                " queueEnable=" +
                queueEnable,
              "i8042Bytes=" + i8042.bytes.length,
            ].join(" "),
          );
        }

        const delta = (usedIdxAfter - usedIdxBefore) & 0xffff;
        const events = [];
        const used = [];
        for (let i = 0; i < delta; i += 1) {
          const usedRingIndex = (usedIdxBefore + i) % queueSize;
          const id = guestReadU32(usedBase + 4 + usedRingIndex * 8 + 0);
          const len = guestReadU32(usedBase + 4 + usedRingIndex * 8 + 4);
          used.push({ id, len });
          const ev = decodeEvent(guestReadBytes(eventBufBase + (id * 8), 8));
          events.push(ev);
        }

        // Advance the cursor so subsequent calls read only newly-produced events.
        usedIdxBefore = usedIdxAfter;
        return { i8042, usedIdxBefore: startIdx, usedIdxAfter, delta, used, events };
      }

      function isEvent(ev, type_, code, value) {
        return ev && ev.type_ === type_ && ev.code === code && ev.value === value;
      }

      self.onmessage = (ev) => {
        const msg = ev.data;
        if (!msg || typeof msg !== "object") return;
        const id = msg.id;
        if (typeof id !== "number") return;
        try {
          if (msg.cmd === "init") {
            const { ioIpcSab, guestMemory, guestBase: gb } = msg;
            if (!(ioIpcSab instanceof SharedArrayBuffer)) throw new Error("init expected ioIpcSab SharedArrayBuffer");
            if (!(guestMemory instanceof WebAssembly.Memory)) throw new Error("init expected guestMemory WebAssembly.Memory");
            if (typeof gb !== "number") throw new Error("init expected guestBase number");

            const cmdQ = openRingByKind(ioIpcSab, IO_IPC_CMD_QUEUE_KIND);
            const evtQ = openRingByKind(ioIpcSab, IO_IPC_EVT_QUEUE_KIND);
            io = new AeroIpcIoClient(cmdQ, evtQ);
            guestBase = gb >>> 0;
            dv = new DataView(guestMemory.buffer);

            const initRes = handleInit();
            self.postMessage({ id, ok: true, ...initRes });
            return;
          }
          if (msg.cmd === "drainI8042") {
            const res = drainI8042();
            self.postMessage({ id, ok: true, ...res });
            return;
          }
          if (msg.cmd === "virtioInit") {
            const res = virtioInitAndSetupQueue0();
            self.postMessage({ id, ok: true, ...res });
            return;
          }
          if (msg.cmd === "collectAfterPhase2") {
            const res = collectAfterPhase2();
            self.postMessage({ id, ok: true, ...res });
            return;
          }
          self.postMessage({ id, ok: false, error: "unknown cmd" });
        } catch (err) {
          self.postMessage({ id, ok: false, error: formatErr(err) });
        }
      };
    `;

    const cpuUrl = URL.createObjectURL(new Blob([cpuWorkerCode], { type: "text/javascript" }));
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    let nextCpuId = 1;
    const cpuPending = new Map<number, { resolve: (v: any) => void; reject: (err: unknown) => void }>();
    cpuWorker.onmessage = (ev) => {
      const data = ev.data as any;
      const id = data?.id;
      if (typeof id !== "number") return;
      const pending = cpuPending.get(id);
      if (!pending) return;
      cpuPending.delete(id);
      pending.resolve(data);
    };
    cpuWorker.onerror = (err) => {
      const pending = Array.from(cpuPending.values());
      cpuPending.clear();
      for (const p of pending) p.reject(err);
    };

    const cpuCall = async (cmd: string, payload?: Record<string, unknown>, timeoutMs = 10_000): Promise<any> => {
      const id = nextCpuId++;
      const p = new Promise((resolve, reject) => cpuPending.set(id, { resolve, reject }));
      cpuWorker.postMessage({ id, cmd, ...(payload ?? {}) });
      const res = await Promise.race([
        p,
        new Promise((_, reject) => setTimeout(() => reject(new Error(`Timed out waiting for CPU cmd=${cmd}`)), timeoutMs)),
      ]);
      if (!res || typeof res !== "object" || (res as any).ok !== true) {
        throw new Error(`CPU cmd=${cmd} failed: ${(res as any)?.error ?? "unknown error"}`);
      }
      return res;
    };

    try {
      ioWorker.postMessage({
        kind: "init",
        role: "io",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      });
      ioWorker.postMessage(emptySetBootDisksMessage());

      await waitFor(() => Atomics.load(status, StatusIndex.IoReady) === 1, 20_000, "StatusIndex.IoReady");

      const dx = 5;
      const dyPs2 = 7; // positive = up (PS/2 convention); IO worker flips for virtio REL_Y.

      const initRes = await cpuCall("init", {
        ioIpcSab: segments.ioIpc,
        guestMemory: segments.guestMemory,
        guestBase: views.guestLayout.guest_base,
      });

      if (!initRes.virtioOk) {
        const id = (initRes?.idDword ?? 0) >>> 0;
        const subsys = (initRes?.subsysDword ?? 0) >>> 0;
        const found = initRes?.foundMouseBdf as { bus: number; device: number; function: number } | null | undefined;
        const foundStr = found ? `${found.bus}:${found.device}.${found.function}` : "not found";
        throw new Error(
          `virtio-input mouse PCI function is unavailable at canonical BDF 0:10.1 (id=0x${id.toString(16)} subsys=0x${subsys.toString(16)}; scan=${foundStr})`,
        );
      }

      // Phase 1: virtio driver_ok is false; mouse goes through i8042 AUX output.
      await sendMouseMoveBatch(dx, dyPs2);
      const phase1 = await cpuCall("drainI8042");
      const mouseBackendAfterPhase1 = Atomics.load(status, StatusIndex.IoInputMouseBackend) | 0;

      // Phase 2: guest completes virtio init + DRIVER_OK; mouse goes through virtio-input eventq.
      const mouseBackendSwitchesBefore = Atomics.load(status, StatusIndex.IoMouseBackendSwitchCounter) >>> 0;
      const virtioInit = await cpuCall("virtioInit");
      await sendMouseMoveBatch(dx, dyPs2);
      const phase2Move = await cpuCall("collectAfterPhase2");

      // Optional: verify button events also route exclusively to virtio-input post-switch.
      await sendMouseButtonsBatch(0x01);
      const phase2BtnDown = await cpuCall("collectAfterPhase2");
      await sendMouseButtonsBatch(0x00);
      const phase2BtnUp = await cpuCall("collectAfterPhase2");

      const mouseBackendSwitchesAfter = Atomics.load(status, StatusIndex.IoMouseBackendSwitchCounter) >>> 0;
      const mouseBackendAfterPhase2 = Atomics.load(status, StatusIndex.IoInputMouseBackend) | 0;
      const virtioMouseDriverOkAfterPhase2 = Atomics.load(status, StatusIndex.IoInputVirtioMouseDriverOk) | 0;

      return {
        initRes,
        dx,
        dyPs2,
        phase1,
        virtioInit,
        phase2Move,
        phase2BtnDown,
        phase2BtnUp,
        mouseBackendSwitchesBefore,
        mouseBackendSwitchesAfter,
        mouseBackendAfterPhase1,
        mouseBackendAfterPhase2,
        virtioMouseDriverOkAfterPhase2,
      };
    } finally {
      cpuWorker.terminate();
      ioWorker.terminate();
      URL.revokeObjectURL(cpuUrl);
      URL.revokeObjectURL(ioWorkerWrapperUrl);
    }
  });

  expect(result.initRes.idDword >>> 0).toBe(0x1052_1af4);
  expect(result.initRes.subsysDword >>> 0).toBe(0x0011_1af4);
  expect(result.initRes.deviceStatusBefore & 0x04).toBe(0x00);

  // Phase 1: i8042 should output some mouse bytes.
  expect(result.phase1.bytes.length).toBeGreaterThan(0);
  expect(result.mouseBackendAfterPhase1).toBe(0); // ps2
  // Prefer an AUX-bit assertion, but don't overfit the packet contents.
  expect(result.phase1.statuses.some((st: number) => (st & 0x20) !== 0)).toBe(true);

  // After virtio DRIVER_OK and queue provisioning, i8042 should be quiet.
  expect(result.virtioInit.deviceStatusAfter & 0x0f).toBe(0x0f);
  expect(result.phase2Move.i8042.bytes).toEqual([]);

  // Backend switching should be observable via the IO worker telemetry counter.
  expect(result.mouseBackendSwitchesAfter - result.mouseBackendSwitchesBefore).toBe(1);
  expect(result.virtioMouseDriverOkAfterPhase2).toBe(1);
  expect(result.mouseBackendAfterPhase2).toBe(2); // virtio

  // For a single mouse move, expect EV_REL(REL_X), EV_REL(REL_Y), EV_SYN(SYN_REPORT).
  expect(result.phase2Move.delta).toBe(3);
  expect(result.phase2Move.used.map((u: { len: number }) => u.len)).toEqual([8, 8, 8]);

  expect(result.phase2Move.events).toEqual([
    { type_: 2, code: 0, value: result.dx },
    // IO worker flips dy (PS/2 up => virtio down).
    { type_: 2, code: 1, value: -result.dyPs2 },
    { type_: 0, code: 0, value: 0 },
  ]);

  // Button down: EV_KEY(BTN_LEFT=0x110), EV_SYN(SYN_REPORT).
  expect(result.phase2BtnDown.i8042.bytes).toEqual([]);
  expect(result.phase2BtnDown.delta).toBe(2);
  expect(result.phase2BtnDown.used.map((u: { len: number }) => u.len)).toEqual([8, 8]);
  expect(result.phase2BtnDown.events).toEqual([
    { type_: 1, code: 0x110, value: 1 },
    { type_: 0, code: 0, value: 0 },
  ]);

  // Button up: EV_KEY(BTN_LEFT=0x110), EV_SYN(SYN_REPORT).
  expect(result.phase2BtnUp.i8042.bytes).toEqual([]);
  expect(result.phase2BtnUp.delta).toBe(2);
  expect(result.phase2BtnUp.used.map((u: { len: number }) => u.len)).toEqual([8, 8]);
  expect(result.phase2BtnUp.events).toEqual([
    { type_: 1, code: 0x110, value: 0 },
    { type_: 0, code: 0, value: 0 },
  ]);
});

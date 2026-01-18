import { expect, test } from "@playwright/test";

import { checkThreadedWasmBundle } from "./util/wasm_bundle";

test("IO worker does not switch keyboard backend while a Consumer Control key is held (prevents stuck media keys)", async ({ page }) => {
  test.setTimeout(60_000);
  await page.goto("/", { waitUntil: "load" });

  const bundle = await checkThreadedWasmBundle(page);
  if (!bundle.ok) {
    if (process.env.CI) throw new Error(bundle.message);
    test.skip(true, bundle.message);
  }

  const support = await page.evaluate(() => {
    const crossOriginIsolated = globalThis.crossOriginIsolated === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== "undefined";
    const atomics = typeof Atomics !== "undefined";
    const worker = typeof Worker !== "undefined";
    const wasm = typeof WebAssembly !== "undefined" && typeof WebAssembly.Memory === "function";
    let wasmThreads = false;
    if (wasm) {
      try {
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
        wasmThreads = true;
      } catch {
        wasmThreads = false;
      }
    }
    return { crossOriginIsolated, sharedArrayBuffer, atomics, worker, wasm, wasmThreads };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics, "Atomics is unavailable in this browser configuration.");
  test.skip(!support.worker, "Web Workers are unavailable in this environment.");
  test.skip(!support.wasm, "WebAssembly.Memory is unavailable in this environment.");
  test.skip(!support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  const result = await page.evaluate(async () => {
    const { allocateSharedMemorySegments, createSharedMemoryViews, StatusIndex } = await import("/web/src/runtime/shared_layout.ts");
    const { InputEventQueue } = await import("/web/src/input/event_queue.ts");
    const { MessageType } = await import("/web/src/runtime/protocol.ts");
    const { emptySetBootDisksMessage } = await import("/web/src/runtime/boot_disks_protocol.ts");

    // This test only needs a tiny guest RAM window for virtqueue descriptors/buffers.
    //
    // Keep allocations small to reduce memory pressure when Playwright runs tests fully-parallel
    // across multiple browsers, and to avoid Firefox structured-clone issues when init messages
    // contain multiple aliased SharedArrayBuffers (e.g. guestMemory.buffer + scanout/cursor state).
    const segments = allocateSharedMemorySegments({
      guestRamMiB: 1,
      vramMiB: 0,
      ioIpcOptions: { includeNet: false, includeHidIn: false },
      sharedFramebufferLayout: { width: 1, height: 1, tileSize: 0 },
    });
    const views = createSharedMemoryViews(segments);
    const status = views.status;
    const guestBase = views.guestLayout.guest_base >>> 0;

    // WebKit can fail to load large module workers directly via `new Worker(httpUrl, { type: "module" })`
    // (it emits an `error` event without useful details). Wrap the module entrypoint in a tiny
    // blob-based module worker and import the real worker from there for cross-browser stability.
    const ioWorkerEntrypoint = new URL("/web/src/workers/io.worker.ts", location.href).toString();
    const ioWorkerWrapperUrl = URL.createObjectURL(
      new Blob(
        [
          `\n            (async () => {\n              const MAX_ERROR_CHARS = 512;\n              const fallbackFormatErr = (err) => {\n                const msg = err instanceof Error ? err.message : err;\n                return String(msg ?? \"Error\")\n                  .replace(/[\\x00-\\x1F\\x7F]/g, \" \")\n                  .replace(/\\s+/g, \" \")\n                  .trim()\n                  .slice(0, MAX_ERROR_CHARS);\n              };\n\n              let formatErr = fallbackFormatErr;\n              try {\n                const mod = await import(\"/web/src/text.ts\");\n                const formatOneLineUtf8 = mod?.formatOneLineUtf8;\n                if (typeof formatOneLineUtf8 === \"function\") {\n                  formatErr = (err) => {\n                    const msg = err instanceof Error ? err.message : err;\n                    return formatOneLineUtf8(String(msg ?? \"\"), 512) || \"Error\";\n                  };\n                }\n              } catch {\n                // ignore: keep fallbackFormatErr\n              }\n\n              try {\n                await import(${JSON.stringify(ioWorkerEntrypoint)});\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_imported\" }), 0);\n              } catch (err) {\n                setTimeout(() => self.postMessage({ type: \"__aero_io_worker_import_failed\", message: formatErr(err) }), 0);\n              }\n            })();\n         `,
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

    // io.worker waits for an initial boot disk selection message before reporting READY.
    ioWorker.postMessage(emptySetBootDisksMessage());
    ioWorker.postMessage({
      kind: "init",
      role: "io",
      controlSab: segments.control,
      guestMemory: segments.guestMemory,
      ioIpcSab: segments.ioIpc,
      sharedFramebuffer: segments.sharedFramebuffer,
      sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
    });

    const waitForAtomic = async (idx: number, expected: number, timeoutMs: number): Promise<void> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        if (ioWorkerError) throw new Error(`io.worker failed: ${ioWorkerError}`);
        if (Atomics.load(status, idx) === expected) return;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`Timed out waiting for status[${idx}] == ${expected} after ${timeoutMs}ms (got ${Atomics.load(status, idx)}).`);
    };

    const waitForIoInputBatchCounter = async (prev: number, timeoutMs: number): Promise<number> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        if (ioWorkerError) throw new Error(`io.worker failed: ${ioWorkerError}`);
        const cur = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
        if (cur > prev) return cur;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(
        `Timed out waiting for IoInputBatchCounter to advance past ${prev} after ${timeoutMs}ms (still ${Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0}).`,
      );
    };

    const flushQueue = (q: InstanceType<typeof InputEventQueue>): void => {
      q.flush(
        {
          postMessage: (msg, transfer) => {
            ioWorker.postMessage(msg, transfer);
          },
        },
        { recycle: false },
      );
    };

    const sendVolumeUpPressNoRelease = (): void => {
      const q = new InputEventQueue(8);
      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      q.pushHidUsage16(nowUs, 0x0c, 0x00e9, true);
      flushQueue(q);
    };

    const sendVolumeUpRelease = (): void => {
      const q = new InputEventQueue(8);
      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      q.pushHidUsage16(nowUs, 0x0c, 0x00e9, false);
      flushQueue(q);
    };

    const sendKeyBPressRelease = (): void => {
      const q = new InputEventQueue(16);
      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      // Include both representations: KeyHidUsage drives `keysHeld` tracking; KeyScancode drives PS/2 injection.
      q.pushKeyScancode(nowUs, 0x32, 1); // B make
      q.pushKeyHidUsage(nowUs, 0x05, true); // B pressed
      q.pushKeyScancode(nowUs, 0x32f0, 2); // B break (0xf0 0x32)
      q.pushKeyHidUsage(nowUs, 0x05, false); // B released
      flushQueue(q);
    };

    const cpuWorkerCode = `
      import { openRingByKind } from "${location.origin}/web/src/ipc/ipc.ts";
      import { queueKind } from "${location.origin}/web/src/ipc/layout.ts";
      import { AeroIpcIoClient } from "${location.origin}/web/src/io/ipc/aero_ipc_io.ts";

      const PCI_ADDR = 0x0cf8;
      const PCI_DATA = 0x0cfc;

      // Virtio status flags (virtio spec).
      const VIRTIO_STATUS_ACKNOWLEDGE = 1;
      const VIRTIO_STATUS_DRIVER = 2;
      const VIRTIO_STATUS_DRIVER_OK = 4;
      const VIRTIO_STATUS_FEATURES_OK = 8;

      // Virtqueue descriptor flags.
      const VIRTQ_DESC_F_WRITE = 2;

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

      function pciAddr(bus, dev, func, reg) {
        return (0x80000000 | ((bus & 0xff) << 16) | ((dev & 0x1f) << 11) | ((func & 0x07) << 8) | (reg & 0xfc)) >>> 0;
      }

      function pciRead32(io, bus, dev, func, reg) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, reg));
        return io.portRead(PCI_DATA, 4) >>> 0;
      }

      function pciRead16(io, bus, dev, func, off) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, off));
        return io.portRead(PCI_DATA + (off & 2), 2) & 0xffff;
      }

      function pciWrite16(io, bus, dev, func, off, value) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, off));
        io.portWrite(PCI_DATA + (off & 2), 2, value & 0xffff);
      }

      function nowMs() {
        return typeof performance?.now === "function" ? performance.now() : Date.now();
      }

      const sleepI32 = new Int32Array(new SharedArrayBuffer(4));
      const sleep = (ms) => Atomics.wait(sleepI32, 0, 0, ms);

      let dv = null;
      let io = null;
      let guestBase = 0;
      let guestSab = null;
      let virtio = null;

      function guestWriteU16(paddr, value) {
        dv.setUint16(guestBase + (paddr >>> 0), value & 0xffff, true);
      }
      function guestWriteU32(paddr, value) {
        dv.setUint32(guestBase + (paddr >>> 0), value >>> 0, true);
      }
      function guestReadU16(paddr) {
        return dv.getUint16(guestBase + (paddr >>> 0), true) >>> 0;
      }
      function guestWriteBytes(paddr, bytes) {
        new Uint8Array(guestSab, guestBase + (paddr >>> 0), bytes.byteLength).set(bytes);
      }
      function guestReadBytes(paddr, len) {
        return new Uint8Array(guestSab, guestBase + (paddr >>> 0), len >>> 0).slice();
      }
      function guestWriteDesc(table, index, addr, len, flags, next) {
        const base = (table >>> 0) + (index >>> 0) * 16;
        // u64 addr (low, then high=0)
        dv.setUint32(guestBase + base, addr >>> 0, true);
        dv.setUint32(guestBase + base + 4, 0, true);
        dv.setUint32(guestBase + base + 8, len >>> 0, true);
        dv.setUint16(guestBase + base + 12, flags & 0xffff, true);
        dv.setUint16(guestBase + base + 14, next & 0xffff, true);
      }

      function mmioReadU16(addr) { return io.mmioRead(addr, 2) & 0xffff; }
      function mmioReadU32(addr) { return io.mmioRead(addr, 4) >>> 0; }
      function mmioWriteU8(addr, value) { io.mmioWrite(addr, 1, value & 0xff); }
      function mmioWriteU16(addr, value) { io.mmioWrite(addr, 2, value & 0xffff); }
      function mmioWriteU32(addr, value) { io.mmioWrite(addr, 4, value >>> 0); }
      function mmioWriteU64(addr, value) {
        mmioWriteU32(addr, Number(value & 0xffff_ffffn));
        mmioWriteU32(addr + 4n, Number((value >> 32n) & 0xffff_ffffn));
      }

      function virtioUsedIdx() {
        if (!virtio) return 0;
        return guestReadU16(virtio.used + 2) >>> 0;
      }

      function virtioUsedEntry(i) {
        if (!virtio) return { id: 0, len: 0 };
        const base = virtio.used + 4 + (i >>> 0) * 8;
        const id = dv.getUint32(guestBase + base, true) >>> 0;
        const len = dv.getUint32(guestBase + base + 4, true) >>> 0;
        return { id, len };
      }

      function decodeInputEvent(evBytes) {
        const dv2 = new DataView(evBytes.buffer, evBytes.byteOffset, evBytes.byteLength);
        const type = dv2.getUint16(0, true);
        const code = dv2.getUint16(2, true);
        const value = dv2.getInt32(4, true);
        return { type, code, value };
      }

      function readVirtioEvents(maxEvents) {
        const idx = virtioUsedIdx();
        const count = Math.min(idx, maxEvents >>> 0);
        const events = [];
        for (let i = 0; i < count; i += 1) {
          const ent = virtioUsedEntry(i);
          const evBytes = guestReadBytes(virtio.eventBufBase + (ent.id >>> 0) * 8, 8);
          events.push({ id: ent.id >>> 0, len: ent.len >>> 0, event: decodeInputEvent(evBytes) });
        }
        return { usedIdx: idx, events };
      }

      function drainI8042(io) {
        const STATUS = 0x64;
        const DATA = 0x60;
        const out = [];
        for (let i = 0; i < 256; i += 1) {
          const st = io.portRead(STATUS, 1) & 0xff;
          if ((st & 1) === 0) break;
          out.push(io.portRead(DATA, 1) & 0xff);
        }
        return out;
      }

      function reply(reqId, ok, result, error) {
        self.postMessage({ reqId, ok, result, error });
      }

      self.onmessage = (ev) => {
        const msg = ev.data || {};
        const reqId = msg.reqId >>> 0;
        const cmd = msg.cmd;
        try {
          if (cmd === "init") {
            const ioIpcSab = msg.ioIpcSab;
            guestBase = msg.guestBase >>> 0;
            guestSab = msg.guestSab;
            if (!(ioIpcSab instanceof SharedArrayBuffer)) throw new Error("init: ioIpcSab must be SharedArrayBuffer");
            if (!(guestSab instanceof SharedArrayBuffer)) throw new Error("init: guestSab must be SharedArrayBuffer");

            dv = new DataView(guestSab);
            const cmdQ = openRingByKind(ioIpcSab, queueKind.CMD);
            const evtQ = openRingByKind(ioIpcSab, queueKind.EVT);
            io = new AeroIpcIoClient(cmdQ, evtQ);
            reply(reqId, true, { ok: true }, null);
            return;
          }

          if (!io || !dv || !guestSab) throw new Error("CPU worker not initialized");

          if (cmd === "drainI8042") {
            reply(reqId, true, { bytes: drainI8042(io) }, null);
            return;
          }

          if (cmd === "virtioInit") {
            // virtio-input keyboard lives at BDF 0:10.0 (device number 10, function 0).
            const idFn0 = pciRead32(io, 0, 10, 0, 0x00);
            if ((idFn0 >>> 0) !== 0x1052_1af4) {
              throw new Error("Unexpected virtio-input fn0 ID: 0x" + (idFn0 >>> 0).toString(16));
            }

            // Enable PCI memory decoding (bit1) + bus mastering (DMA, bit2).
            const cmdReg = pciRead16(io, 0, 10, 0, 0x04);
            pciWrite16(io, 0, 10, 0, 0x04, cmdReg | 0x6);

            const bar0Lo = pciRead32(io, 0, 10, 0, 0x10);
            const bar0Hi = pciRead32(io, 0, 10, 0, 0x14);
            const bar0Base = (BigInt(bar0Hi >>> 0) << 32n) | (BigInt(bar0Lo) & 0xffff_fff0n);

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
            mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);

            // Queue 0 config (eventq). Do this before DRIVER_OK (spec-correct).
            const desc = 0x1000;
            const avail = 0x2000;
            const used = 0x3000;
            const eventBufBase = 0x4000;

            mmioWriteU16(commonBase + 0x16n, 0); // queue_select
            const notifyOff = mmioReadU16(commonBase + 0x1en);

            mmioWriteU64(commonBase + 0x20n, BigInt(desc));
            mmioWriteU64(commonBase + 0x28n, BigInt(avail));
            mmioWriteU64(commonBase + 0x30n, BigInt(used));
            mmioWriteU16(commonBase + 0x1cn, 1); // queue_enable

            const bufferCount = 8;
            for (let i = 0; i < bufferCount; i += 1) {
              const bufAddr = eventBufBase + i * 8;
              guestWriteBytes(bufAddr, new Uint8Array(8).fill(0xaa));
              guestWriteDesc(desc, i, bufAddr, 8, VIRTQ_DESC_F_WRITE, 0);
            }

            // Avail ring: flags=0, idx=bufferCount, ring[i]=descriptor index.
            guestWriteU16(avail + 0, 0);
            guestWriteU16(avail + 2, bufferCount);
            for (let i = 0; i < bufferCount; i += 1) {
              guestWriteU16(avail + 4 + i * 2, i);
            }

            // Used ring: flags=0, idx=0.
            guestWriteU16(used + 0, 0);
            guestWriteU16(used + 2, 0);

            // DRIVER_OK: the driver is ready; the device can now start processing queues.
            mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK);

            // Notify queue 0 (notify_off_multiplier is fixed to 4 in contract v1).
            mmioWriteU16(notifyBase + BigInt((notifyOff >>> 0) * 4), 0);

            virtio = { bar0Base, commonBase, notifyBase, desc, avail, used, eventBufBase, notifyOff };

            reply(reqId, true, { idFn0, usedIdx: virtioUsedIdx() }, null);
            return;
          }

          if (cmd === "waitForVirtioUsedIdx") {
            if (!virtio) throw new Error("virtio not initialized");
            const target = msg.target >>> 0;
            const timeoutMs = msg.timeoutMs >>> 0;
            const start = nowMs();
            for (;;) {
              const cur = virtioUsedIdx();
              if (cur >= target) {
                reply(reqId, true, { usedIdx: cur }, null);
                return;
              }
              if (nowMs() - start > timeoutMs) {
                throw new Error("Timed out waiting for virtio used.idx >= " + target + " (still " + cur + ")");
              }
              sleep(10);
            }
          }

          if (cmd === "readVirtioEvents") {
            if (!virtio) throw new Error("virtio not initialized");
            const maxEvents = msg.maxEvents >>> 0;
            reply(reqId, true, readVirtioEvents(maxEvents), null);
            return;
          }

          reply(reqId, false, null, "Unknown cmd: " + String(cmd));
        } catch (err) {
          reply(reqId, false, null, formatErr(err));
        }
      };
    `;

    const cpuUrl = URL.createObjectURL(new Blob([cpuWorkerCode], { type: "text/javascript" }));
    const cpuWorker = new Worker(cpuUrl, { type: "module" });

    const pending = new Map<number, { resolve: (value: unknown) => void; reject: (err: Error) => void }>();
    let nextReqId = 1;
    let cpuWorkerFatal: Error | null = null;

    const rejectAllPending = (err: Error): void => {
      cpuWorkerFatal = err;
      for (const [, entry] of pending) {
        try {
          entry.reject(err);
        } catch {
          // ignore
        }
      }
      pending.clear();
    };

    cpuWorker.onmessage = (ev: MessageEvent) => {
      const data = ev.data as { reqId?: unknown; ok?: unknown; result?: unknown; error?: unknown };
      const reqId = typeof data.reqId === "number" ? (data.reqId >>> 0) : 0;
      const entry = pending.get(reqId);
      if (!entry) return;
      pending.delete(reqId);
      if (data.ok === true) {
        entry.resolve(data.result);
      } else {
        entry.reject(new Error(typeof data.error === "string" ? data.error : "CPU worker error"));
      }
    };
    cpuWorker.addEventListener("error", (ev) => {
      const msg = (ev as ErrorEvent).message || "CPU worker error";
      rejectAllPending(new Error(msg));
    });
    cpuWorker.addEventListener("messageerror", () => {
      rejectAllPending(new Error("CPU worker messageerror"));
    });

    const callCpu = (cmd: string, payload: Record<string, unknown> = {}, timeoutMs = 2000): Promise<unknown> => {
      const reqId = nextReqId++;
      return new Promise((resolve, reject) => {
        if (cpuWorkerFatal) {
          reject(cpuWorkerFatal);
          return;
        }
        const timer = setTimeout(() => {
          pending.delete(reqId);
          reject(new Error(`Timed out waiting for CPU worker response to ${cmd} after ${timeoutMs}ms.`));
        }, timeoutMs);

        const wrappedResolve = (value: unknown) => {
          clearTimeout(timer);
          resolve(value);
        };
        const wrappedReject = (err: Error) => {
          clearTimeout(timer);
          reject(err);
        };

        pending.set(reqId, { resolve: wrappedResolve, reject: wrappedReject });
        cpuWorker.postMessage({ reqId, cmd, ...payload });
      });
    };

    const drainI8042UntilNonEmpty = async (timeoutMs: number): Promise<number[]> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        const drained = (await callCpu("drainI8042", {}, 2000)) as { bytes: number[] };
        if (drained.bytes.length > 0) return drained.bytes;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`Timed out waiting for non-empty i8042 output after ${timeoutMs}ms.`);
    };

    let virtioIdFn0 = 0;
    let virtioUsedIdxInitial = 0;
    let virtioUsedIdxAfterHold = 0;
    let virtioUsedIdxAfter = 0;
    let virtioEvents: Array<{ type: number; code: number; value: number }> = [];

    let i8042WhileHeldBytes: number[] = [];
    let i8042AfterVirtioBytes: number[] = [];

    let keyboardBackendAfterHold = 0;
    let keyboardBackendAfterRelease = 0;
    let keyboardHeldCountAfterHold = 0;
    let keyboardHeldCountAfterRelease = 0;

    try {
      await waitForAtomic(StatusIndex.IoReady, 1, 10_000);
      await callCpu("init", { ioIpcSab: segments.ioIpc, guestSab: segments.guestMemory.buffer, guestBase }, 5000);

      // Drain any existing i8042 output bytes so phase checks only observe our injections.
      await callCpu("drainI8042", {}, 2000);

      // ---------------------------------------------------------------------
      // Phase 1: Press AudioVolumeUp (Consumer Control) and keep it held.
      // ---------------------------------------------------------------------
      const batch0 = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      sendVolumeUpPressNoRelease();
      await waitForIoInputBatchCounter(batch0, 2000);
      await waitForAtomic(StatusIndex.IoInputKeyboardHeldCount, 1, 2000);

      // Bring virtio-input online (DRIVER_OK + eventq provisioning) while the media key is still held.
      const virtioInit = (await callCpu("virtioInit", {}, 5000)) as { idFn0: number; usedIdx: number };
      virtioIdFn0 = virtioInit.idFn0 >>> 0;
      virtioUsedIdxInitial = virtioInit.usedIdx >>> 0;
      await waitForAtomic(StatusIndex.IoInputVirtioKeyboardDriverOk, 1, 2000);

      // ---------------------------------------------------------------------
      // Phase 2: While the Consumer Control key is held, backend must stay PS/2.
      // Press+release KeyB and ensure bytes show up on i8042 and virtio stays idle.
      // ---------------------------------------------------------------------
      await callCpu("drainI8042", {}, 2000);
      const batch1 = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      sendKeyBPressRelease();
      await waitForIoInputBatchCounter(batch1, 2000);
      i8042WhileHeldBytes = await drainI8042UntilNonEmpty(2000);
      const afterHoldVirtio = (await callCpu("readVirtioEvents", { maxEvents: 0 }, 2000)) as { usedIdx: number };
      virtioUsedIdxAfterHold = afterHoldVirtio.usedIdx >>> 0;
      keyboardBackendAfterHold = Atomics.load(status, StatusIndex.IoInputKeyboardBackend) | 0;
      keyboardHeldCountAfterHold = Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) | 0;

      // ---------------------------------------------------------------------
      // Phase 3: Release AudioVolumeUp. Backend should now be allowed to switch to virtio.
      // ---------------------------------------------------------------------
      const batch2 = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      sendVolumeUpRelease();
      await waitForIoInputBatchCounter(batch2, 2000);
      await waitForAtomic(StatusIndex.IoInputKeyboardHeldCount, 0, 2000);
      await waitForAtomic(StatusIndex.IoInputKeyboardBackend, 2, 2000);
      keyboardBackendAfterRelease = Atomics.load(status, StatusIndex.IoInputKeyboardBackend) | 0;
      keyboardHeldCountAfterRelease = Atomics.load(status, StatusIndex.IoInputKeyboardHeldCount) | 0;

      // ---------------------------------------------------------------------
      // Phase 4: With no keys held, KeyB should now route via virtio-input.
      // i8042 scancode injection must stop, and virtio eventq should advance.
      // ---------------------------------------------------------------------
      await callCpu("drainI8042", {}, 2000);
      const batch3 = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      sendKeyBPressRelease();
      await waitForIoInputBatchCounter(batch3, 2000);

      const expectedDelta = 4; // EV_KEY + EV_SYN for press, then for release.
      await callCpu("waitForVirtioUsedIdx", { target: virtioUsedIdxInitial + expectedDelta, timeoutMs: 2000 }, 3000);

      const virtioRead = (await callCpu("readVirtioEvents", { maxEvents: virtioUsedIdxInitial + expectedDelta }, 2000)) as {
        usedIdx: number;
        events: Array<{ event: { type: number; code: number; value: number } }>;
      };
      virtioUsedIdxAfter = virtioRead.usedIdx >>> 0;
      virtioEvents = virtioRead.events.slice(virtioUsedIdxInitial, virtioUsedIdxInitial + expectedDelta).map((e) => e.event);

      const drainedAfter = (await callCpu("drainI8042", {}, 2000)) as { bytes: number[] };
      i8042AfterVirtioBytes = drainedAfter.bytes;
    } finally {
      cpuWorker.terminate();
      ioWorker.terminate();
      URL.revokeObjectURL(cpuUrl);
      URL.revokeObjectURL(ioWorkerWrapperUrl);
    }

    return {
      virtioIdFn0,
      virtioUsedIdxInitial,
      virtioUsedIdxAfterHold,
      virtioUsedIdxAfter,
      virtioEvents,
      i8042WhileHeldBytes,
      i8042AfterVirtioBytes,
      keyboardBackendAfterHold,
      keyboardBackendAfterRelease,
      keyboardHeldCountAfterHold,
      keyboardHeldCountAfterRelease,
    };
  });

  expect(result.virtioIdFn0 >>> 0).toBe(0x1052_1af4);

  // While the consumer key is held, backend must stay PS/2 and KeyB scancodes should reach i8042.
  expect(result.keyboardBackendAfterHold).toBe(0); // ps2
  expect(result.keyboardHeldCountAfterHold).toBe(1);
  expect(result.i8042WhileHeldBytes.length).toBeGreaterThan(0);
  expect(result.virtioUsedIdxAfterHold).toBe(result.virtioUsedIdxInitial);

  // After release, backend should switch to virtio.
  expect(result.keyboardHeldCountAfterRelease).toBe(0);
  expect(result.keyboardBackendAfterRelease).toBe(2); // virtio

  // KeyB should now route via virtio-input (and PS/2 scancodes should stop).
  expect(result.i8042AfterVirtioBytes.length).toBe(0);
  expect(result.virtioUsedIdxAfter - result.virtioUsedIdxInitial).toBe(4);
  expect(result.virtioEvents).toEqual([
    { type: 1, code: 48, value: 1 }, // KEY_B press
    { type: 0, code: 0, value: 0 }, // SYN
    { type: 1, code: 48, value: 0 }, // KEY_B release
    { type: 0, code: 0, value: 0 }, // SYN
  ]);
});

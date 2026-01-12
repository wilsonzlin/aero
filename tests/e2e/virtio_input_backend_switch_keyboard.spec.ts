import { expect, test } from "@playwright/test";

test("IO worker switches keyboard input from i8042 scancodes to virtio-input after DRIVER_OK (no duplicates)", async ({ page }) => {
  test.setTimeout(60_000);
  await page.goto("/", { waitUntil: "load" });

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

    const segments = allocateSharedMemorySegments({ guestRamMiB: 256 });
    const views = createSharedMemoryViews(segments);
    const status = views.status;
    const guestBase = views.guestLayout.guest_base >>> 0;

    const ioWorker = new Worker(new URL("/web/src/workers/io.worker.ts", location.href), { type: "module" });

    // io.worker waits for an initial boot disk selection message before reporting READY.
    ioWorker.postMessage({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });
    ioWorker.postMessage({
      kind: "init",
      role: "io",
      controlSab: segments.control,
      guestMemory: segments.guestMemory,
      vgaFramebuffer: segments.vgaFramebuffer,
      ioIpcSab: segments.ioIpc,
      sharedFramebuffer: segments.sharedFramebuffer,
      sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      scanoutState: segments.scanoutState,
      scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    });

    const waitForAtomic = async (idx: number, expected: number, timeoutMs: number): Promise<void> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        if (Atomics.load(status, idx) === expected) return;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`Timed out waiting for status[${idx}] == ${expected} after ${timeoutMs}ms (got ${Atomics.load(status, idx)}).`);
    };

    const waitForIoInputBatchCounter = async (prev: number, timeoutMs: number): Promise<number> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        const cur = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
        if (cur > prev) return cur;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(
        `Timed out waiting for IoInputBatchCounter to advance past ${prev} after ${timeoutMs}ms (still ${Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0}).`,
      );
    };

    const sendKeyboardAInputBatch = (): void => {
      const q = new InputEventQueue(8);
      const nowUs = Math.round(performance.now() * 1000) >>> 0;
      // Include *both* representations (HID usage + PS/2 scancodes). Order is chosen so that by
      // the time the virtio queue has observed the HID events, the scancode events have already
      // been processed (so we can deterministically assert "no i8042 bytes were injected").
      q.pushKeyScancode(nowUs, 0x1c, 1); // make
      q.pushKeyHidUsage(nowUs, 0x04, true); // press
      q.pushKeyScancode(nowUs, 0x1cf0, 2); // break (0xf0 0x1c)
      q.pushKeyHidUsage(nowUs, 0x04, false); // release
      q.flush(
        {
          postMessage: (msg, transfer) => {
            ioWorker.postMessage(msg, transfer);
          },
        },
        { recycle: false },
      );
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

      // Linux input ABI (matches \`crates/aero-virtio/src/devices/input.rs\`).
      const EV_SYN = 0;
      const EV_KEY = 1;
      const SYN_REPORT = 0;
      const KEY_A = 30;

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

      function pciRead16(io, bus, dev, func, off) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, off));
        return io.portRead(PCI_DATA + (off & 2), 2) & 0xffff;
      }

      function pciWrite16(io, bus, dev, func, off, value) {
        io.portWrite(PCI_ADDR, 4, pciAddr(bus, dev, func, off));
        io.portWrite(PCI_DATA + (off & 2), 2, value & 0xffff);
      }

      function drainI8042(io, limit = 4096) {
        const out = [];
        for (let i = 0; i < limit; i += 1) {
          const status = io.portRead(0x64, 1) & 0xff;
          if ((status & 0x01) === 0) break;
          out.push(io.portRead(0x60, 1) & 0xff);
        }
        return out;
      }

      function decodeInputEvent(bytes) {
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        return {
          type: view.getUint16(0, true) >>> 0,
          code: view.getUint16(2, true) >>> 0,
          value: view.getInt32(4, true) | 0,
        };
      }

      function nowMs() {
        return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
      }

      const sleepSab = new SharedArrayBuffer(4);
      const sleepI32 = new Int32Array(sleepSab);

      let io = null;
      let guestBase = 0;
      let guestSab = null;
      let dv = null;

      let virtio = null;

      function guestWriteU16(paddr, value) {
        dv.setUint16(guestBase + (paddr >>> 0), value & 0xffff, true);
      }

      function guestWriteU32(paddr, value) {
        dv.setUint32(guestBase + (paddr >>> 0), value >>> 0, true);
      }

      function guestReadU32(paddr) {
        return dv.getUint32(guestBase + (paddr >>> 0), true) >>> 0;
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

      function mmioReadU8(addr) { return io.mmioRead(addr, 1) & 0xff; }
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
        const id = guestReadU32(base + 0);
        const len = guestReadU32(base + 4);
        return { id, len };
      }

      function readVirtioEvents(maxEvents) {
        const idx = virtioUsedIdx();
        const count = Math.min(idx, maxEvents >>> 0);
        const events = [];
        for (let i = 0; i < count; i += 1) {
          const ent = virtioUsedEntry(i);
          const evBytes = guestReadBytes(virtio.eventBufBase + (ent.id >>> 0) * 8, 8);
          events.push({ id: ent.id >>> 0, len: ent.len >>> 0, event: decodeInputEvent(evBytes), bytes: Array.from(evBytes) });
        }
        return { usedIdx: idx, events };
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

            // Enable memory decoding.
            const cmdReg = pciRead16(io, 0, 10, 0, 0x04);
            pciWrite16(io, 0, 10, 0, 0x04, cmdReg | 0x2);

            const bar0Lo = pciRead32(io, 0, 10, 0, 0x10);
            const bar0Hi = pciRead32(io, 0, 10, 0, 0x14);
            const bar0Base = (BigInt(bar0Hi >>> 0) << 32n) | BigInt(bar0Lo & 0xffff_fff0);

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
            mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK);

            // Queue 0 config (eventq).
            const desc = 0x1000;
            const avail = 0x2000;
            const used = 0x3000;
            const eventBufBase = 0x4000;

            mmioWriteU16(commonBase + 0x16n, 0); // queue_select
            const queueSize = mmioReadU16(commonBase + 0x18n);
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
            for (let i = 0; i < bufferCount; i += 1) {
              guestWriteU32(used + 4 + i * 8 + 0, 0);
              guestWriteU32(used + 4 + i * 8 + 4, 0);
            }

            // Notify queue 0 (notify_off_multiplier is fixed to 4 in contract v1).
            mmioWriteU16(notifyBase + BigInt((notifyOff >>> 0) * 4), 0);

            virtio = { bar0Base, commonBase, notifyBase, desc, avail, used, eventBufBase, queueSize, notifyOff };

            reply(reqId, true, { idFn0, bar0Base: bar0Base.toString(), queueSize, notifyOff, usedIdx: virtioUsedIdx() }, null);
            return;
          }

          if (cmd === "waitForVirtioUsedIdx") {
            if (!virtio) throw new Error("virtio not initialized");
            const initial = msg.initial >>> 0;
            const target = msg.target >>> 0;
            const timeoutMs = msg.timeoutMs >>> 0;
            const start = nowMs();
            for (;;) {
              const cur = virtioUsedIdx();
              if (cur >= target) {
                reply(reqId, true, { initial, target, usedIdx: cur }, null);
                return;
              }
              if (nowMs() - start > timeoutMs) {
                throw new Error("Timed out waiting for virtio used.idx >= " + target + " (still " + cur + ")");
              }
              // Sleep briefly without burning CPU.
              Atomics.wait(sleepI32, 0, 0, 10);
            }
          }

          if (cmd === "readVirtioEvents") {
            if (!virtio) throw new Error("virtio not initialized");
            const maxEvents = msg.maxEvents >>> 0;
            const res = readVirtioEvents(maxEvents);
            reply(reqId, true, res, null);
            return;
          }

          reply(reqId, false, null, "Unknown cmd: " + String(cmd));
        } catch (err) {
          reply(reqId, false, null, err instanceof Error ? err.message : String(err));
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
        (timer as unknown as { unref?: () => void }).unref?.();

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

    let phase1I8042Bytes = 0;
    let phase2I8042Bytes = 0;
    let virtioIdFn0 = 0;
    let virtioUsedIdxInitial = 0;
    let virtioUsedIdxAfter = 0;
    let virtioEvents: Array<{ type: number; code: number; value: number }> = [];

    const drainI8042UntilNonEmpty = async (timeoutMs: number): Promise<{ bytes: number[] }> => {
      const start = typeof performance?.now === "function" ? performance.now() : Date.now();
      while ((typeof performance?.now === "function" ? performance.now() : Date.now()) - start < timeoutMs) {
        const drained = (await callCpu("drainI8042", {}, 2000)) as { bytes: number[] };
        if (drained.bytes.length > 0) return drained;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`Timed out waiting for non-empty i8042 output after ${timeoutMs}ms.`);
    };

    try {
      // Wait until the worker reports READY via the shared status flag.
      await waitForAtomic(StatusIndex.IoReady, 1, 10_000);

      await callCpu("init", { ioIpcSab: segments.ioIpc, guestSab: segments.guestMemory.buffer, guestBase }, 5000);

      // Drain any existing i8042 output bytes so Phase 1 only observes bytes injected by our batch.
      await callCpu("drainI8042", {}, 2000);

      // ---------------------------------------------------------------------
      // Phase 1: virtio driver not OK → PS/2 scancodes should be injected.
      // ---------------------------------------------------------------------
      const batchCounter0 = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      sendKeyboardAInputBatch();
      await waitForIoInputBatchCounter(batchCounter0, 2000);
      const drained1 = await drainI8042UntilNonEmpty(2000);
      phase1I8042Bytes = drained1.bytes.length;

      // ---------------------------------------------------------------------
      // Phase 2: guest sets DRIVER_OK + configures eventq → PS/2 injection stops and events flow via virtio.
      // ---------------------------------------------------------------------
      const virtioInit = (await callCpu("virtioInit", {}, 5000)) as {
        idFn0: number;
        usedIdx: number;
      };
      virtioIdFn0 = virtioInit.idFn0 >>> 0;
      virtioUsedIdxInitial = virtioInit.usedIdx >>> 0;

      const batchCounter1 = Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0;
      sendKeyboardAInputBatch();
      await waitForIoInputBatchCounter(batchCounter1, 2000);

      // We expect at least the key-press pair (EV_KEY + EV_SYN). Some implementations may also
      // deliver the release pair immediately (total 4 events); allow both to keep the test stable
      // while still proving that virtio-input is the active path.
      const expectedUsedDeltaMin = 2;
      const expectedUsedDeltaMax = 4;
      await callCpu(
        "waitForVirtioUsedIdx",
        { initial: virtioUsedIdxInitial, target: virtioUsedIdxInitial + expectedUsedDeltaMin, timeoutMs: 2000 },
        3000,
      );

      // Give the device a brief grace period to flush any additional events (e.g. release) when
      // enough buffers are available.
      let virtioRead = (await callCpu("readVirtioEvents", { maxEvents: virtioUsedIdxInitial + expectedUsedDeltaMax }, 2000)) as {
        usedIdx: number;
        events: Array<{ event: { type: number; code: number; value: number } }>;
      };
      if ((virtioRead.usedIdx >>> 0) < virtioUsedIdxInitial + expectedUsedDeltaMax) {
        const start = performance.now();
        while (performance.now() - start < 200) {
          await new Promise((resolve) => setTimeout(resolve, 10));
          virtioRead = (await callCpu("readVirtioEvents", { maxEvents: virtioUsedIdxInitial + expectedUsedDeltaMax }, 2000)) as typeof virtioRead;
          if ((virtioRead.usedIdx >>> 0) >= virtioUsedIdxInitial + expectedUsedDeltaMax) break;
        }
      }
      virtioUsedIdxAfter = virtioRead.usedIdx >>> 0;
      virtioEvents = virtioRead.events.map((e) => e.event);

      const drained2 = (await callCpu("drainI8042", {}, 2000)) as { bytes: number[] };
      phase2I8042Bytes = drained2.bytes.length;

      // Ensure we got at least the press+sync pair; release is optional (see comment above).
      if ((virtioUsedIdxAfter - virtioUsedIdxInitial) >>> 0 < expectedUsedDeltaMin) {
        throw new Error(
          `virtio used.idx did not advance by at least ${expectedUsedDeltaMin} (initial=${virtioUsedIdxInitial} after=${virtioUsedIdxAfter})`,
        );
      }
      if ((virtioUsedIdxAfter - virtioUsedIdxInitial) >>> 0 > expectedUsedDeltaMax) {
        throw new Error(
          `virtio used.idx advanced too far (expected <=${expectedUsedDeltaMax}): initial=${virtioUsedIdxInitial} after=${virtioUsedIdxAfter}`,
        );
      }
    } finally {
      cpuWorker.terminate();
      ioWorker.terminate();
      URL.revokeObjectURL(cpuUrl);
    }

    return {
      phase1I8042Bytes,
      phase2I8042Bytes,
      virtioIdFn0,
      virtioUsedIdxInitial,
      virtioUsedIdxAfter,
      virtioEvents,
    };
  });

  expect(result.virtioIdFn0 >>> 0).toBe(0x1052_1af4);

  // Phase 1: before virtio DRIVER_OK, scancode injection should reach i8042.
  expect(result.phase1I8042Bytes).toBeGreaterThan(0);

  // Phase 2: after virtio driver OK, scancode injection must stop.
  expect(result.phase2I8042Bytes).toBe(0);

  // Phase 2: virtio eventq should receive EV_KEY/EV_SYN pairs for press and release.
  const delta = result.virtioUsedIdxAfter - result.virtioUsedIdxInitial;
  expect([2, 4]).toContain(delta);
  expect(result.virtioEvents.slice(0, 2)).toEqual([
    { type: 1, code: 30, value: 1 },
    { type: 0, code: 0, value: 0 },
  ]);
  if (delta === 4) {
    expect(result.virtioEvents.slice(2, 4)).toEqual([
      { type: 1, code: 30, value: 0 },
      { type: 0, code: 0, value: 0 },
    ]);
  }
});

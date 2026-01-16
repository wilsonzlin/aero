import { expect, test } from "@playwright/test";

import { checkThreadedWasmBundle } from "./util/wasm_bundle";
import { DEFAULT_EXTERNAL_HUB_PORT_COUNT, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT } from "../../web/src/usb/uhci_external_hub";

test("runtime UHCI: WebHID + WebUSB passthrough are guest-visible (NAK while pending)", async ({ page }) => {
  // This spec exercises a full UHCI runtime stack (PCI config, control transfers, interrupt IN/OUT,
  // and WebUSB proxying) entirely inside browser workers. Some engines (and CI VMs) can be slow
  // enough that the TD-level polling loops take >20s, so keep the per-test budget generous.
  test.setTimeout(90_000);

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
      wasmThreads,
    };
  });

  test.skip(!support.crossOriginIsolated || !support.sharedArrayBuffer, "SharedArrayBuffer requires COOP/COEP headers.");
  test.skip(!support.atomics || !support.wasmThreads, "Shared WebAssembly.Memory (WASM threads) is unavailable.");

  const result = await page.evaluate(async (opts) => {
    const { dynamicHubPort, externalHubPortCount } = opts as { dynamicHubPort: number; externalHubPortCount: number };
    const { allocateSharedMemorySegments, createSharedMemoryViews } = await import("/web/src/runtime/shared_layout.ts");
    const { MessageType } = await import("/web/src/runtime/protocol.ts");
    const { emptySetBootDisksMessage } = await import("/web/src/runtime/boot_disks_protocol.ts");

    // This spec uses a few low guest RAM addresses for UHCI frame/TD buffers, so a minimal guest
    // RAM size is sufficient and keeps parallel Playwright runs from allocating unnecessary memory.
    const segments = allocateSharedMemorySegments({
      guestRamMiB: 1,
      vramMiB: 0,
      ioIpcOptions: { includeNet: false, includeHidIn: false },
      sharedFramebufferLayout: { width: 1, height: 1, tileSize: 0 },
    });
    const views = createSharedMemoryViews(segments);

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

    const ioImported = new Promise<void>((resolve, reject) => {
      let timer = 0;
      const cleanup = () => {
        if (timer) clearTimeout(timer);
        ioWorker.removeEventListener("message", messageHandler);
        ioWorker.removeEventListener("error", errorHandler);
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

      ioWorker.addEventListener("message", messageHandler);
      ioWorker.addEventListener("error", errorHandler);
      timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for io.worker import marker"));
      }, 20_000);
    });

    let ioReady = false;
    let ioWasmReady = false;
    let ioError: string | null = null;

    const usbActions: unknown[] = [];
    let guestUsbStatus: unknown | null = null;
    let hidSendReport: { deviceId: number; reportType: string; reportId: number; data: number[] } | null = null;
    let hidAttachResult: { deviceId: number; ok: boolean; error?: string } | null = null;

    ioWorker.onmessage = (ev) => {
      const data = ev.data as any;
      if (!data) return;

      if (data.type === MessageType.READY && data.role === "io") {
        ioReady = true;
        return;
      }
      if (data.type === MessageType.WASM_READY && data.role === "io") {
        ioWasmReady = true;
        return;
      }
      if (data.type === MessageType.ERROR && data.role === "io") {
        ioError = typeof data.message === "string" ? data.message : String(data.message);
        return;
      }
      if (data.type === "usb.action") {
        usbActions.push(data.action);
        return;
      }
      if (data.type === "usb.guest.status") {
        guestUsbStatus = data.snapshot;
        return;
      }
      if (data.type === "hid.sendReport") {
        const report = data as { deviceId?: unknown; reportType?: unknown; reportId?: unknown; data?: unknown };
        if (typeof report.deviceId !== "number" || typeof report.reportType !== "string" || typeof report.reportId !== "number") return;
        if (!(report.data instanceof Uint8Array)) return;
        hidSendReport = {
          deviceId: report.deviceId,
          reportType: report.reportType,
          reportId: report.reportId,
          data: Array.from(report.data),
        };
        return;
      }
      if (data.type === "hid.attachResult") {
        const msg = data as { deviceId?: unknown; ok?: unknown; error?: unknown };
        if (typeof msg.deviceId !== "number" || typeof msg.ok !== "boolean") return;
        hidAttachResult = { deviceId: msg.deviceId, ok: msg.ok, ...(typeof msg.error === "string" ? { error: msg.error } : {}) };
        return;
      }
    };

    // Avoid dropping early messages on WebKit by waiting until the imported worker module has run.
    await ioImported;

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

    const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
    const waitFor = async (predicate: () => boolean, timeoutMs: number, name: string) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        if (ioError) throw new Error(`io.worker error while waiting for ${name}: ${ioError}`);
        if (predicate()) return;
        await sleep(5);
      }
      throw new Error(`Timed out waiting for ${name}`);
    };

    await waitFor(() => ioReady, 10_000, "io READY");
    await waitFor(() => ioWasmReady, 20_000, "io WASM_READY");

    const guestSab = segments.guestMemory.buffer as unknown as SharedArrayBuffer;

     const guestWorkerCode = `
        import { openRingByKind } from "${location.origin}/web/src/ipc/ipc.ts";
        import { IO_IPC_CMD_QUEUE_KIND, IO_IPC_EVT_QUEUE_KIND } from "${location.origin}/web/src/runtime/shared_layout.ts";
        import { AeroIpcIoClient } from "${location.origin}/web/src/io/ipc/aero_ipc_io.ts";

        let UHCI_BASE = 0;
        const HUB_DYNAMIC_PORT = ${dynamicHubPort};

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

       function findUhciDevice(io) {
         for (let dev = 0; dev < 32; dev++) {
           const id = pciRead32(io, 0, dev, 0, 0x00);
           const vendorId = id & 0xffff;
           const deviceId = (id >>> 16) & 0xffff;
           if (vendorId === 0xffff) continue;
           if (vendorId === 0x8086 && deviceId === 0x7112) return dev;
         }
         return -1;
       }

       const REG_USBCMD = 0x00;
       const REG_USBINTR = 0x04;
       const REG_FRNUM = 0x06;
       const REG_FRBASEADD = 0x08;
       const REG_PORTSC1 = 0x10;
      const REG_PORTSC2 = 0x12;

      const USBCMD_RUN = 1 << 0;
      const USBCMD_MAXP = 1 << 7;
      const USBINTR_IOC = 1 << 2;

      const PORTSC_CCS = 1 << 0;
      const PORTSC_PED = 1 << 2;

      const LINK_PTR_T = 1 << 0;
       const LINK_PTR_Q = 1 << 1;
 
        const TD_CTRL_NAK = 1 << 19;
        const TD_CTRL_BITSTUFF = 1 << 17;
        const TD_CTRL_CRC_TIMEOUT = 1 << 18;
        const TD_CTRL_BABBLE = 1 << 20;
        const TD_CTRL_DBUFERR = 1 << 21;
        const TD_CTRL_STALL = 1 << 22;
        const TD_CTRL_ACTIVE = 1 << 23;
        const TD_CTRL_IOC = 1 << 24;
        const TD_CTRL_ACTLEN_MASK = 0x7ff;

        function assertTdOk(ctrl, context) {
          const err = ctrl & (TD_CTRL_STALL | TD_CTRL_DBUFERR | TD_CTRL_BABBLE | TD_CTRL_CRC_TIMEOUT | TD_CTRL_BITSTUFF);
          if (err === 0) return;
          throw new Error(
            (context || "TD") +
              " failed (ctrl=0x" +
              (ctrl >>> 0).toString(16) +
              " err=0x" +
              (err >>> 0).toString(16) +
              ")",
          );
        }

        function findInterruptInEndpoint(configDesc) {
          const total = ((configDesc[2] ?? 0) | (((configDesc[3] ?? 0) & 0xff) << 8)) >>> 0;
          const limit = Math.min(total || configDesc.length, configDesc.length);
          let off = 0;
          while (off + 2 <= limit) {
            const len = configDesc[off] ?? 0;
            const type = configDesc[off + 1] ?? 0;
            if (len <= 0) break;
            if (type === 5 && off + 7 <= limit) {
              const addr = configDesc[off + 2] ?? 0;
              const attrs = configDesc[off + 3] ?? 0;
              const max = ((configDesc[off + 4] ?? 0) | (((configDesc[off + 5] ?? 0) & 0xff) << 8)) >>> 0;
              const isInterrupt = (attrs & 0x03) === 0x03;
              const isIn = (addr & 0x80) !== 0;
              if (isInterrupt && isIn) {
                return { ep: addr & 0x0f, maxPacketSize: max };
              }
            }
            off += len;
          }
          return null;
        }

      const TD_TOKEN_DEVADDR_SHIFT = 8;
      const TD_TOKEN_ENDPT_SHIFT = 15;
      const TD_TOKEN_MAXLEN_SHIFT = 21;

      const PID_SETUP = 0x2d;
      const PID_IN = 0x69;
      const PID_OUT = 0xe1;

      const sleepArr = new Int32Array(new SharedArrayBuffer(4));
      function sleep(ms) {
        Atomics.wait(sleepArr, 0, 0, ms);
      }

      function le16(v) {
        return [v & 0xff, (v >>> 8) & 0xff];
      }

      function setupPacketBytes(pkt) {
        return Uint8Array.from([
          pkt.bmRequestType & 0xff,
          pkt.bRequest & 0xff,
          ...le16(pkt.wValue >>> 0),
          ...le16(pkt.wIndex >>> 0),
          ...le16(pkt.wLength >>> 0),
        ]);
      }

      function tdToken(pid, devAddr, endpt, maxLen) {
        const maxlenField = maxLen === 0 ? (0x7ff << TD_TOKEN_MAXLEN_SHIFT) : ((maxLen - 1) << TD_TOKEN_MAXLEN_SHIFT);
        return (
          (pid & 0xff) |
          ((devAddr & 0x7f) << TD_TOKEN_DEVADDR_SHIFT) |
          ((endpt & 0x0f) << TD_TOKEN_ENDPT_SHIFT) |
          (maxlenField >>> 0)
        ) >>> 0;
      }

      function writeU32(dv, paddr, value) {
        dv.setUint32(paddr >>> 0, value >>> 0, true);
      }

      function readU32(dv, paddr) {
        return dv.getUint32(paddr >>> 0, true) >>> 0;
      }

       const PORTSC_PR = 1 << 9;

       function portReg(portIndex) {
         return portIndex === 0 ? REG_PORTSC1 : REG_PORTSC2;
       }

       function readPortsc(io, portIndex) {
         return io.portRead(UHCI_BASE + portReg(portIndex), 2) & 0xffff;
       }

       function findConnectedPort(io) {
         const p1 = readPortsc(io, 0);
         if (p1 & PORTSC_CCS) return 0;
         const p2 = readPortsc(io, 1);
         if (p2 & PORTSC_CCS) return 1;
         return -1;
       }

       function disablePort(io, portIndex) {
         io.portWrite(UHCI_BASE + portReg(portIndex), 2, 0);
       }

        function resetPort(io, portIndex) {
          const reg = portReg(portIndex);
          io.portWrite(UHCI_BASE + reg, 2, PORTSC_PR);
          // UHCI model completes reset asynchronously after ~50ms (step_frame ticks), but some
          // engines (notably WebKit in CI) can run the IO tick loop with aggressive timer clamping.
          // Keep this delay conservative so the device has time to re-appear.
          sleep(200);
        }

       function enablePort(io, portIndex) {
         const reg = portReg(portIndex);
         io.portWrite(UHCI_BASE + reg, 2, PORTSC_PED);
       }

      function setupFrameListAndQh(dv) {
        const FRAME_LIST = 0x1000;
        const QH = 0x2000;
        for (let i = 0; i < 1024; i++) {
          writeU32(dv, FRAME_LIST + i * 4, (QH | LINK_PTR_Q) >>> 0);
        }
        // QH head = terminate, element = TD_SETUP (filled later)
        writeU32(dv, QH + 0x00, LINK_PTR_T);
        writeU32(dv, QH + 0x04, LINK_PTR_T);
        return { FRAME_LIST, QH };
      }

       function setupControlInChain(dv, setupBytes, inLen, devAddr) {
         const TD_SETUP = 0x3000;
         const TD_IN = 0x3010;
         const TD_STATUS = 0x3020;
         const BUF_SETUP = 0x4000;
         const BUF_DATA = 0x4100;

        // Copy setup packet bytes.
        new Uint8Array(dv.buffer, dv.byteOffset + BUF_SETUP, 8).set(setupBytes);

        // TD: SETUP
         writeU32(dv, TD_SETUP + 0x00, TD_IN);
         writeU32(dv, TD_SETUP + 0x04, (TD_CTRL_ACTIVE | 0x7ff) >>> 0);
         writeU32(dv, TD_SETUP + 0x08, tdToken(PID_SETUP, devAddr, 0, 8));
         writeU32(dv, TD_SETUP + 0x0c, BUF_SETUP);

         // TD: IN (data)
         writeU32(dv, TD_IN + 0x00, TD_STATUS);
         writeU32(dv, TD_IN + 0x04, (TD_CTRL_ACTIVE | 0x7ff) >>> 0);
         writeU32(dv, TD_IN + 0x08, tdToken(PID_IN, devAddr, 0, inLen));
         writeU32(dv, TD_IN + 0x0c, BUF_DATA);

         // TD: OUT (status)
         writeU32(dv, TD_STATUS + 0x00, LINK_PTR_T);
         writeU32(dv, TD_STATUS + 0x04, (TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7ff) >>> 0);
         writeU32(dv, TD_STATUS + 0x08, tdToken(PID_OUT, devAddr, 0, 0));
         writeU32(dv, TD_STATUS + 0x0c, 0);

         return { TD_SETUP, TD_IN, TD_STATUS, BUF_DATA };
       }

       function setupControlNoDataChain(dv, setupBytes, devAddr) {
         const TD_SETUP = 0x3000;
         const TD_STATUS = 0x3020;
         const BUF_SETUP = 0x4000;

         // Copy setup packet bytes.
         new Uint8Array(dv.buffer, dv.byteOffset + BUF_SETUP, 8).set(setupBytes);

         // TD: SETUP
         writeU32(dv, TD_SETUP + 0x00, TD_STATUS);
         writeU32(dv, TD_SETUP + 0x04, (TD_CTRL_ACTIVE | 0x7ff) >>> 0);
         writeU32(dv, TD_SETUP + 0x08, tdToken(PID_SETUP, devAddr, 0, 8));
         writeU32(dv, TD_SETUP + 0x0c, BUF_SETUP);

         // TD: IN (status)
         writeU32(dv, TD_STATUS + 0x00, LINK_PTR_T);
         writeU32(dv, TD_STATUS + 0x04, (TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7ff) >>> 0);
         writeU32(dv, TD_STATUS + 0x08, tdToken(PID_IN, devAddr, 0, 0));
         writeU32(dv, TD_STATUS + 0x0c, 0);

         return { TD_SETUP, TD_STATUS };
       }

       function waitForTdInactive(dv, tdAddr, timeoutMs) {
          const start = performance.now();
          while (performance.now() - start < timeoutMs) {
            const ctrl = readU32(dv, tdAddr + 0x04);
            if ((ctrl & TD_CTRL_ACTIVE) === 0) return ctrl >>> 0;
            sleep(1);
          }
          const ctrl = readU32(dv, tdAddr + 0x04);
          throw new Error(
            "timeout waiting for TD to complete (td=0x" +
              (tdAddr >>> 0).toString(16) +
              " ctrl=0x" +
              (ctrl >>> 0).toString(16) +
              " timeoutMs=" +
              timeoutMs +
              ")",
          );
        }

       function runControlIn(io, dv, QH, setup, inLen, devAddr) {
          const setupBytes = setupPacketBytes(setup);
          const chain = setupControlInChain(dv, setupBytes, inLen, devAddr);
          writeU32(dv, QH + 0x04, chain.TD_SETUP);
          const statusCtrl = waitForTdInactive(dv, chain.TD_STATUS, 15_000);
          assertTdOk(statusCtrl, "control-in status TD");
          return Array.from(new Uint8Array(dv.buffer, dv.byteOffset + chain.BUF_DATA, inLen));
        }
 
        function runControlNoData(io, dv, QH, setup, devAddr) {
          const setupBytes = setupPacketBytes(setup);
          const chain = setupControlNoDataChain(dv, setupBytes, devAddr);
          writeU32(dv, QH + 0x04, chain.TD_SETUP);
          const statusCtrl = waitForTdInactive(dv, chain.TD_STATUS, 15_000);
          assertTdOk(statusCtrl, "control-no-data status TD");
        }

        self.onmessage = (ev) => {
          try {
          const { ioIpc, guestSab, mode, guestBase, guestSize, setup, inLen, forcedPortIndex } = ev.data;

         const cmdQ = openRingByKind(ioIpc, IO_IPC_CMD_QUEUE_KIND);
         const evtQ = openRingByKind(ioIpc, IO_IPC_EVT_QUEUE_KIND);
         const io = new AeroIpcIoClient(cmdQ, evtQ);

         const dv = new DataView(guestSab, guestBase, guestSize);

         // Enable I/O decoding for UHCI and discover its I/O base from BAR4.
         const uhciDev = findUhciDevice(io);
         if (uhciDev === -1) {
           self.postMessage({ type: "error", message: "UHCI PCI device not found" });
           return;
         }
         const bar4 = pciRead32(io, 0, uhciDev, 0, 0x20) >>> 0;
         UHCI_BASE = bar4 & 0xfffc;
         if (UHCI_BASE === 0) {
           self.postMessage({ type: "error", message: "UHCI BAR4 base is 0" });
           return;
         }
         // Command register: enable I/O decoding (bit0=1) + bus mastering (bit2=1).
         pciWrite32(io, 0, uhciDev, 0, 0x04, 0x0005);

         // Halt+reset the controller so repeated test phases don't race each other.
         io.portWrite(UHCI_BASE + REG_USBCMD, 2, 0);
         io.portWrite(UHCI_BASE + REG_USBCMD, 2, 1 << 1);

         let portIndex = typeof forcedPortIndex === "number" ? forcedPortIndex : -1;
         if (portIndex !== 0 && portIndex !== 1) {
           // Find which root port is connected.
           const deadline = performance.now() + 5000;
           while (performance.now() < deadline) {
             portIndex = findConnectedPort(io);
             if (portIndex !== -1) break;
             sleep(5);
           }
           if (portIndex === -1) {
             self.postMessage({ type: "error", message: "no UHCI root port reports a connected device" });
             return;
           }
         } else {
           // Wait for the expected port to report CCS so we talk to the correct device.
           const deadline = performance.now() + 5000;
           while (performance.now() < deadline) {
             if (readPortsc(io, portIndex) & PORTSC_CCS) break;
             sleep(5);
           }
           if ((readPortsc(io, portIndex) & PORTSC_CCS) === 0) {
             const p0 = readPortsc(io, 0);
             const p1 = readPortsc(io, 1);
             self.postMessage({
               type: "error",
               message: \`forcedPortIndex=\${portIndex} never reported CCS (PORTSC1=0x\${p0.toString(16)} PORTSC2=0x\${p1.toString(16)})\`,
             });
             return;
           }
         }

         // Ensure only the selected port is enabled so addr0 requests don't hit a different device.
         disablePort(io, portIndex ^ 1);
         resetPort(io, portIndex);
         enablePort(io, portIndex);

         // Frame list + QH.
         const { FRAME_LIST, QH } = setupFrameListAndQh(dv);

         // Note: each control transfer rewrites the TD chain and resets QH.element.

         // Program UHCI registers.
          io.portWrite(UHCI_BASE + REG_USBINTR, 2, USBINTR_IOC);
          io.portWrite(UHCI_BASE + REG_FRNUM, 2, 0);
         io.portWrite(UHCI_BASE + REG_FRBASEADD, 4, FRAME_LIST);
         io.portWrite(UHCI_BASE + REG_USBCMD, 2, (USBCMD_RUN | USBCMD_MAXP) >>> 0);
          const fr0 = io.portRead(UHCI_BASE + REG_FRNUM, 2) & 0xffff;
          let fr1 = fr0;
          const frDeadline = performance.now() + 1000;
          while (performance.now() < frDeadline) {
            sleep(10);
            fr1 = io.portRead(UHCI_BASE + REG_FRNUM, 2) & 0xffff;
            if (fr1 !== fr0) break;
          }
          if (fr0 === fr1) {
            self.postMessage({ type: "error", message: "UHCI FRNUM did not advance after RUN" });
            return;
          }
 
          if (mode === "hidConfig") {
            const first = runControlIn(io, dv, QH, setup, inLen, 0);
            // Interface descriptor begins at offset 9; bInterfaceClass is at offset 9+5.
            const ifaceClass = first[14] ?? 0;
            if (ifaceClass === 0x03) {
              self.postMessage({ type: "hid.result", portIndex, data: first });
              return;
            }

            // Newer UHCI runtime builds attach WebHID passthrough devices behind an emulated hub
            // on root port 0. In that case, first is the hub (class 0x09) and we must enumerate it
            // enough to power+reset downstream port 1 before addressing the HID device at addr0.
            const USB_REQUEST_SET_ADDRESS = 0x05;
            const USB_REQUEST_SET_CONFIGURATION = 0x09;
            const USB_REQUEST_SET_FEATURE = 0x03;
            const HUB_PORT_FEATURE_POWER = 8;
            const HUB_PORT_FEATURE_RESET = 4;

            if (ifaceClass !== 0x09) {
              self.postMessage({ type: "error", message: \`unexpected USB interface class 0x\${ifaceClass.toString(16)} (wanted HID=0x03 or Hub=0x09)\` });
              return;
            }

            runControlNoData(io, dv, QH, { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_ADDRESS, wValue: 1, wIndex: 0, wLength: 0 }, 0);
            runControlNoData(io, dv, QH, { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_CONFIGURATION, wValue: 1, wIndex: 0, wLength: 0 }, 1);
             // Port numbers are 1-based for hub class requests.
             runControlNoData(io, dv, QH, { bmRequestType: 0x23, bRequest: USB_REQUEST_SET_FEATURE, wValue: HUB_PORT_FEATURE_POWER, wIndex: HUB_DYNAMIC_PORT, wLength: 0 }, 1);
             runControlNoData(io, dv, QH, { bmRequestType: 0x23, bRequest: USB_REQUEST_SET_FEATURE, wValue: HUB_PORT_FEATURE_RESET, wIndex: HUB_DYNAMIC_PORT, wLength: 0 }, 1);
              sleep(200);

             const data = runControlIn(io, dv, QH, setup, inLen, 0);
            self.postMessage({ type: "hid.result", portIndex, data });
            return;
          }

           if (mode === "hidInterruptIn") {
            const first = runControlIn(io, dv, QH, setup, inLen, 0);
            let hidConfigDesc = first;
            // Interface descriptor begins at offset 9; bInterfaceClass is at offset 9+5.
            const ifaceClass = first[14] ?? 0;

             const USB_REQUEST_SET_ADDRESS = 0x05;
             const USB_REQUEST_SET_CONFIGURATION = 0x09;
             const USB_REQUEST_SET_FEATURE = 0x03;
             const HUB_PORT_FEATURE_POWER = 8;
             const HUB_PORT_FEATURE_RESET = 4;

            if (ifaceClass === 0x09) {
              // Enumerate the hub at addr0 so the downstream HID device at addr0 becomes reachable.
              runControlNoData(
                io,
                dv,
                QH,
                { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_ADDRESS, wValue: 1, wIndex: 0, wLength: 0 },
                0,
              );
              runControlNoData(
                io,
                dv,
                QH,
                { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_CONFIGURATION, wValue: 1, wIndex: 0, wLength: 0 },
                1,
              );
               // Port numbers are 1-based for hub class requests.
               runControlNoData(
                 io,
                 dv,
                 QH,
                 { bmRequestType: 0x23, bRequest: USB_REQUEST_SET_FEATURE, wValue: HUB_PORT_FEATURE_POWER, wIndex: HUB_DYNAMIC_PORT, wLength: 0 },
                 1,
               );
               runControlNoData(
                 io,
                 dv,
                 QH,
                 { bmRequestType: 0x23, bRequest: USB_REQUEST_SET_FEATURE, wValue: HUB_PORT_FEATURE_RESET, wIndex: HUB_DYNAMIC_PORT, wLength: 0 },
                 1,
               );
                sleep(200);

              // Fetch the downstream HID config descriptor after resetting the dynamic hub port so we can
              // discover its interrupt endpoint number.
              hidConfigDesc = runControlIn(io, dv, QH, setup, inLen, 0);
             } else if (ifaceClass !== 0x03) {
               self.postMessage({
                 type: "error",
                 message:
                  "unexpected USB interface class 0x" +
                  ifaceClass.toString(16) +
                  " (wanted HID=0x03 or Hub=0x09)",
              });
              return;
            }

             // Now enumerate the HID device at address 0.
             const HID_ADDR = 2;
            const epInfo = findInterruptInEndpoint(hidConfigDesc);
            if (!epInfo) {
              self.postMessage({ type: "error", message: "interrupt IN endpoint not found in HID config descriptor" });
              return;
            }
            const intEp = epInfo.ep >>> 0;
            const intMaxLen = Math.max(1, Math.min(64, epInfo.maxPacketSize >>> 0 || 8));

             runControlNoData(
               io,
               dv,
               QH,
               { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_ADDRESS, wValue: HID_ADDR, wIndex: 0, wLength: 0 },
               0,
             );
            // Allow the device time to begin responding to the new address.
            sleep(20);
             runControlNoData(
               io,
               dv,
               QH,
               { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_CONFIGURATION, wValue: 1, wIndex: 0, wLength: 0 },
               HID_ADDR,
             );
            sleep(20);

             // Schedule a single interrupt-IN TD. It should NAK while there are no pending input reports.
             const TD_INT = 0x3030;
             const BUF_INT = 0x4200;
             writeU32(dv, TD_INT + 0x00, LINK_PTR_T);
             writeU32(dv, TD_INT + 0x04, (TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7ff) >>> 0);
            writeU32(dv, TD_INT + 0x08, tdToken(PID_IN, HID_ADDR, intEp, intMaxLen));
             writeU32(dv, TD_INT + 0x0c, BUF_INT);
            new Uint8Array(dv.buffer, dv.byteOffset + BUF_INT, intMaxLen).fill(0);
             writeU32(dv, QH + 0x04, TD_INT);

            const start = performance.now();
            let nakNotified = false;
             while (performance.now() - start < 20_000) {
               const ctrl = readU32(dv, TD_INT + 0x04);
               if (!nakNotified && (ctrl & TD_CTRL_ACTIVE) !== 0 && (ctrl & TD_CTRL_NAK) !== 0) {
                 nakNotified = true;
                 self.postMessage({ type: "hid.interruptNakObserved", ctrl });
               }
               if ((ctrl & TD_CTRL_ACTIVE) === 0) break;
               sleep(1);
             }
 
             const ctrlFinal = waitForTdInactive(dv, TD_INT, 20_000);
             assertTdOk(ctrlFinal, "interrupt-IN TD");
             const actLen = ctrlFinal & TD_CTRL_ACTLEN_MASK;
             const bytes = actLen === 0x7ff ? 0 : (actLen + 1);
             const data = Array.from(new Uint8Array(dv.buffer, dv.byteOffset + BUF_INT, bytes));
             self.postMessage({ type: "hid.interruptResult", portIndex, data, nakObserved: nakNotified, ctrlFinal });
             return;
           }

          if (mode === "hidInterruptOut") {
            const { outData } = ev.data;
            const outBytes = Array.isArray(outData) ? Uint8Array.from(outData) : new Uint8Array();

            const first = runControlIn(io, dv, QH, setup, inLen, 0);
            // Interface descriptor begins at offset 9; bInterfaceClass is at offset 9+5.
            const ifaceClass = first[14] ?? 0;

            const USB_REQUEST_SET_ADDRESS = 0x05;
            const USB_REQUEST_SET_CONFIGURATION = 0x09;
            const USB_REQUEST_SET_FEATURE = 0x03;
            const HUB_PORT_FEATURE_POWER = 8;
            const HUB_PORT_FEATURE_RESET = 4;

            if (ifaceClass === 0x09) {
              // Enumerate the hub at addr0 so the downstream HID device at addr0 becomes reachable.
              runControlNoData(
                io,
                dv,
                QH,
                { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_ADDRESS, wValue: 1, wIndex: 0, wLength: 0 },
                0,
              );
              runControlNoData(
                io,
                dv,
                QH,
                { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_CONFIGURATION, wValue: 1, wIndex: 0, wLength: 0 },
                1,
              );
               // Port numbers are 1-based for hub class requests.
               runControlNoData(
                 io,
                 dv,
                 QH,
                 { bmRequestType: 0x23, bRequest: USB_REQUEST_SET_FEATURE, wValue: HUB_PORT_FEATURE_POWER, wIndex: HUB_DYNAMIC_PORT, wLength: 0 },
                 1,
               );
               runControlNoData(
                 io,
                 dv,
                 QH,
                 { bmRequestType: 0x23, bRequest: USB_REQUEST_SET_FEATURE, wValue: HUB_PORT_FEATURE_RESET, wIndex: HUB_DYNAMIC_PORT, wLength: 0 },
                 1,
               );
                sleep(200);
            } else if (ifaceClass !== 0x03) {
              self.postMessage({
                type: "error",
                message:
                  "unexpected USB interface class 0x" +
                  ifaceClass.toString(16) +
                  " (wanted HID=0x03 or Hub=0x09)",
              });
              return;
            }

            // Now enumerate the HID device at address 0.
            const HID_ADDR = 2;
            runControlNoData(
              io,
              dv,
              QH,
              { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_ADDRESS, wValue: HID_ADDR, wIndex: 0, wLength: 0 },
              0,
            );
            runControlNoData(
              io,
              dv,
              QH,
              { bmRequestType: 0x00, bRequest: USB_REQUEST_SET_CONFIGURATION, wValue: 1, wIndex: 0, wLength: 0 },
              HID_ADDR,
            );

            // Schedule a single interrupt-OUT TD (endpoint 1). The guest should get an ACK and the
            // host should observe an output report via 'webhid_drain_output_reports'.
            const TD_OUT = 0x3030;
            const BUF_OUT = 0x4200;
            const outLen = outBytes.length;

            new Uint8Array(dv.buffer, dv.byteOffset + BUF_OUT, outLen).set(outBytes);

            writeU32(dv, TD_OUT + 0x00, LINK_PTR_T);
            writeU32(dv, TD_OUT + 0x04, (TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7ff) >>> 0);
            writeU32(dv, TD_OUT + 0x08, tdToken(PID_OUT, HID_ADDR, 1, outLen));
            writeU32(dv, TD_OUT + 0x0c, BUF_OUT);

            writeU32(dv, QH + 0x04, TD_OUT);

             const ctrlFinal = waitForTdInactive(dv, TD_OUT, 20_000);
             self.postMessage({ type: "hid.outResult", portIndex, ctrlFinal, outLen });
             return;
           }

          if (mode === "webusbDevice") {
            const setupBytes = setupPacketBytes(setup);
            const chain = setupControlInChain(dv, setupBytes, inLen, 0);
            writeU32(dv, QH + 0x04, chain.TD_SETUP);

            const start = performance.now();
            let nakNotified = false;
             while (performance.now() - start < 15_000) {
               const ctrl = readU32(dv, chain.TD_IN + 0x04);
               if (!nakNotified && (ctrl & TD_CTRL_ACTIVE) !== 0 && (ctrl & TD_CTRL_NAK) !== 0) {
                 const qhElem = readU32(dv, QH + 0x04);
                 nakNotified = true;
                 self.postMessage({ type: "webusb.nakObserved", ctrl, qhElem });
               }
               if ((ctrl & TD_CTRL_ACTIVE) === 0) break;
               sleep(1);
             }
 
             waitForTdInactive(dv, chain.TD_STATUS, 15_000);
             const data = Array.from(new Uint8Array(dv.buffer, dv.byteOffset + chain.BUF_DATA, inLen));
             const inCtrlFinal = readU32(dv, chain.TD_IN + 0x04);
             const portsc = [readPortsc(io, 0), readPortsc(io, 1)];
             self.postMessage({ type: "webusb.result", portIndex, data, nakObserved: nakNotified, inCtrlFinal, portsc });
             return;
           }

        self.postMessage({ type: "error", message: "unknown mode" });
          } catch (err) {
            const msg = err instanceof Error ? err.message : err;
            const message = String(msg ?? "Error")
              .replace(/[\\x00-\\x1F\\x7F]/g, " ")
              .replace(/\\s+/g, " ")
              .trim()
              .slice(0, 512);
            self.postMessage({ type: "error", message });
          }
      };
    `;

    const guestUrl = URL.createObjectURL(new Blob([guestWorkerCode], { type: "text/javascript" }));

    const createGuestWorker = () => {
      // Similar to the io.worker wrapper: WebKit can be flaky when the *entrypoint* module worker is large.
      // Import the real worker module from a tiny wrapper and wait for a marker before sending messages.
      const entrypoint = guestUrl;
      const wrapperUrl = URL.createObjectURL(
        new Blob(
          [
            `\n              (async () => {\n                const MAX_ERROR_CHARS = 512;\n                const fallbackFormatErr = (err) => {\n                  const msg = err instanceof Error ? err.message : err;\n                  return String(msg ?? \"Error\")\n                    .replace(/[\\x00-\\x1F\\x7F]/g, \" \")\n                    .replace(/\\s+/g, \" \")\n                    .trim()\n                    .slice(0, MAX_ERROR_CHARS);\n                };\n\n                let formatErr = fallbackFormatErr;\n                try {\n                  const mod = await import(\"/web/src/text.ts\");\n                  const formatOneLineUtf8 = mod?.formatOneLineUtf8;\n                  if (typeof formatOneLineUtf8 === \"function\") {\n                    formatErr = (err) => {\n                      const msg = err instanceof Error ? err.message : err;\n                      return formatOneLineUtf8(String(msg ?? \"\"), 512) || \"Error\";\n                    };\n                  }\n                } catch {\n                  // ignore: keep fallbackFormatErr\n                }\n\n                try {\n                  await import(${JSON.stringify(entrypoint)});\n                  setTimeout(() => self.postMessage({ type: \"__aero_guest_worker_imported\" }), 0);\n                } catch (err) {\n                  setTimeout(() => self.postMessage({ type: \"__aero_guest_worker_import_failed\", message: formatErr(err) }), 0);\n                }\n              })();\n            `,
          ],
          { type: "text/javascript" },
        ),
      );

      const worker = new Worker(wrapperUrl, { type: "module" });
      const imported = new Promise<void>((resolve, reject) => {
        let timer = 0;
        const cleanup = () => {
          if (timer) clearTimeout(timer);
          worker.removeEventListener("message", messageHandler);
          worker.removeEventListener("error", errorHandler);
        };
        const messageHandler = (ev: MessageEvent): void => {
          const data = ev.data as { type?: unknown; message?: unknown } | undefined;
          if (!data) return;
          if (data.type === "__aero_guest_worker_imported") {
            cleanup();
            resolve();
            return;
          }
          if (data.type === "__aero_guest_worker_import_failed") {
            cleanup();
            reject(
              new Error(
                `guest worker wrapper import failed: ${typeof data.message === "string" ? data.message : "unknown error"}`,
              ),
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
          reject(new Error(`guest worker wrapper error during import: ${message} (${filename}:${lineno}:${colno})`));
        };

        worker.addEventListener("message", messageHandler);
        worker.addEventListener("error", errorHandler);
        timer = setTimeout(() => {
          cleanup();
          reject(new Error("Timed out waiting for guest worker import marker"));
        }, 20_000);
      });

      return { worker, wrapperUrl, imported };
    };

    const runGuest = async (payload: any): Promise<any> => {
      const { worker, wrapperUrl, imported } = createGuestWorker();
      try {
        await imported;
        return await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timeout waiting for guest worker")), 30_000);
          worker.onmessage = (ev) => {
            clearTimeout(timeout);
            resolve(ev.data);
          };
          worker.onerror = (err) => {
            clearTimeout(timeout);
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
            reject(new Error(`guest worker error: ${message} (${filename}:${lineno}:${colno})`));
          };
          worker.postMessage(payload);
        });
      } finally {
        worker.terminate();
        URL.revokeObjectURL(wrapperUrl);
      }
    };

    // -------------------------
    // Phase 1: WebHID config descriptor proves guest-visible HID device.
    // -------------------------

    const collections = [
      {
        usagePage: 0x01,
        usage: 0x02,
        collectionType: 0x01,
        children: [],
        inputReports: [
          {
            reportId: 0,
            items: [
              {
                usagePage: 0x01,
                usages: [0x30],
                usageMinimum: 0,
                usageMaximum: 0,
                reportSize: 8,
                reportCount: 1,
                unitExponent: 0,
                unit: 0,
                logicalMinimum: 0,
                logicalMaximum: 255,
                physicalMinimum: 0,
                physicalMaximum: 255,
                strings: [],
                stringMinimum: 0,
                stringMaximum: 0,
                designators: [],
                designatorMinimum: 0,
                designatorMaximum: 0,
                isAbsolute: true,
                isArray: false,
                isBufferedBytes: false,
                isConstant: false,
                isLinear: true,
                isRange: false,
                isRelative: false,
                isVolatile: false,
                hasNull: false,
                hasPreferredState: true,
                isWrapped: false,
              },
            ],
          },
        ],
        outputReports: [
          {
            reportId: 0,
            items: [
              {
                usagePage: 0x01,
                usages: [0x31],
                usageMinimum: 0,
                usageMaximum: 0,
                reportSize: 8,
                reportCount: 1,
                unitExponent: 0,
                unit: 0,
                logicalMinimum: 0,
                logicalMaximum: 255,
                physicalMinimum: 0,
                physicalMaximum: 255,
                strings: [],
                stringMinimum: 0,
                stringMaximum: 0,
                designators: [],
                designatorMinimum: 0,
                designatorMaximum: 0,
                isAbsolute: true,
                isArray: false,
                isBufferedBytes: false,
                isConstant: false,
                isLinear: true,
                isRange: false,
                isRelative: false,
                isVolatile: false,
                hasNull: false,
                hasPreferredState: true,
                isWrapped: false,
              },
            ],
          },
        ],
        featureReports: [],
      },
    ];

    // Ensure the UHCI runtime's external hub (root port 0) has enough downstream ports to host both
    // the built-in synthetic HID devices (ports 1..4) and dynamic passthrough devices (port 5+).
    //
    // Without this, the hub can default to the synthetic-only range and stall on hub-class requests
    // targeting the dynamic port.
    ioWorker.postMessage({ type: "hid:attachHub", guestPath: [0], portCount: externalHubPortCount });

    ioWorker.postMessage({
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Playwright HID",
      guestPort: 0,
      collections,
      hasInterruptOut: true,
    });

    await waitFor(() => hidAttachResult !== null && hidAttachResult.deviceId === 1, 10_000, "hid.attachResult deviceId=1");
    if (!hidAttachResult.ok) {
      throw new Error(`hid.attach failed: ${hidAttachResult.error ?? "unknown error"}`);
    }

    const hidResult = await runGuest({
      mode: "hidConfig",
      ioIpc: segments.ioIpc,
      guestSab,
      guestBase: views.guestLayout.guest_base,
      guestSize: views.guestLayout.guest_size,
      setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0200, wIndex: 0, wLength: 34 },
      inLen: 34,
    });

    // -------------------------
    // Phase 1b: Interrupt-IN polling should NAK until an input report is injected.
    // -------------------------

    const expectedHidInputReport = [0x7f];
    let hidNakObserved = false;
    let hidInputSent = false;

    const { worker: hidWorker, wrapperUrl: hidWrapperUrl, imported: hidImported } = createGuestWorker();
    let hidInterruptResult: any;
    try {
      await hidImported;
      const hidInterruptResultPromise = new Promise<any>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for hid interrupt guest worker")), 45_000);
        hidWorker.onmessage = (ev) => {
          const data = ev.data as any;
          if (data?.type === "hid.interruptNakObserved") {
            hidNakObserved = true;
            if (!hidInputSent) {
              hidInputSent = true;
              const payload = new Uint8Array(expectedHidInputReport);
              ioWorker.postMessage({ type: "hid.inputReport", deviceId: 1, reportId: 0, data: payload }, [payload.buffer]);
            }
            return;
          }
          clearTimeout(timeout);
          resolve(data);
        };
        hidWorker.onerror = (err) => {
          clearTimeout(timeout);
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
          reject(new Error(`hid interrupt guest worker error: ${message} (${filename}:${lineno}:${colno})`));
        };
      });

      hidWorker.postMessage({
        mode: "hidInterruptIn",
        ioIpc: segments.ioIpc,
        guestSab,
        guestBase: views.guestLayout.guest_base,
        guestSize: views.guestLayout.guest_size,
        setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0200, wIndex: 0, wLength: 34 },
        inLen: 34,
        forcedPortIndex: 0,
      });

      hidInterruptResult = await hidInterruptResultPromise;
    } finally {
      hidWorker.terminate();
      URL.revokeObjectURL(hidWrapperUrl);
    }
    if (!hidInputSent) {
      throw new Error(
        `hid interrupt test did not observe NAK (hidNakObserved=${hidNakObserved}) result=${JSON.stringify(hidInterruptResult)}`,
      );
    }

    // -------------------------
    // Phase 1c: Interrupt-OUT should enqueue an output report and the IO worker should forward it.
    // -------------------------

    const expectedHidOutputReport = [0x33];
    hidSendReport = null;

    const { worker: hidOutWorker, wrapperUrl: hidOutWrapperUrl, imported: hidOutImported } = createGuestWorker();
    let hidOutResult: any;
    try {
      await hidOutImported;
      const hidOutResultPromise = new Promise<any>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for hid OUT guest worker")), 45_000);
        hidOutWorker.onmessage = (ev) => {
          clearTimeout(timeout);
          resolve(ev.data);
        };
        hidOutWorker.onerror = (err) => {
          clearTimeout(timeout);
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
          reject(new Error(`hid OUT guest worker error: ${message} (${filename}:${lineno}:${colno})`));
        };
      });

      // Wait for the IO worker to forward `hid.sendReport` (guest->host output report).
      const waitForSendReport = async () => {
        const deadline = performance.now() + 10_000;
        while (!hidSendReport && performance.now() < deadline) {
          await sleep(5);
        }
        if (!hidSendReport) throw new Error("timeout waiting for hid.sendReport");
      };

      hidOutWorker.postMessage({
        mode: "hidInterruptOut",
        ioIpc: segments.ioIpc,
        guestSab,
        guestBase: views.guestLayout.guest_base,
        guestSize: views.guestLayout.guest_size,
        setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0200, wIndex: 0, wLength: 34 },
        inLen: 34,
        forcedPortIndex: 0,
        outData: expectedHidOutputReport,
      });

      // Race the guest-side completion with the host-side forwarding.
      hidOutResult = await hidOutResultPromise;
      await waitForSendReport();
    } finally {
      hidOutWorker.terminate();
      URL.revokeObjectURL(hidOutWrapperUrl);
    }

    ioWorker.postMessage({ type: "hid.detach", deviceId: 1 });

    // -------------------------
    // Phase 2: WebUSB device descriptor proves TD-level NAK while awaiting host completion.
    // -------------------------

    usbActions.length = 0;
    ioWorker.postMessage({
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1d6b, productId: 0x0104, productName: "Playwright WebUSB" },
    });

    try {
      await waitFor(() => guestUsbStatus !== null, 5000, "usb.guest.status initial");
      await waitFor(() => (guestUsbStatus as any)?.attached === true, 5000, "usb.guest.status attached");
    } catch (err) {
      throw new Error(
        `WebUSB guest did not attach: ${String(err)} status=${guestUsbStatus ? JSON.stringify(guestUsbStatus) : "null"}`,
      );
    }

    const expectedDeviceDescriptor = [
      0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01, 0x01, 0x02, 0x03, 0x01,
    ];

    let nakObserved = false;
    let completionSent = false;

    const { worker: webusbWorker, wrapperUrl: webusbWrapperUrl, imported: webusbImported } = createGuestWorker();
    let webusbResult: any;
    try {
      await webusbImported;
      const webusbResultPromise = new Promise<any>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for webusb guest worker")), 45_000);
        webusbWorker.onmessage = (ev) => {
          const data = ev.data as any;
          if (data?.type === "webusb.nakObserved") {
            nakObserved = true;
            return;
          }
          clearTimeout(timeout);
          resolve(data);
        };
        webusbWorker.onerror = (err) => {
          clearTimeout(timeout);
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
          reject(new Error(`webusb guest worker error: ${message} (${filename}:${lineno}:${colno})`));
        };
      });

    const maybeSendCompletion = () => {
      if (completionSent) return;
      if (!nakObserved) return;
      const action = usbActions[0] as any;
      if (!action || typeof action !== "object") return;
      if (action.kind !== "controlIn" || typeof action.id !== "number") return;
      completionSent = true;

      const data = new Uint8Array(expectedDeviceDescriptor);
      ioWorker.postMessage(
        { type: "usb.completion", completion: { kind: "controlIn", id: action.id, status: "success", data } },
        [data.buffer],
      );
    };

    webusbWorker.postMessage({
      mode: "webusbDevice",
      ioIpc: segments.ioIpc,
      guestSab,
      guestBase: views.guestLayout.guest_base,
      guestSize: views.guestLayout.guest_size,
      setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0100, wIndex: 0, wLength: 18 },
      inLen: 18,
      forcedPortIndex: 1,
    });

    // Poll for usb.action + nakObserved before sending the completion to prove NAK-while-pending.
    const completionDeadline = performance.now() + 10_000;
    while (!completionSent && performance.now() < completionDeadline) {
      maybeSendCompletion();
      await sleep(5);
    }
    if (!completionSent) {
      throw new Error(`did not observe NAK+usb.action (nakObserved=${nakObserved} actions=${usbActions.length})`);
    }

      webusbResult = await webusbResultPromise;
    } finally {
      webusbWorker.terminate();
      URL.revokeObjectURL(webusbWrapperUrl);
    }
    ioWorker.postMessage({ type: "usb.selected", ok: false, error: "test complete" });

    URL.revokeObjectURL(guestUrl);
    ioWorker.terminate();
    URL.revokeObjectURL(ioWorkerWrapperUrl);

    return {
      hidResult,
      hidInterruptResult,
      expectedHidInputReport,
      hidOutResult,
      expectedHidOutputReport,
      hidSendReport,
      webusbResult,
      usbActions: usbActions.slice(),
      expectedDeviceDescriptor,
    };
  }, { dynamicHubPort: UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT, externalHubPortCount: DEFAULT_EXTERNAL_HUB_PORT_COUNT });

  if (result.hidResult?.type === "error") {
    throw new Error(String(result.hidResult.message));
  }
  if (result.webusbResult?.type === "error") {
    throw new Error(String(result.webusbResult.message));
  }
  if (result.hidInterruptResult?.type === "error") {
    throw new Error(String(result.hidInterruptResult.message));
  }
  if (result.hidOutResult?.type === "error") {
    throw new Error(String(result.hidOutResult.message));
  }

  const hidData = (result.hidResult as { data: number[] }).data;
  expect(hidData.length).toBe(34);
  // Interface descriptor begins at offset 9; bInterfaceClass is at offset 9+5.
  expect(hidData[14]).toBe(0x03);

  // HID interrupt-IN should NAK until we inject an input report, then return the expected bytes.
  expect((result.hidInterruptResult as any).nakObserved).toBe(true);
  expect((result.hidInterruptResult as any).data).toEqual(result.expectedHidInputReport);

  // HID interrupt-OUT should be forwarded to the host via `hid.sendReport`.
  expect((result.hidSendReport as any)?.deviceId).toBe(1);
  expect((result.hidSendReport as any)?.reportType).toBe("output");
  expect((result.hidSendReport as any)?.reportId).toBe(0);
  expect(Array.from((result.hidSendReport as any)?.data ?? [])).toEqual(result.expectedHidOutputReport);

  // WebUSB: should emit exactly one controlIn action and the guest TD should NAK until completion.
  expect(result.usbActions).toHaveLength(1);
  expect((result.usbActions[0] as any).kind).toBe("controlIn");
  expect((result.webusbResult as any).nakObserved).toBe(true);
  expect((result.webusbResult as any).data).toEqual(result.expectedDeviceDescriptor);
});

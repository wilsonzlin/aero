import "./style.css";

import { installAeroGlobals } from "./aero";
import { startFrameScheduler, type FrameSchedulerHandle } from "./main/frameScheduler";
import { GpuRuntime } from "./gpu/gpuRuntime";
import { fnv1a32Hex } from "./utils/fnv1a";
import { perf } from "./perf/perf";
import { createAdaptiveRingBufferTarget, createAudioOutput, startAudioPerfSampling } from "./platform/audio";
import { MicCapture, micRingBufferReadInto, type MicRingBuffer } from "./audio/mic_capture";
import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
} from "./audio/mic_ring.js";
import { startSyntheticMic } from "./audio/synthetic_mic";
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from "./platform/features";
import { importFileToOpfs, openFileHandle, removeOpfsEntry } from "./platform/opfs.ts";
import { ensurePersistentStorage, getPersistentStorageInfo, getStorageEstimate } from "./platform/storage_quota";
import { mountWebHidPassthroughPanel, WebHidPassthroughManager } from "./platform/webhid_passthrough";
import { initAeroStatusApi } from "./api/status";
import { AeroConfigManager } from "./config/manager";
import { InputCapture } from "./input/input_capture";
import { InputEventType, type InputBatchTarget } from "./input/event_queue";
import { decodeGamepadReport, formatGamepadHat } from "./input/gamepad";
import { installPerfHud } from "./perf/hud_entry";
import {
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  addHeaderI32,
  initFramebufferHeader,
  requiredFramebufferBytes,
  storeHeaderI32,
  wrapSharedFramebuffer,
} from "./display/framebuffer_protocol";
import { VgaPresenter } from "./display/vga_presenter";
import { installAeroGlobal } from "./runtime/aero_global";
import { createWebGpuCanvasContext, requestWebGpuDevice } from "./platform/webgpu";
import { WorkerCoordinator } from "./runtime/coordinator";
import { installNetTraceBackendOnAeroGlobal } from "./net/trace_backend";
import { initWasm, type WasmApi, type WasmVariant } from "./runtime/wasm_loader";
import { precompileWasm } from "./runtime/wasm_preload";
import { IO_IPC_HID_IN_QUEUE_KIND, type WorkerRole } from "./runtime/shared_layout";
import { DiskManager } from "./storage/disk_manager";
import type { DiskImageMetadata, MountConfig } from "./storage/metadata";
import { OPFS_DISKS_PATH, OPFS_LEGACY_IMAGES_DIR } from "./storage/metadata";
import { RuntimeDiskClient, type OpenResult } from "./storage/runtime_disk_client";
import { type JitWorkerResponse } from "./workers/jit_protocol";
import { JitWorkerClient } from "./workers/jit_worker_client";
import { DemoVmWorkerClient } from "./workers/demo_vm_worker_client";
import { openRingByKind } from "./ipc/ipc";
import { FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "./ipc/gpu-protocol";
import { SHARED_FRAMEBUFFER_HEADER_U32_LEN, SharedFramebufferHeaderIndex } from "./ipc/shared-layout";
import { mountSettingsPanel } from "./ui/settings_panel";
import { mountStatusPanel } from "./ui/status_panel";
import { installNetTraceUI } from "./net/trace_ui";
import { renderWebUsbPanel } from "./usb/webusb_panel";
import { renderWebUsbUhciHarnessPanel } from "./usb/webusb_uhci_harness_panel";
import { isUsbUhciHarnessStatusMessage, type WebUsbUhciHarnessRuntimeSnapshot } from "./usb/webusb_harness_runtime";
import { UsbBroker } from "./usb/usb_broker";
import { renderWebUsbBrokerPanel as renderWebUsbBrokerPanelUi } from "./usb/usb_broker_panel";
import { formatHexBytes, hex16, hex8 } from "./usb/usb_hex";
import {
  isUsbPassthroughDemoResultMessage,
  type UsbPassthroughDemoResult,
  type UsbPassthroughDemoRunMessage,
} from "./usb/usb_passthrough_demo_runtime";
import { isUsbSelectedMessage, type UsbHostAction, type UsbHostCompletion } from "./usb/usb_proxy_protocol";

const configManager = new AeroConfigManager({ staticConfigUrl: "/aero.config.json" });
const configInitPromise = configManager.init();

initAeroStatusApi("booting");
installPerfHud({ guestRamBytes: configManager.getState().effective.guestMemoryMiB * 1024 * 1024 });
installAeroGlobal();
perf.installGlobalApi();

if (new URLSearchParams(location.search).has("trace")) perf.traceStart();
perf.instant("boot:main:start", "p");

installAeroGlobals();

const workerCoordinator = new WorkerCoordinator();
installNetTraceBackendOnAeroGlobal(workerCoordinator);
const usbBroker = new UsbBroker();
const diskManagerPromise = DiskManager.create();

const wiredWebHidWorkers = new WeakSet<Worker>();

function wireIoWorkerForWebHid(ioWorker: Worker, manager: WebHidPassthroughManager): void {
  if (wiredWebHidWorkers.has(ioWorker)) return;
  wiredWebHidWorkers.add(ioWorker);

  ioWorker.addEventListener("message", (ev: MessageEvent<unknown>) => {
    manager.handleWorkerMessage(ev.data);
  });
}

const webHidManager = new WebHidPassthroughManager({
  target: {
    postMessage: (message, transfer) => {
      const ioWorker = workerCoordinator.getIoWorker();
      if (!ioWorker) {
        throw new Error("I/O worker is not running. Start workers before attaching WebHID devices.");
      }

      wireIoWorkerForWebHid(ioWorker, webHidManager);

      if (transfer && transfer.length) {
        ioWorker.postMessage(message, transfer);
      } else {
        ioWorker.postMessage(message);
      }
    },
  },
});

function syncWebHidInputReportRing(ioWorker: Worker | null): void {
  if (!ioWorker) {
    webHidManager.setInputReportRing(null);
    return;
  }

  const sab = workerCoordinator.getIoIpcSab();
  const status = workerCoordinator.getStatusView();
  if (!sab || !status) {
    webHidManager.setInputReportRing(null);
    return;
  }

  try {
    const ring = openRingByKind(sab, IO_IPC_HID_IN_QUEUE_KIND);
    webHidManager.setInputReportRing(ring, status);
  } catch {
    webHidManager.setInputReportRing(null);
  }
}

configManager.subscribe((state) => {
  workerCoordinator.updateConfig(state.effective);
});
const wasmInitPromise = perf.spanAsync("wasm:init", async () => {
  const preferThreaded = (() => {
    if (!(globalThis as any).crossOriginIsolated) return false;
    if (typeof SharedArrayBuffer === "undefined") return false;
    if (typeof Atomics === "undefined") return false;
    if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory !== "function") return false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      return true;
    } catch {
      return false;
    }
  })();

  const preferredVariant: WasmVariant = preferThreaded ? "threaded" : "single";
  try {
    const { module } = await precompileWasm(preferredVariant);
    return await initWasm({ variant: preferredVariant, module });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.warn(`[wasm] Precompile (${preferredVariant}) failed; falling back to default init. Error: ${message}`);
    return await initWasm();
  }
});
let frameScheduler: FrameSchedulerHandle | null = null;

type OpenedDisk = { meta: DiskImageMetadata; open: OpenResult };
type OpenedBootDisks = { client: RuntimeDiskClient; mounts: MountConfig; hdd?: OpenedDisk; cd?: OpenedDisk };
type BootDiskSelection = { mounts: MountConfig; hdd?: DiskImageMetadata; cd?: DiskImageMetadata };

let bootDiskSelection: BootDiskSelection | null = null;

async function getBootDiskSelection(manager: DiskManager): Promise<BootDiskSelection> {
  const [disks, mounts] = await Promise.all([manager.listDisks(), manager.getMounts()]);
  const byId = new Map(disks.map((d) => [d.id, d]));
  return {
    mounts,
    hdd: mounts.hddId ? byId.get(mounts.hddId) : undefined,
    cd: mounts.cdId ? byId.get(mounts.cdId) : undefined,
  };
}

async function openBootDisks(manager: DiskManager): Promise<OpenedBootDisks> {
  const [disks, mounts] = await Promise.all([manager.listDisks(), manager.getMounts()]);
  const byId = new Map(disks.map((d) => [d.id, d]));
  const client = new RuntimeDiskClient();
  const opened: OpenedBootDisks = { client, mounts };

  try {
    if (mounts.hddId) {
      const meta = byId.get(mounts.hddId);
      if (!meta) throw new Error(`Mounted HDD disk not found: ${mounts.hddId}`);
      opened.hdd = { meta, open: await client.open(meta, { mode: "cow" }) };
    }
    if (mounts.cdId) {
      const meta = byId.get(mounts.cdId);
      if (!meta) throw new Error(`Mounted CD disk not found: ${mounts.cdId}`);
      opened.cd = { meta, open: await client.open(meta, { mode: "direct" }) };
    }
    return opened;
  } catch (err) {
    client.close();
    throw err;
  }
}

// Updated by the microphone UI and read by the worker coordinator so that
// newly-started workers inherit the current mic attachment (if any).
// `sampleRate` is the actual capture sample rate (AudioContext.sampleRate).
let micAttachment: { ringBuffer: SharedArrayBuffer; sampleRate: number } | null = null;

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<Node | string | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) continue;
    if (key === "class") {
      node.className = String(value);
    } else if (key === "text") {
      node.textContent = String(value);
    } else if (key.startsWith("on") && typeof value === "function") {
      (node as unknown as Record<string, unknown>)[key.toLowerCase()] = value;
    } else {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    node.append(child instanceof Node ? child : document.createTextNode(child));
  }
  return node;
}

function renderBuildInfoPanel(): HTMLElement {
  // `__AERO_BUILD_INFO__` is normally injected by `web/vite.config.ts`. When this
  // UI is served under the repo-root Vite harness (`npm run dev:harness`) that
  // define is not present, so fall back to a small placeholder instead of
  // crashing the entire page.
  const buildInfo =
    // eslint-disable-next-line no-undef
    typeof __AERO_BUILD_INFO__ !== "undefined"
      ? // eslint-disable-next-line no-undef
        __AERO_BUILD_INFO__
      : { version: "dev", gitSha: "unknown", builtAt: "unknown" };

  const versionLink = el("a", {
    href: "/aero.version.json",
    target: "_blank",
    rel: "noreferrer",
    text: "aero.version.json",
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Build info" }),
    el("div", { class: "row" }, el("strong", { text: "Version:" }), el("span", { class: "mono", text: buildInfo.version })),
    el("div", { class: "row" }, el("strong", { text: "Commit:" }), el("span", { class: "mono", text: buildInfo.gitSha })),
    el(
      "div",
      { class: "row" },
      el("strong", { text: "Built:" }),
      el("span", { class: "mono", text: buildInfo.builtAt }),
    ),
    el("div", { class: "hint muted" }, "Also available at ", versionLink, "."),
  );
}

function createExpectedTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const isLeft = x < halfW;
      const isTop = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      let r = 0;
      let g = 0;
      let b = 0;
      if (isTop && isLeft) {
        r = 255;
      } else if (isTop && !isLeft) {
        g = 255;
      } else if (!isTop && isLeft) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }

      out[i] = r;
      out[i + 1] = g;
      out[i + 2] = b;
      out[i + 3] = 255;
    }
  }

  return out;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return "unknown";
  const abs = Math.abs(bytes);
  if (abs < 1024) return `${bytes.toFixed(0)} B`;
  if (abs < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (abs < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (abs < 1024 * 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  return `${(bytes / (1024 * 1024 * 1024 * 1024)).toFixed(1)} TB`;
}

function formatMaybeBytes(bytes: number | null): string {
  return bytes === null ? "unknown" : formatBytes(bytes);
}

function render(): void {
  const app = document.getElementById("app");
  if (!app) throw new Error("Missing #app element");

  const report = detectPlatformFeatures();
  const missing = explainMissingRequirements(report);

  const settingsHost = el("div", { class: "panel" });
  mountSettingsPanel(settingsHost, configManager);

  const statusHost = el("div", { class: "panel" });
  mountStatusPanel(statusHost, configManager, workerCoordinator);

  app.replaceChildren(
    el("h1", { text: "Aero Platform Capabilities" }),
    renderBuildInfoPanel(),
    settingsHost,
    statusHost,
    el(
      "div",
      { class: `panel ${missing.length ? "missing" : ""}` },
      el("h2", { text: "Required features" }),
      missing.length
        ? el(
            "ul",
            {},
            ...missing.map((m) => el("li", { text: m })),
          )
        : el("div", { text: "All required features appear to be available." }),
    ),
    el(
      "div",
      { class: "panel" },
      el("h2", { text: "Capability report" }),
      renderCapabilityTable(report),
    ),
    renderWasmPanel(),
    renderGraphicsPanel(report),
    renderMachinePanel(),
    renderSnapshotPanel(report),
    renderWebGpuPanel(),
    renderGpuWorkerPanel(),
    renderSm5TrianglePanel(),
    renderOpfsPanel(),
    renderNetTracePanel(),
    renderDisksPanel(),
    renderAudioPanel(),
    renderMicrophonePanel(),
    renderWebUsbDiagnosticsPanel(report),
    renderWebUsbPanel(report),
    renderWebUsbUhciHarnessPanel(report, wasmInitPromise),
    renderWebHidPassthroughPanel(),
    renderInputPanel(),
    renderWebUsbBrokerPanel(),
    renderWebUsbPassthroughDemoWorkerPanel(),
    renderWebUsbUhciHarnessWorkerPanel(),
    renderWorkersPanel(report),
    renderIpcDemoPanel(),
    renderMicrobenchPanel(),
  );
}

function renderWebUsbDiagnosticsPanel(report: PlatformFeatureReport): HTMLElement {
  const link = el("a", {
    href: "/webusb_diagnostics.html",
    target: "_blank",
    rel: "noopener",
    text: "/webusb_diagnostics.html",
  });

  const secure = (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true;
  const hasUsb =
    typeof navigator !== "undefined" && "usb" in navigator && !!(navigator as Navigator & { usb?: unknown }).usb;

  const messages: string[] = [];
  if (!secure) messages.push("Not a secure context (WebUSB requires https:// or localhost).");
  if (!hasUsb) messages.push("navigator.usb is missing (unsupported browser or blocked by policy).");

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "WebUSB diagnostics" }),
    el("div", { class: "hint" }, "Standalone WebUSB diagnostics: ", link, "."),
    report.webusb
      ? el("div", { class: "ok", text: "WebUSB appears to be available in this context." })
      : el(
          "div",
          { class: "bad" },
          el("div", { text: "WebUSB is unavailable in this context." }),
          messages.length
            ? el(
                "ul",
                { class: "hint" },
                ...messages.map((m) => el("li", { text: m })),
              )
            : null,
        ),
  );
}

function renderCapabilityTable(report: PlatformFeatureReport): HTMLTableElement {
  const orderedKeys: Array<keyof PlatformFeatureReport> = [
    "crossOriginIsolated",
    "sharedArrayBuffer",
    "wasmSimd",
    "wasmThreads",
    "jit_dynamic_wasm",
    "webgpu",
    "webusb",
    "webhid",
    "webgl2",
    "opfs",
    "opfsSyncAccessHandle",
    "audioWorklet",
    "offscreenCanvas",
  ];

  const tbody = el("tbody");
  for (const key of orderedKeys) {
    const val = report[key];
    tbody.append(
      el(
        "tr",
        {},
        el("th", { text: key }),
        el("td", { class: val ? "ok" : "bad", text: val ? "supported" : "missing" }),
      ),
    );
  }

  return el(
    "table",
    {},
    el("thead", {}, el("tr", {}, el("th", { text: "feature" }), el("th", { text: "status" }))),
    tbody,
  );
}

function renderWasmPanel(): HTMLElement {
  const status = el("pre", { text: "Loading WASM…" });
  const output = el("pre", { text: "" });
  const error = el("pre", { text: "" });

  wasmInitPromise
    .then(({ api, variant, reason }) => {
      status.textContent = `Loaded variant: ${variant}\nReason: ${reason}`;
      output.textContent = `greet(\"Aero\") → ${api.greet("Aero")}\nadd(2, 3) → ${api.add(2, 3)}`;
      // Expose for quick interactive debugging / Playwright assertions.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroWasmApi = api;
    })
    .catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      status.textContent = "Failed to initialize WASM";
      error.textContent = message;
      console.error(err);
    });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "WASM runtime (threaded/single auto-selection)" }),
    status,
    output,
    error,
  );
}

function renderGraphicsPanel(report: PlatformFeatureReport): HTMLElement {
  const selected =
    report.webgpu ? "WebGPU" : report.webgl2 ? "WebGL2 (fallback)" : "Unavailable (no WebGPU/WebGL2)";

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Graphics backend" }),
    el("div", { class: "row" }, el("strong", { text: `Auto selection: ${selected}` })),
    el(
      "div",
      {},
      "Open the standalone fallback demo: ",
      el("a", { href: "./webgl2_fallback_demo.html" }, "./webgl2_fallback_demo.html"),
      ".",
    ),
  );
}

async function getOpfsFileIfExists(path: string): Promise<File | null> {
  try {
    const handle = await openFileHandle(path, { create: false });
    return await handle.getFile();
  } catch (err) {
    if (err instanceof DOMException && err.name === "NotFoundError") return null;
    throw err;
  }
}

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const buf = new ArrayBuffer(bytes.byteLength);
  const out = new Uint8Array(buf);
  out.set(bytes);
  return out;
}

async function writeBytesToOpfs(path: string, bytes: Uint8Array): Promise<void> {
  const handle = await openFileHandle(path, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  // `FileSystemWritableFileStream.write()` only accepts ArrayBuffer-backed views in the
  // lib.dom typings. Snapshot buffers coming from threaded WASM builds can be backed by
  // SharedArrayBuffer, so clone into an ArrayBuffer-backed Uint8Array before writing.
  const payload = ensureArrayBufferBacked(bytes);

  try {
    await writable.write(payload);
    await writable.close();
  } catch (err) {
    try {
      await writable.abort();
    } catch {
      // ignore
    }
    throw err;
  }
}

function downloadFile(file: Blob, filename: string): void {
  const url = URL.createObjectURL(file);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  // Safari can cancel the download if the object URL is revoked synchronously.
  const timer = window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  (timer as unknown as { unref?: () => void }).unref?.();
}

function renderMachinePanel(): HTMLElement {
  const status = el("pre", { text: "Initializing canonical machine…" });
  const vgaInfo = el("pre", { text: "" });
  const inputHint = el("div", {
    class: "mono",
    text: "Tip: click the canvas to focus + request pointer lock (keyboard/mouse will be forwarded to the guest).",
  });
  const canvas = el("canvas", { id: "canonical-machine-vga-canvas" }) as HTMLCanvasElement;
  canvas.tabIndex = 0;
  canvas.style.width = "640px";
  canvas.style.height = "400px";
  canvas.style.border = "1px solid rgba(127, 127, 127, 0.5)";
  canvas.style.background = "#000";
  canvas.style.imageRendering = "pixelated";

  const output = el("pre", { text: "" });
  const error = el("pre", { text: "" });

  const decoder = new TextDecoder();
  const encoder = new TextEncoder();

  // Expose machine panel state for Playwright smoke tests.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const testState = ((globalThis as any).__aeroMachinePanelTest = {
    ready: false,
    vgaSupported: false,
    framesPresented: 0,
    sharedFramesPublished: 0,
    transport: "none" as "none" | "ptr" | "copy",
    width: 0,
    height: 0,
    strideBytes: 0,
    error: null as string | null,
  });

  function setError(msg: string): void {
    error.textContent = msg;
    testState.error = msg;
    console.error(msg);
  }

  // Avoid pathological allocations if the guest (or a buggy WASM build) reports
  // an absurd scanout mode. Keep the UI responsive rather than attempting to
  // allocate multi-gigabyte buffers.
  const MAX_VGA_FRAME_BYTES = 32 * 1024 * 1024; // ~4K@60-ish upper bound for a demo panel.

  function buildSerialBootSector(message: string): Uint8Array {
    const msgBytes = encoder.encode(message);
    const sector = new Uint8Array(512);
    let off = 0;

    // Emit a tiny VGA text-mode banner ("AERO!") into 0xB8000 so the machine demo
    // has something visible on screen even before real disks/firmware are wired up.
    //
    // cld
    sector[off++] = 0xfc;
    // mov ax, 0xb800
    sector.set([0xb8, 0x00, 0xb8], off);
    off += 3;
    // mov es, ax
    sector.set([0x8e, 0xc0], off);
    off += 2;
    // xor di, di
    sector.set([0x31, 0xff], off);
    off += 2;
    // mov ah, 0x1f  (white-on-blue)
    sector.set([0xb4, 0x1f], off);
    off += 2;
    for (const ch of encoder.encode("AERO!")) {
      // mov al, imm8
      sector.set([0xb0, ch], off);
      off += 2;
      // stosw
      sector[off++] = 0xab;
    }

    // mov dx, 0x3f8
    sector.set([0xba, 0xf8, 0x03], off);
    off += 3;

    for (const b of msgBytes) {
      // mov al, imm8
      sector.set([0xb0, b], off);
      off += 2;
      // out dx, al
      sector[off] = 0xee;
      off += 1;
    }

    // sti (ensure IRQs can wake the CPU)
    sector[off++] = 0xfb;
    // hlt; jmp hlt (wait-for-interrupt loop)
    const hltOff = off;
    sector[off++] = 0xf4;
    const jmpOff = off;
    sector[off++] = 0xeb;
    sector[off++] = (hltOff - (jmpOff + 2)) & 0xff;

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xaa;
    return sector;
  }

  wasmInitPromise
    .then(({ api, variant, wasmMemory }) => {
      const machine = new api.Machine(2 * 1024 * 1024);
      machine.set_disk_image(buildSerialBootSector("Hello from aero-machine\\n"));
      machine.reset();

      status.textContent = `Machine ready (WASM ${variant}). Booting…`;

      const ctx = canvas.getContext("2d", { alpha: false });
      if (!ctx) {
        testState.ready = true;
        setError("Machine demo: Canvas 2D context unavailable.");
        return;
      }
      // TS does not narrow captured variables inside nested functions (even for `const`),
      // so stash the non-null canvas context for the VGA present closure.
      const ctx2 = ctx;

      // Optional input capture: drive the machine's i8042 PS/2 keyboard/mouse devices directly.
      // This uses the same `InputCapture` batching/scancode translation as the I/O worker path.
      let inputCapture: InputCapture | null = null;
      {
        const messageListeners: ((ev: MessageEvent<unknown>) => void)[] = [];
        const inputTarget: InputBatchTarget & {
          addEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
          removeEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
        } = {
          postMessage: (msg, _transfer) => {
            const words = new Int32Array(msg.buffer);
            const count = words[0] >>> 0;
            const base = 2;
            for (let i = 0; i < count; i += 1) {
              const off = base + i * 4;
              const type = words[off] >>> 0;
              if (type === InputEventType.KeyScancode) {
                const packed = words[off + 2] >>> 0;
                const len = Math.min(words[off + 3] >>> 0, 4);
                if (len === 0) continue;
                if (typeof machine.inject_key_scancode_bytes === "function") {
                  machine.inject_key_scancode_bytes(packed, len);
                } else if (typeof machine.inject_keyboard_bytes === "function") {
                  const bytes = new Uint8Array(len);
                  for (let j = 0; j < len; j++) bytes[j] = (packed >>> (j * 8)) & 0xff;
                  machine.inject_keyboard_bytes(bytes);
                }
              } else if (type === InputEventType.MouseMove) {
                const dx = words[off + 2] | 0;
                const dyPs2 = words[off + 3] | 0;
                if (typeof machine.inject_ps2_mouse_motion === "function") {
                  machine.inject_ps2_mouse_motion(dx, dyPs2, 0);
                } else if (typeof machine.inject_mouse_motion === "function") {
                  // Machine expects browser-style coordinates (+Y down).
                  machine.inject_mouse_motion(dx, -dyPs2, 0);
                }
              } else if (type === InputEventType.MouseWheel) {
                const dz = words[off + 2] | 0;
                if (typeof machine.inject_ps2_mouse_motion === "function") {
                  machine.inject_ps2_mouse_motion(0, 0, dz);
                } else if (typeof machine.inject_mouse_motion === "function") {
                  machine.inject_mouse_motion(0, 0, dz);
                }
              } else if (type === InputEventType.MouseButtons) {
                // PS/2 supports the core 3 buttons.
                const buttons = words[off + 2] & 0xff;
                const mask = buttons & 0x07;
                if (typeof machine.inject_mouse_buttons_mask === "function") {
                  machine.inject_mouse_buttons_mask(mask);
                } else if (typeof machine.inject_ps2_mouse_buttons === "function") {
                  machine.inject_ps2_mouse_buttons(mask);
                }
              }
            }

            if (msg.recycle) {
              const ev = new MessageEvent("message", {
                data: { type: "in:input-batch-recycle", buffer: msg.buffer },
              });
              for (const listener of messageListeners.slice()) listener(ev);
            }
          },
          addEventListener: (type, listener) => {
            if (type !== "message") return;
            messageListeners.push(listener);
          },
          removeEventListener: (type, listener) => {
            if (type !== "message") return;
            const idx = messageListeners.indexOf(listener);
            if (idx >= 0) messageListeners.splice(idx, 1);
          },
        };

        const capture = new InputCapture(canvas, inputTarget, {
          enableGamepad: false,
          recycleBuffers: true,
        });
        capture.start();
        inputCapture = capture;
      }

      const stopInputCapture = (): void => {
        if (!inputCapture) return;
        try {
          inputCapture.stop();
        } catch {
          // ignore
        }
        inputCapture = null;
      };

      const hasVgaPresent = typeof machine.vga_present === "function";
      const hasVgaSize = typeof machine.vga_width === "function" && typeof machine.vga_height === "function";
      const hasVgaPtr =
        !!wasmMemory &&
        typeof machine.vga_framebuffer_ptr === "function" &&
        typeof machine.vga_framebuffer_len_bytes === "function";
      const hasVgaCopy =
        typeof machine.vga_framebuffer_copy_rgba8888 === "function" ||
        typeof machine.vga_framebuffer_rgba8888_copy === "function";
      const hasVga = hasVgaPresent && hasVgaSize && (hasVgaPtr || hasVgaCopy);

      testState.vgaSupported = hasVga;
      vgaInfo.textContent = hasVga ? "vga: ready" : "vga: unavailable (WASM build missing scanout exports)";

      let imageData: ImageData | null = null;
      let imageDataBytes: Uint8ClampedArray<ArrayBuffer> | null = null;
      let dstWidth = 0;
      let dstHeight = 0;
      let vgaFailed = false;

      let sharedVgaSab: SharedArrayBuffer | null = null;
      let sharedVga: ReturnType<typeof wrapSharedFramebuffer> | null = null;
      let sharedVgaWidth = 0;
      let sharedVgaHeight = 0;
      let sharedVgaStrideBytes = 0;

      function ensureSharedVga(width: number, height: number, strideBytes: number): ReturnType<typeof wrapSharedFramebuffer> | null {
        if (typeof SharedArrayBuffer === "undefined") return null;

        let requiredBytes: number;
        try {
          requiredBytes = requiredFramebufferBytes(width, height, strideBytes);
        } catch {
          return null;
        }
        if (!Number.isFinite(requiredBytes) || requiredBytes <= 0 || requiredBytes > MAX_VGA_FRAME_BYTES + 4096) {
          return null;
        }

        if (!sharedVgaSab || sharedVgaSab.byteLength < requiredBytes) {
          try {
            sharedVgaSab = new SharedArrayBuffer(requiredBytes);
          } catch {
            sharedVgaSab = null;
            sharedVga = null;
            return null;
          }

          sharedVga = wrapSharedFramebuffer(sharedVgaSab, 0);
          initFramebufferHeader(sharedVga.header, { width, height, strideBytes });
          sharedVgaWidth = width;
          sharedVgaHeight = height;
          sharedVgaStrideBytes = strideBytes;

          // Expose the SAB for harnesses / debugging (optional).
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (globalThis as any).__aeroMachineVgaFramebuffer = sharedVgaSab;
          return sharedVga;
        }

        if (!sharedVga) {
          sharedVga = wrapSharedFramebuffer(sharedVgaSab, 0);
          initFramebufferHeader(sharedVga.header, { width, height, strideBytes });
          sharedVgaWidth = width;
          sharedVgaHeight = height;
          sharedVgaStrideBytes = strideBytes;
          return sharedVga;
        }

        if (sharedVgaWidth !== width || sharedVgaHeight !== height || sharedVgaStrideBytes !== strideBytes) {
          storeHeaderI32(sharedVga.header, HEADER_INDEX_WIDTH, width);
          storeHeaderI32(sharedVga.header, HEADER_INDEX_HEIGHT, height);
          storeHeaderI32(sharedVga.header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
          addHeaderI32(sharedVga.header, HEADER_INDEX_CONFIG_COUNTER, 1);
          sharedVgaWidth = width;
          sharedVgaHeight = height;
          sharedVgaStrideBytes = strideBytes;
        }

        return sharedVga;
      }

      function presentVgaFrame(): void {
        if (!hasVga || vgaFailed) return;

        try {
          // Trigger WASM-side VGA/VBE scanout. This populates an RGBA framebuffer in
          // WASM linear memory which we then blit onto the HTML canvas.
          machine.vga_present?.();

          const width = machine.vga_width?.() ?? 0;
          const height = machine.vga_height?.() ?? 0;
          // Some older WASM builds may not expose a stride helper; assume tightly packed RGBA8888.
          let strideBytes = machine.vga_stride_bytes?.() ?? width * 4;

          if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0) return;
          if (!Number.isInteger(strideBytes) || strideBytes < width * 4) return;

          const requiredDstBytes = width * height * 4;
          let requiredSrcBytes = strideBytes * height;
          if (
            !Number.isFinite(requiredDstBytes) ||
            requiredDstBytes <= 0 ||
            requiredDstBytes > MAX_VGA_FRAME_BYTES ||
            !Number.isFinite(requiredSrcBytes) ||
            requiredSrcBytes <= 0 ||
            requiredSrcBytes > MAX_VGA_FRAME_BYTES
          ) {
            return;
          }
          let src: Uint8Array | null = null;
          if (hasVgaPtr && wasmMemory) {
            const ptr = machine.vga_framebuffer_ptr?.() ?? 0;
            const lenBytes = machine.vga_framebuffer_len_bytes?.() ?? 0;
            if (!Number.isSafeInteger(ptr) || !Number.isSafeInteger(lenBytes) || ptr < 0 || lenBytes < 0) return;
            if (lenBytes < requiredSrcBytes) return;
            const buf = wasmMemory.buffer;
            if (ptr > buf.byteLength || lenBytes > buf.byteLength - ptr) return;
            src = new Uint8Array(buf, ptr, requiredSrcBytes);
            testState.transport = "ptr";
          } else if (hasVgaCopy) {
            // Fall back to a JS-owned copy if we cannot access WASM linear memory.
            const copied =
              machine.vga_framebuffer_copy_rgba8888?.() ?? machine.vga_framebuffer_rgba8888_copy?.() ?? null;
            if (!copied || !copied.byteLength) return;
            src = copied;
            testState.transport = "copy";
            if (src.byteLength < requiredSrcBytes && src.byteLength === width * height * 4) {
              // Some helpers return tight-packed buffers even if a stride is reported.
              strideBytes = width * 4;
              requiredSrcBytes = strideBytes * height;
            }
            if (src.byteLength < requiredSrcBytes) return;
          } else {
            return;
          }

          if (canvas.width !== width || canvas.height !== height) {
            canvas.width = width;
            canvas.height = height;
          }

          if (
            !imageDataBytes ||
            dstWidth !== width ||
            dstHeight !== height ||
            imageDataBytes.byteLength !== requiredDstBytes
          ) {
            dstWidth = width;
            dstHeight = height;
            imageDataBytes = new Uint8ClampedArray(requiredDstBytes);
            imageData = new ImageData(imageDataBytes, width, height);
          }
          if (!imageData || !imageDataBytes) return;

          // Optional: also publish the scanout into a SharedArrayBuffer-backed framebuffer so
          // existing shared-framebuffer plumbing can consume it (e.g. GPU-worker harnesses).
          const shared = ensureSharedVga(width, height, strideBytes);
          if (shared) {
            shared.pixelsU8.set(src.subarray(0, requiredSrcBytes));
            addHeaderI32(shared.header, HEADER_INDEX_FRAME_COUNTER, 1);
            testState.sharedFramesPublished += 1;
          }

          if (strideBytes === width * 4) {
            imageDataBytes.set(src.subarray(0, requiredDstBytes));
          } else {
            for (let y = 0; y < height; y++) {
              const srcOff = y * strideBytes;
              const dstOff = y * width * 4;
              imageDataBytes.set(src.subarray(srcOff, srcOff + width * 4), dstOff);
            }
          }

          ctx2.putImageData(imageData, 0, 0);
          testState.framesPresented += 1;
          testState.width = width;
          testState.height = height;
          testState.strideBytes = strideBytes;
          const pointerLock = typeof document !== "undefined" && document.pointerLockElement === canvas ? "yes" : "no";
          vgaInfo.textContent =
            `vga: ${width}x${height} stride=${strideBytes} ` +
            `frames=${testState.framesPresented} transport=${testState.transport}` +
            (testState.sharedFramesPublished ? ` shared=${testState.sharedFramesPublished}` : "") +
            ` pointerLock=${pointerLock}`;
        } catch (err) {
          vgaFailed = true;
          const message = err instanceof Error ? err.message : String(err);
          vgaInfo.textContent = `vga: error (${message})`;
          setError(`Machine demo VGA present failed: ${message}`);
        }
      }

      const timer = window.setInterval(() => {
        const exit = machine.run_slice(50_000);
        const exitKind = exit.kind;
        const exitExecuted = exit.executed;
        const exitDetail = exit.detail;

        const bytes = machine.serial_output();
        if (bytes.byteLength) {
          output.textContent = `${output.textContent ?? ""}${decoder.decode(bytes)}`;
        }

        presentVgaFrame();

        status.textContent = `run_slice: kind=${exitKind} executed=${exitExecuted} detail=${exitDetail}`;
        exit.free();

        // `RunExitKind::Completed` is 0 and `RunExitKind::Halted` is 1.
        // Keep ticking while halted so injected interrupts (keyboard/mouse) can wake the CPU.
        if (exitKind !== 0 && exitKind !== 1) {
          window.clearInterval(timer);
          stopInputCapture();
          try {
            (machine as unknown as { free?: () => void }).free?.();
          } catch {
            // ignore
          }
        }
      }, 50);
      (timer as unknown as { unref?: () => void }).unref?.();
      testState.ready = true;
    })
    .catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      status.textContent = "Machine unavailable (WASM init failed)";
      setError(message);
      testState.ready = true;
    });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Machine (canonical VM) – serial + VGA demo" }),
    status,
    inputHint,
    el("div", { class: "row" }, canvas),
    vgaInfo,
    output,
    error,
  );
}

function renderSnapshotPanel(report: PlatformFeatureReport): HTMLElement {
  const status = el("pre", { id: "demo-vm-snapshot-status", text: "Initializing demo VM…" });
  const output = el("pre", { id: "demo-vm-snapshot-output", text: "" });
  const error = el("pre", { id: "demo-vm-snapshot-error", text: "" });

  const autosaveInput = el("input", { type: "number", min: "0", step: "1", value: "0" }) as HTMLInputElement;
  const importInput = el("input", {
    id: "demo-vm-snapshot-import",
    type: "file",
    accept: ".snap,application/octet-stream",
  }) as HTMLInputElement;

  const saveButton = el("button", { id: "demo-vm-snapshot-save", text: "Save", disabled: "true" }) as HTMLButtonElement;
  const loadButton = el("button", { id: "demo-vm-snapshot-load", text: "Load", disabled: "true" }) as HTMLButtonElement;
  const exportButton = el("button", {
    id: "demo-vm-snapshot-export",
    text: "Export",
    disabled: "true",
  }) as HTMLButtonElement;
  const deleteButton = el("button", {
    id: "demo-vm-snapshot-delete",
    text: "Delete",
    disabled: "true",
  }) as HTMLButtonElement;
  const advanceButton = el("button", {
    id: "demo-vm-snapshot-advance",
    text: "Advance",
    disabled: "true",
  }) as HTMLButtonElement;

  const SNAPSHOT_PATH = "state/demo-vm-autosave.snap";

  let autosaveTimer: number | null = null;
  let workerClient: DemoVmWorkerClient | null = null;
  let workerReady = false;
  let autosaveInFlight = false;
  let vm: InstanceType<WasmApi["DemoVm"]> | null = null;
  let mainStepTimer: number | null = null;
  let mainThreadStarted = false;
  const savedSerialBytesByPath = new Map<string, number | null>();

  let steps = 0;
  let serialBytes: number | null = 0;
  let unloadHandlerAttached = false;

  // Expose current snapshot panel state for Playwright smoke tests.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const testState = ((globalThis as any).__aeroDemoVmSnapshot = {
    ready: false,
    streaming: false,
    error: null as string | null,
  });

  function clearError(): void {
    error.textContent = "";
    testState.error = null;
  }

  function clearAutosaveTimer(): void {
    if (autosaveTimer !== null) {
      window.clearInterval(autosaveTimer);
      autosaveTimer = null;
    }
    autosaveInFlight = false;
  }

  function setError(err: unknown): void {
    const message = err instanceof Error ? err.message : String(err);
    error.textContent = message;
    testState.error = message;
    console.error(err);
  }

  function formatSerialBytes(value: number | null): string {
    return value === null ? "unknown" : value.toLocaleString();
  }

  function setButtonsEnabled(enabled: boolean): void {
    saveButton.disabled = !enabled;
    loadButton.disabled = !enabled;
    advanceButton.disabled = !enabled;
    exportButton.disabled = !enabled;
    deleteButton.disabled = !enabled;
    autosaveInput.disabled = !enabled;
    importInput.disabled = !enabled;
  }

  function getSerialOutputLenFromVm(current: InstanceType<WasmApi["DemoVm"]>): number | null {
    const fn = current.serial_output_len;
    if (typeof fn !== "function") return null;
    try {
      const value = fn.call(current);
      if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return null;
      return value;
    } catch {
      return null;
    }
  }

  function stopMainStepLoop(): void {
    if (mainStepTimer !== null) {
      window.clearInterval(mainStepTimer);
      mainStepTimer = null;
    }
  }

  function updateOutputState(nextSteps: number, nextSerialBytes: number | null): void {
    steps = nextSteps;
    serialBytes = nextSerialBytes;
    output.textContent =
      `steps=${steps.toLocaleString()} ` +
      `serial_bytes=${nextSerialBytes === null ? "unknown" : nextSerialBytes.toLocaleString()}`;
  }

  function startMainStepLoop(): void {
    stopMainStepLoop();
    if (!vm) return;
    const STEPS_PER_TICK = 5_000;
    const TICK_MS = 250;
    const timer = window.setInterval(() => {
      try {
        const current = vm;
        if (!current) return;
        current.run_steps(STEPS_PER_TICK);
        const maybeLen = getSerialOutputLenFromVm(current);
        if (maybeLen !== null) {
          // Demo VM writes one serial byte per step; treat serial length as a proxy for total steps.
          updateOutputState(maybeLen, maybeLen);
        } else {
          steps += STEPS_PER_TICK;
          if (serialBytes !== null) serialBytes += STEPS_PER_TICK;
          updateOutputState(steps, serialBytes);
        }
      } catch (err) {
        setError(err);
        stopMainStepLoop();
      }
    }, TICK_MS);
    (timer as unknown as { unref?: () => void }).unref?.();
    mainStepTimer = timer as unknown as number;
  }

  function handleWorkerFatal(err: Error): void {
    clearAutosaveTimer();
    status.textContent = "Demo VM unavailable (worker crashed)";
    setError(err);
    setButtonsEnabled(false);
    workerReady = false;
    workerClient?.terminate();
    workerClient = null;
    testState.ready = false;
    testState.streaming = false;
  }

  function ensureUnloadHandler(): void {
    if (unloadHandlerAttached) return;
    unloadHandlerAttached = true;
    window.addEventListener(
      "pagehide",
      (ev) => {
        // `pagehide` fires for both real navigations and BFCache. If the page is being
        // preserved (`persisted=true`), keep the worker so the demo VM continues to work
        // when the page is restored.
        if ("persisted" in ev && (ev as PageTransitionEvent).persisted) return;
        workerClient?.terminate();
        workerClient = null;
      },
      { passive: true },
    );
  }

  async function restoreSnapshotFromOpfs(): Promise<{ sizeBytes: number; serialBytes: number | null } | null> {
    const file = await getOpfsFileIfExists(SNAPSHOT_PATH);
    if (!file) return null;

    if (workerReady && workerClient) {
      const restore = await workerClient.restoreFromOpfs(SNAPSHOT_PATH, { timeoutMs: 120_000 });
      return { sizeBytes: file.size, serialBytes: restore.serialBytes };
    }

    if (vm) {
      stopMainStepLoop();
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        vm.restore_snapshot(bytes);
        const maybeLen = getSerialOutputLenFromVm(vm);
        if (maybeLen !== null) {
          updateOutputState(maybeLen, maybeLen);
          return { sizeBytes: file.size, serialBytes: maybeLen };
        }
        const saved = savedSerialBytesByPath.get(SNAPSHOT_PATH);
        if (typeof saved === "number") {
          updateOutputState(saved, saved);
          return { sizeBytes: file.size, serialBytes: saved };
        }

        // If the WASM build lacks `serial_output_len`, fall back to copying the buffer once
        // during restore (avoid doing this repeatedly in the main stepping loop).
        const len = vm.serial_output().byteLength;
        savedSerialBytesByPath.set(SNAPSHOT_PATH, len);
        updateOutputState(len, len);
        return { sizeBytes: file.size, serialBytes: len };
      } finally {
        startMainStepLoop();
      }
    }

    throw new Error("Demo VM not initialized");
  }

  async function saveSnapshot(): Promise<void> {
    if (workerReady && workerClient) {
      const snap = await workerClient.snapshotFullToOpfs(SNAPSHOT_PATH, { timeoutMs: 120_000 });
      const file = await getOpfsFileIfExists(SNAPSHOT_PATH);
      status.textContent = `Saved snapshot (${formatMaybeBytes(file?.size ?? null)}) serial_bytes=${formatSerialBytes(
        snap.serialBytes,
      )}`;
      return;
    }

    if (!vm) throw new Error("Demo VM not initialized");
    stopMainStepLoop();
    try {
      const bytes = vm.snapshot_full();
      await writeBytesToOpfs(SNAPSHOT_PATH, bytes);
      const savedSerial = getSerialOutputLenFromVm(vm) ?? serialBytes ?? null;
      savedSerialBytesByPath.set(SNAPSHOT_PATH, savedSerial);
      const file = await getOpfsFileIfExists(SNAPSHOT_PATH);
      status.textContent = `Saved snapshot (${formatMaybeBytes(file?.size ?? null)}) serial_bytes=${formatSerialBytes(
        savedSerial,
      )}`;
    } finally {
      startMainStepLoop();
    }
  }

  async function loadSnapshot(): Promise<void> {
    const restored = await restoreSnapshotFromOpfs();
    if (!restored) {
      status.textContent = "No snapshot found in OPFS.";
      return;
    }
    status.textContent = `Loaded snapshot (${formatBytes(restored.sizeBytes)}) serial_bytes=${formatSerialBytes(
      restored.serialBytes,
    )}`;
  }

  function setAutosave(seconds: number): void {
    clearAutosaveTimer();
    if (!Number.isFinite(seconds) || seconds <= 0) {
      status.textContent = "Auto-save disabled.";
      return;
    }
    const timer = window.setInterval(() => {
      if (autosaveInFlight) return;
      autosaveInFlight = true;
      saveSnapshot()
        .catch((err) => setError(err))
        .finally(() => {
          autosaveInFlight = false;
        });
    }, seconds * 1000);
    (timer as unknown as { unref?: () => void }).unref?.();
    autosaveTimer = timer as unknown as number;
    status.textContent = `Auto-save every ${seconds}s.`;
  }

  autosaveInput.addEventListener("change", () => {
    const seconds = Number.parseInt(autosaveInput.value, 10);
    setAutosave(seconds);
  });

  saveButton.onclick = () => {
    clearError();
    saveSnapshot().catch((err) => setError(err));
  };

  loadButton.onclick = () => {
    clearError();
    loadSnapshot().catch((err) => setError(err));
  };

  advanceButton.onclick = () => {
    clearError();
    const client = workerClient;
    if (workerReady && client) {
      client
        .runSteps(50_000, { timeoutMs: 30_000 })
        .then((state) => updateOutputState(state.steps, state.serialBytes))
        .catch((err) => setError(err));
      return;
    }

    if (!vm) return;
    try {
      vm.run_steps(50_000);
      const maybeLen = getSerialOutputLenFromVm(vm);
      if (maybeLen !== null) {
        updateOutputState(maybeLen, maybeLen);
      } else {
        steps += 50_000;
        if (serialBytes !== null) serialBytes += 50_000;
        updateOutputState(steps, serialBytes);
      }
    } catch (err) {
      setError(err);
    }
  };

  exportButton.onclick = () => {
    clearError();
    getOpfsFileIfExists(SNAPSHOT_PATH)
      .then((file) => {
        if (!file) {
          status.textContent = "No snapshot found to export.";
          return;
        }
        downloadFile(file, "aero-demo-vm.snap");
        status.textContent = `Exported snapshot (${formatBytes(file.size)})`;
      })
      .catch((err) => setError(err));
  };

  deleteButton.onclick = () => {
    clearError();
    removeOpfsEntry(SNAPSHOT_PATH)
      .then(() => {
        savedSerialBytesByPath.delete(SNAPSHOT_PATH);
        status.textContent = "Deleted snapshot from OPFS.";
      })
      .catch((err) => setError(err));
  };

  importInput.addEventListener("change", () => {
    void (async () => {
      clearError();
      const file = importInput.files?.[0];
      if (!file) return;
      status.textContent = `Importing snapshot (${formatBytes(file.size)})…`;
      savedSerialBytesByPath.delete(SNAPSHOT_PATH);
      await importFileToOpfs(file, SNAPSHOT_PATH, (progress) => {
        status.textContent = `Importing snapshot: ${formatBytes(progress.writtenBytes)} / ${formatBytes(progress.totalBytes)}`;
      });
      const restored = await restoreSnapshotFromOpfs();
      status.textContent = `Imported snapshot (${formatBytes(file.size)}) serial_bytes=${formatSerialBytes(
        restored?.serialBytes ?? null,
      )}`;
      importInput.value = "";
    })().catch((err) => setError(err));
  });

  if (!report.opfs) {
    clearAutosaveTimer();
    status.textContent = "Snapshots unavailable (OPFS missing).";
    setButtonsEnabled(false);
    setError("OPFS is unavailable in this browser/context (navigator.storage.getDirectory missing).");
    // Initialization is complete but the feature is unavailable.
    testState.ready = true;
    testState.streaming = false;
  } else {
    function startMainThreadVm(reason: string): void {
      if (mainThreadStarted) return;
      mainThreadStarted = true;
      status.textContent = "Initializing demo VM (main thread)…";

      wasmInitPromise
        .then(async ({ api, variant }) => {
          vm = new api.DemoVm(256 * 1024);
          status.textContent = `Demo VM ready (main thread, WASM ${variant}). ${reason}`;
          setButtonsEnabled(true);
          testState.ready = true;
          testState.streaming = false;
          startMainStepLoop();

          // Best-effort crash recovery: try to restore the last autosave snapshot.
          try {
            const restored = await restoreSnapshotFromOpfs();
            if (restored) {
              status.textContent = `Restored snapshot (${formatBytes(restored.sizeBytes)}) serial_bytes=${formatSerialBytes(
                restored.serialBytes,
              )}`;
            }
          } catch (err) {
            setError(err);
          }
        })
        .catch((err) => {
          status.textContent = "Demo VM unavailable (WASM init failed)";
          setButtonsEnabled(false);
          setError(err);
          testState.ready = true;
          testState.streaming = false;
        });
    }

    // Prefer streaming snapshots (Dedicated Worker + sync access handles) when available.
    // Otherwise, fall back to main-thread in-memory snapshots for compatibility.
    if (!report.opfsSyncAccessHandle) {
      startMainThreadVm("OPFS sync access handles unavailable; using in-memory snapshots.");
    } else {
      setButtonsEnabled(false);
      status.textContent = "Initializing demo VM worker…";

      try {
        ensureUnloadHandler();
        workerClient = new DemoVmWorkerClient({
          onStatus: (state) => updateOutputState(state.steps, state.serialBytes),
          onError: (err) => setError(err),
          onFatalError: (err) => handleWorkerFatal(err),
        });
      } catch (err) {
        clearAutosaveTimer();
        status.textContent = "Demo VM unavailable (worker creation failed)";
        setError(err);
        workerClient = null;
        testState.ready = false;
        testState.streaming = false;
        startMainThreadVm("Worker creation failed; using in-memory snapshots.");
      }

      if (workerClient) {
        void (async () => {
          try {
            const init = await workerClient!.init(256 * 1024, { timeoutMs: 60_000 });

            if (!init.syncAccessHandles) {
              workerClient?.terminate();
              workerClient = null;
              startMainThreadVm("OPFS sync access handles unavailable; using in-memory snapshots.");
              return;
            }

            if (!init.streamingSnapshots || !init.streamingRestore) {
              workerClient?.terminate();
              workerClient = null;
              startMainThreadVm("Snapshot streaming APIs missing; using in-memory snapshots.");
              return;
            }

            workerReady = true;
            status.textContent = `Demo VM worker ready (WASM ${init.wasmVariant}). Running…`;
            setButtonsEnabled(true);
            testState.ready = true;
            testState.streaming = true;

            // Best-effort crash recovery: try to restore the last autosave snapshot.
            try {
              const restored = await restoreSnapshotFromOpfs();
              if (restored) {
                status.textContent = `Restored snapshot (${formatBytes(restored.sizeBytes)}) serial_bytes=${formatSerialBytes(
                  restored.serialBytes,
                )}`;
              }
            } catch (err) {
              setError(err);
            }
          } catch (err) {
            workerClient?.terminate();
            workerClient = null;
            startMainThreadVm("Worker init failed; using in-memory snapshots.");
          }
        })();
      }
    }
  }

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Snapshots (demo VM + OPFS autosave)" }),
    el(
      "div",
      { class: "row" },
      saveButton,
      loadButton,
      advanceButton,
      exportButton,
      deleteButton,
      el("label", { text: "Auto-save (seconds):" }),
      autosaveInput,
      el("label", { text: "Import:" }),
      importInput,
    ),
    status,
    output,
    error,
  );
}

function renderWebGpuPanel(): HTMLElement {
  const output = el("pre", { text: "" });
  const button = el("button", {
    text: "Request WebGPU device",
    onclick: async () => {
      output.textContent = "";
      try {
        const { adapter, preferredFormat } = await requestWebGpuDevice({ powerPreference: "high-performance" });
        output.textContent = JSON.stringify(
          {
            adapterInfo: "requestAdapter succeeded",
            features: Array.from(adapter.features.values()),
            preferredFormat,
          },
          null,
          2,
        );
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  return el("div", { class: "panel" }, el("h2", { text: "WebGPU" }), el("div", { class: "row" }, button), output);
}

function renderGpuWorkerPanel(): HTMLElement {
  const output = el("pre", { text: "" });
  const canvas = el("canvas") as HTMLCanvasElement;

  const cssWidth = 64;
  const cssHeight = 64;
  const devicePixelRatio = window.devicePixelRatio || 1;

  canvas.width = Math.max(1, Math.round(cssWidth * devicePixelRatio));
  canvas.height = Math.max(1, Math.round(cssHeight * devicePixelRatio));
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;
  canvas.style.border = "1px solid #333";
  canvas.style.imageRendering = "pixelated";

  function appendLog(line: string): void {
    output.textContent += `${line}\n`;
  }

  let runtime: GpuRuntime | null = null;

  const button = el("button", {
    text: "Run GPU runtime smoke test",
    onclick: async () => {
      output.textContent = "";

      try {
        if (!runtime) {
          runtime = new GpuRuntime();
          await runtime.init(canvas, cssWidth, cssHeight, devicePixelRatio, {
            mode: "auto",
            gpuOptions: { preferWebGpu: true },
            onError: (msg) => {
              appendLog(`gpu error: ${msg.message}${msg.code ? ` (code=${msg.code})` : ""}`);
            },
          });
          if (runtime.workerReady) {
            const ready = runtime.workerReady;
            appendLog(`ready backend=${ready.backendKind}`);
            if (ready.fallback) {
              appendLog(`fallback ${ready.fallback.from} -> ${ready.fallback.to}: ${ready.fallback.reason}`);
            }
          } else {
            appendLog(`ready backend=${runtime.backendKind ?? "webgl2"} (main-thread)`);
          }

          appendLog(`runtime mode=${runtime.mode} backend=${runtime.backendKind ?? "n/a"}`);
        }

        await runtime.present();
        const screenshot = await runtime.screenshot();

        const actual = new Uint8Array(screenshot.data.buffer, screenshot.data.byteOffset, screenshot.data.byteLength);
        const expected = createExpectedTestPattern(screenshot.width, screenshot.height);

        const actualHash = fnv1a32Hex(actual);
        const expectedHash = fnv1a32Hex(expected);

        appendLog(`screenshot ${screenshot.width}x${screenshot.height} rgba8 bytes=${actual.byteLength}`);
        appendLog(`hash actual=${actualHash} expected=${expectedHash}`);
        appendLog(actualHash === expectedHash ? "PASS" : "FAIL");
      } catch (err) {
        appendLog(err instanceof Error ? err.message : String(err));
      }
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "GPU Runtime" }),
    el("div", { class: "row" }, button, canvas),
    output,
  );
}

function renderSm5TrianglePanel(): HTMLElement {
  const output = el("pre", { text: "" });
  const canvas = el("canvas") as HTMLCanvasElement;

  const cssWidth = 320;
  const cssHeight = 320;
  const devicePixelRatio = window.devicePixelRatio || 1;

  canvas.width = Math.max(1, Math.round(cssWidth * devicePixelRatio));
  canvas.height = Math.max(1, Math.round(cssHeight * devicePixelRatio));
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;
  canvas.style.border = "1px solid #2a2a2a";

  // WGSL below is intentionally kept in sync with the current output of the
  // `crates/aero-d3d11` bootstrap DXBC→WGSL translator for the synthetic SM5
  // passthrough shaders (mov o*, v*; ret).
  const vsWgsl = `
struct VsIn {
  @location(0) v0: vec4<f32>,
  @location(1) v1: vec4<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) o1: vec4<f32>,
};

@vertex
fn main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.pos = input.v0;
  out.o1 = input.v1;
  return out;
}
`;

  const psWgsl = `
struct PsIn {
  @builtin(position) pos: vec4<f32>,
  @location(0) v1: vec4<f32>,
};

@fragment
fn main(input: PsIn) -> @location(0) vec4<f32> {
  return input.v1;
}
`;

  const button = el("button", {
    text: "Render SM5 passthrough triangle",
    onclick: async () => {
      output.textContent = "";
      try {
        const { device, preferredFormat } = await requestWebGpuDevice({ powerPreference: "high-performance" });
        const context = createWebGpuCanvasContext(canvas, device, preferredFormat);

        const vertexModule = device.createShaderModule({ code: vsWgsl });
        const fragmentModule = device.createShaderModule({ code: psWgsl });

        const pipeline = device.createRenderPipeline({
          layout: "auto",
          vertex: {
            module: vertexModule,
            entryPoint: "main",
            buffers: [
              {
                arrayStride: 32,
                attributes: [
                  { shaderLocation: 0, offset: 0, format: "float32x4" },
                  { shaderLocation: 1, offset: 16, format: "float32x4" },
                ],
              },
            ],
          },
          fragment: {
            module: fragmentModule,
            entryPoint: "main",
            targets: [{ format: preferredFormat }],
          },
          primitive: {
            topology: "triangle-list",
          },
        });

        const vertices = new Float32Array([
          // position (x,y,z,w), color (r,g,b,a)
          0.0, 0.7, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0,
          -0.7, -0.7, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0,
          0.7, -0.7, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0,
        ]);

        const vertexBuffer = device.createBuffer({
          size: vertices.byteLength,
          usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
        });
        device.queue.writeBuffer(vertexBuffer, 0, vertices);

        const encoder = device.createCommandEncoder();
        const pass = encoder.beginRenderPass({
          colorAttachments: [
            {
              view: context.getCurrentTexture().createView(),
              clearValue: { r: 0.06, g: 0.06, b: 0.08, a: 1.0 },
              loadOp: "clear",
              storeOp: "store",
            },
          ],
        });
        pass.setPipeline(pipeline);
        pass.setVertexBuffer(0, vertexBuffer);
        pass.draw(3, 1, 0, 0);
        pass.end();

        device.queue.submit([encoder.finish()]);
        output.textContent = "Rendered.";
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "SM5 passthrough triangle (WebGPU)" }),
    el("div", { class: "row" }, button),
    canvas,
    output,
  );
}

function renderOpfsPanel(): HTMLElement {
  const quotaLine = el("div", { class: "mono", text: "Storage quota: loading…" });
  const persistenceLine = el("div", { class: "mono", text: "Persistent storage: loading…" });
  const persistenceResult = el("div", { class: "mono", text: "" });

  const refreshButton = el("button", {
    text: "Refresh storage info",
    onclick: async () => {
      persistenceResult.textContent = "";
      await refreshStorageInfo();
    },
  });

  const requestPersistenceButton = el("button", {
    text: "Request persistent storage",
    onclick: async () => {
      persistenceResult.textContent = "";
      const result = await ensurePersistentStorage();
      if (!result.supported) {
        persistenceResult.textContent = "Persistent storage request is not supported in this browser.";
      } else if (result.granted) {
        persistenceResult.textContent = "Persistent storage granted.";
      } else {
        persistenceResult.textContent = "Persistent storage not granted (denied or unavailable).";
      }
      await refreshStorageInfo();
    },
  });

  const status = el("pre", { text: "" });
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;
  const destPathInput = el("input", { type: "text", value: "images/disk.img" }) as HTMLInputElement;
  const fileInput = el("input", { type: "file" }) as HTMLInputElement;

  async function refreshStorageInfo(): Promise<void> {
    const estimate = await getStorageEstimate();
    if (!estimate.supported) {
      quotaLine.className = "mono";
      quotaLine.textContent = "Storage quota: unsupported in this browser.";
    } else if (
      estimate.usageBytes === null ||
      estimate.quotaBytes === null ||
      estimate.usagePercent === null ||
      estimate.remainingBytes === null
    ) {
      quotaLine.className = "mono";
      quotaLine.textContent = "Storage quota: unavailable.";
    } else {
      quotaLine.className = estimate.warning ? "mono bad" : "mono";
      const percent = estimate.usagePercent.toFixed(1);
      quotaLine.textContent = `Storage quota: ${formatBytes(estimate.usageBytes)} used / ${formatBytes(
        estimate.quotaBytes,
      )} quota (${percent}% used, ${formatBytes(estimate.remainingBytes)} free)${estimate.warning ? " (warning: >80%)" : ""}`;
    }

    const persistence = await getPersistentStorageInfo();
    if (!persistence.supported) {
      persistenceLine.className = "mono";
      persistenceLine.textContent = "Persistent storage: unsupported in this browser.";
      return;
    }
    if (persistence.persisted === null) {
      persistenceLine.className = "mono";
      persistenceLine.textContent = "Persistent storage: unknown.";
      return;
    }
    persistenceLine.className = persistence.persisted ? "mono ok" : "mono bad";
    persistenceLine.textContent = persistence.persisted ? "Persistent storage: granted." : "Persistent storage: not granted.";
  }

  const importButton = el("button", {
    text: "Import to OPFS",
    onclick: async () => {
      status.textContent = "";
      progress.value = 0;
      const file = fileInput.files?.[0];
      if (!file) {
        status.textContent = "Pick a file first.";
        return;
      }
      const destPath = destPathInput.value.trim();
      if (!destPath) {
        status.textContent = "Destination path must not be empty.";
        return;
      }

      try {
        const estimate = await getStorageEstimate();
        if (estimate.supported && estimate.remainingBytes !== null) {
          // OPFS metadata + internal fragmentation can require extra headroom.
          const safetyMarginBytes = Math.max(50 * 1024 * 1024, Math.floor(file.size * 0.05));
          const requiredBytes = file.size + safetyMarginBytes;

          if (estimate.remainingBytes < requiredBytes) {
            const ok = window.confirm(
              `Estimated remaining browser storage (${formatMaybeBytes(estimate.remainingBytes)}) is less than the recommended free space (${formatBytes(
                requiredBytes,
              )}) for this import.\n\nThe import may fail or the browser may evict data.\n\nContinue anyway?`,
            );
            if (!ok) return;
          }
        }

        await importFileToOpfs(file, destPath, ({ writtenBytes, totalBytes }) => {
          progress.value = totalBytes ? writtenBytes / totalBytes : 0;
          status.textContent = `Writing ${writtenBytes.toLocaleString()} / ${totalBytes.toLocaleString()} bytes…`;
        });
        status.textContent = `Imported to OPFS: ${destPath}`;
        await refreshStorageInfo();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  fileInput.addEventListener("change", () => {
    const file = fileInput.files?.[0];
    if (file) destPathInput.value = `images/${file.name}`;
  });

  const panel = el(
    "div",
    { class: "panel" },
    el("h2", { text: "OPFS tools" }),
    el("h3", { text: "Quota & durability" }),
    el("div", { class: "row" }, refreshButton, requestPersistenceButton),
    quotaLine,
    persistenceLine,
    persistenceResult,
    el("h3", { text: "Import" }),
    el(
      "div",
      { class: "row" },
      el("label", { text: "File:" }),
      fileInput,
      el("label", { text: "Dest path (relative):" }),
      destPathInput,
      importButton,
      progress,
    ),
    status,
  );

  refreshStorageInfo().catch((err) => {
    quotaLine.className = "mono bad";
    quotaLine.textContent = `Storage quota: error (${err instanceof Error ? err.message : String(err)})`;
    persistenceLine.className = "mono bad";
    persistenceLine.textContent = "Persistent storage: error.";
  });

  return panel;
}

function renderNetTracePanel(): HTMLElement {
  const panel = el("div", { class: "panel" }, el("h2", { text: "Network trace (PCAPNG)" }));

  function requireWorkersRunning(): void {
    const state = workerCoordinator.getVmState();
    if (state === "stopped" || state === "poweredOff") {
      throw new Error("Workers are not running. Start workers before using network tracing.");
    }
    if (state === "failed") {
      throw new Error("Workers are in a failed state. Restart workers before using network tracing.");
    }
  }

  installNetTraceUI(panel, {
    isEnabled: () => workerCoordinator.isNetTraceEnabled(),
    enable: () => {
      workerCoordinator.setNetTraceEnabled(true);
    },
    disable: () => {
      workerCoordinator.setNetTraceEnabled(false);
    },
    clear: () => {
      // Best-effort: allow clearing even if the net worker isn't running yet.
      // A newly started worker has an empty capture, and if it's already running
      // we forward the clear command immediately.
      workerCoordinator.clearNetTrace();
    },
    getStats: async () => {
      // Avoid spamming the UI with errors when the VM/net worker isn't running yet.
      // We can only fetch real stats once the net worker is ready.
      const state = workerCoordinator.getVmState();
      if (state === "stopped" || state === "poweredOff" || state === "failed") {
        return { enabled: workerCoordinator.isNetTraceEnabled(), records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 };
      }
      const netStatus = workerCoordinator.getWorkerStatuses().net;
      if (netStatus.state !== "ready") {
        return { enabled: workerCoordinator.isNetTraceEnabled(), records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 };
      }
      return await workerCoordinator.getNetTraceStats(1000);
    },
    downloadPcapng: async () => {
      requireWorkersRunning();
      return await workerCoordinator.takeNetTracePcapng(30_000);
    },
    exportPcapng: async () => {
      requireWorkersRunning();
      return await workerCoordinator.exportNetTracePcapng(30_000);
    },
  });

  return panel;
}

function renderDisksPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;
  progress.hidden = true;
  const progressText = el("span", { class: "muted", text: "" });

  const headerLine = el("div", { class: "mono", text: "Initializing disk manager…" });

  const createNameInput = el("input", { type: "text", placeholder: "Blank HDD name (e.g. win7.img)" }) as HTMLInputElement;
  const createSizeGiB = el("input", { type: "number", min: "1", step: "1", value: "20" }) as HTMLInputElement;

  const importNameInput = el("input", { type: "text", placeholder: "Optional display name (defaults to file name)" }) as HTMLInputElement;
  const fileInput = el("input", { type: "file", style: "display: none" }) as HTMLInputElement;
  let importMode: "direct" | "convert" = "direct";

  const hddSelect = el("select") as HTMLSelectElement;
  const cdSelect = el("select") as HTMLSelectElement;

  const tableBody = el("tbody");

  let manager: DiskManager | null = null;
  let disks: DiskImageMetadata[] = [];
  let mounts: MountConfig = {};
  let adoptedLegacy = false;

  function setProgress(phase: string, processed: number, total?: number): void {
    progress.hidden = false;
    progress.value = total ? Math.min(processed, total) : processed;
    progress.max = total ? Math.max(total, 1) : Math.max(processed, 1);
    const pct = total && total > 0 ? ` (${Math.floor((processed / total) * 100)}%)` : "";
    progressText.textContent = `${phase}: ${formatBytes(processed)}${total ? ` / ${formatBytes(total)}` : ""}${pct}`;
  }

  function clearProgress(): void {
    progress.hidden = true;
    progress.value = 0;
    progress.max = 1;
    progressText.textContent = "";
  }

  function diskLocationLabel(meta: DiskImageMetadata): string {
    if (meta.source === "remote") {
      return `remote:${meta.remote.delivery} cache=${meta.cache.backend}`;
    }
    if (meta.backend !== "opfs") return meta.backend;
    if (meta.remote) return "remote+opfs-cache";
    const dir = meta.opfsDirectory ?? OPFS_DISKS_PATH;
    return dir === OPFS_DISKS_PATH ? "opfs" : `opfs:${dir}`;
  }

  function diskRow(meta: DiskImageMetadata): HTMLTableRowElement {
    const mountHdd = el("button", {
      text: "Mount HDD",
      onclick: async () => {
        if (!manager) return;
        status.textContent = "";
        try {
          mounts = await manager.setMounts({ ...mounts, hddId: meta.kind === "hdd" ? meta.id : mounts.hddId });
          await refresh();
        } catch (err) {
          status.textContent = err instanceof Error ? err.message : String(err);
        }
      },
      disabled: meta.kind !== "hdd" ? "true" : undefined,
    }) as HTMLButtonElement;

    const mountCd = el("button", {
      text: "Mount CD",
      onclick: async () => {
        if (!manager) return;
        status.textContent = "";
        try {
          mounts = await manager.setMounts({ ...mounts, cdId: meta.kind === "cd" ? meta.id : mounts.cdId });
          await refresh();
        } catch (err) {
          status.textContent = err instanceof Error ? err.message : String(err);
        }
      },
      disabled: meta.kind !== "cd" ? "true" : undefined,
    }) as HTMLButtonElement;

    const exportBtn = el("button", {
      text: "Export",
      onclick: async () => {
        if (!manager) return;
        status.textContent = "";
        clearProgress();
        try {
          await manager.exportDiskToFile(meta.id, {
            onProgress(p) {
              setProgress(p.phase, p.processedBytes, p.totalBytes);
            },
          });
          status.textContent = "Export complete.";
        } catch (err) {
          status.textContent = err instanceof Error ? err.message : String(err);
        } finally {
          clearProgress();
        }
      },
      disabled: meta.remote ? "true" : undefined,
    }) as HTMLButtonElement;

    const resizeBtn = el("button", {
      text: "Resize",
      onclick: async () => {
        if (!manager) return;
        const curGiB = (meta.sizeBytes / (1024 * 1024 * 1024)).toFixed(2);
        const input = window.prompt(`New size in GiB (current ~${curGiB} GiB):`, curGiB);
        if (!input) return;
        const newGiB = Number(input);
        if (!Number.isFinite(newGiB) || newGiB <= 0) {
          status.textContent = "Invalid size.";
          return;
        }
        const newBytes = Math.floor(newGiB * 1024 * 1024 * 1024);

        status.textContent = "";
        clearProgress();
        try {
          await manager.resizeDisk(meta.id, newBytes, {
            onProgress(p) {
              setProgress(p.phase, p.processedBytes, p.totalBytes);
            },
          });
          await refresh();
        } catch (err) {
          status.textContent = err instanceof Error ? err.message : String(err);
        } finally {
          clearProgress();
        }
      },
      disabled: meta.kind !== "hdd" || !!meta.remote ? "true" : undefined,
    }) as HTMLButtonElement;

    const deleteBtn = el("button", {
      text: "Delete",
      onclick: async () => {
        if (!manager) return;
        if (!confirm(`Delete disk "${meta.name}"?`)) return;
        status.textContent = "";
        try {
          await manager.deleteDisk(meta.id);
          await refresh();
        } catch (err) {
          status.textContent = err instanceof Error ? err.message : String(err);
        }
      },
    }) as HTMLButtonElement;

    const mountedLabel =
      mounts.hddId === meta.id ? "HDD" : mounts.cdId === meta.id ? "CD" : "";

    return el(
      "tr",
      {},
      el("td", { text: mountedLabel }),
      el("td", { text: meta.name }),
      el("td", { text: meta.kind }),
      el("td", { text: meta.format }),
      el("td", { text: formatBytes(meta.sizeBytes) }),
      el("td", { text: diskLocationLabel(meta) }),
      el("td", { class: "actions" }, mountHdd, mountCd, exportBtn, resizeBtn, deleteBtn),
    );
  }

  function renderTable(): void {
    tableBody.replaceChildren();
    if (disks.length === 0) {
      tableBody.append(el("tr", {}, el("td", { colspan: "7", class: "muted", text: "No disks yet." })));
      return;
    }
    for (const meta of disks) {
      tableBody.append(diskRow(meta));
    }
  }

  function renderMountSelects(): void {
    function fill(select: HTMLSelectElement, kind: "hdd" | "cd", selectedId?: string): void {
      select.replaceChildren();
      select.append(el("option", { value: "", text: "(none)" }));
      for (const d of disks) {
        if (d.kind !== kind) continue;
        let label = d.name;
        if (d.source === "remote") {
          label = `${d.name} (remote:${d.remote.delivery})`;
        } else if (d.remote) {
          label = `${d.name} (remote)`;
        } else if (d.opfsDirectory === OPFS_LEGACY_IMAGES_DIR) {
          label = `${d.name} (legacy)`;
        }
        select.append(el("option", { value: d.id, text: label }));
      }
      select.value = selectedId ?? "";
    }

    fill(hddSelect, "hdd", mounts.hddId);
    fill(cdSelect, "cd", mounts.cdId);
  }

  async function refresh(): Promise<void> {
    if (!manager) {
      manager = await diskManagerPromise;
      headerLine.textContent = `Disk backend: ${manager.backend}`;
    }

    // Auto-adopt legacy images once per session so users upgrading from the v1
    // disk storage flow see their existing OPFS `images/` files.
    if (!adoptedLegacy && manager.backend === "opfs") {
      adoptedLegacy = true;
      try {
        await manager.adoptLegacyOpfsImages();
      } catch {
        // ignore
      }
    }

    disks = await manager.listDisks();
    mounts = await manager.getMounts();
    renderMountSelects();
    renderTable();
  }

  const refreshBtn = el("button", {
    text: "Refresh",
    onclick: () => {
      status.textContent = "";
      void refresh().catch((err) => {
        status.textContent = err instanceof Error ? err.message : String(err);
      });
    },
  }) as HTMLButtonElement;

  const adoptLegacyBtn = el("button", {
    text: "Scan legacy OPFS images/",
    onclick: async () => {
      status.textContent = "";
      if (!manager) manager = await diskManagerPromise;
      try {
        const res = await manager.adoptLegacyOpfsImages();
        status.textContent = `Legacy scan: adopted=${res.adopted} found=${res.found}`;
        await refresh();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  const createBtn = el("button", {
    text: "Create blank HDD",
    onclick: async () => {
      status.textContent = "";
      clearProgress();
      if (!manager) manager = await diskManagerPromise;
      const name = createNameInput.value.trim() || "blank.img";
      const giB = Number(createSizeGiB.value);
      const sizeBytes = Math.floor(giB * 1024 * 1024 * 1024);
      if (!Number.isFinite(sizeBytes) || sizeBytes <= 0) {
        status.textContent = "Invalid size.";
        return;
      }
      try {
        await manager.createBlankDisk({
          name,
          sizeBytes,
          onProgress(p) {
            setProgress(p.phase, p.processedBytes, p.totalBytes);
          },
        });
        await refresh();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        clearProgress();
      }
    },
  }) as HTMLButtonElement;

  const importBtn = el("button", {
    text: "Import file…",
    onclick: () => {
      importMode = "direct";
      status.textContent = "";
      clearProgress();
      fileInput.value = "";
      fileInput.click();
    },
  }) as HTMLButtonElement;

  const importConvertBtn = el("button", {
    text: "Import + convert…",
    onclick: () => {
      importMode = "convert";
      status.textContent = "";
      clearProgress();
      fileInput.value = "";
      fileInput.click();
    },
  }) as HTMLButtonElement;

  fileInput.addEventListener("change", () => {
    const file = fileInput.files?.[0];
    if (!file) return;
    void (async () => {
      if (!manager) manager = await diskManagerPromise;
      status.textContent = "";
      clearProgress();

      try {
        const nameOverride = importNameInput.value.trim() || undefined;
        if (importMode === "convert") {
          await manager.importDiskConverted(file, {
            name: nameOverride,
            onProgress(p) {
              setProgress(p.phase, p.processedBytes, p.totalBytes);
            },
          });
        } else {
          await manager.importDisk(file, {
            name: nameOverride,
            onProgress(p) {
              setProgress(p.phase, p.processedBytes, p.totalBytes);
            },
          });
        }
        await refresh();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        clearProgress();
      }
    })();
  });

  const remoteUrlInput = el("input", { type: "url", placeholder: "https://example.com/win7.img" }) as HTMLInputElement;
  const remoteBlockKiB = el("input", { type: "number", min: "4", step: "4", value: "1024" }) as HTMLInputElement;
  const remoteCacheMiB = el("input", { type: "number", min: "0", step: "64", value: "512" }) as HTMLInputElement;

  const addRemoteBtn = el("button", {
    text: "Add remote disk",
    onclick: async () => {
      if (!manager) manager = await diskManagerPromise;
      status.textContent = "";
      const url = remoteUrlInput.value.trim();
      if (!url) {
        status.textContent = "Enter a URL.";
        return;
      }
      const blockSizeBytes = Number(remoteBlockKiB.value) * 1024;
      const cacheLimitMiB = Number(remoteCacheMiB.value);
      const cacheLimitBytes = cacheLimitMiB <= 0 ? null : cacheLimitMiB * 1024 * 1024;
      try {
        await manager.addRemoteStreamingDisk({ url, blockSizeBytes, cacheLimitBytes, prefetchSequentialBlocks: 2 });
        await refresh();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  hddSelect.addEventListener("change", () => {
    void (async () => {
      if (!manager) manager = await diskManagerPromise;
      mounts = await manager.setMounts({ ...mounts, hddId: hddSelect.value || undefined });
      await refresh();
    })().catch((err) => {
      status.textContent = err instanceof Error ? err.message : String(err);
    });
  });
  cdSelect.addEventListener("change", () => {
    void (async () => {
      if (!manager) manager = await diskManagerPromise;
      mounts = await manager.setMounts({ ...mounts, cdId: cdSelect.value || undefined });
      await refresh();
    })().catch((err) => {
      status.textContent = err instanceof Error ? err.message : String(err);
    });
  });

  const runtimeTestBtn = el("button", {
    text: "Open mounts via runtime disk worker",
    onclick: async () => {
      status.textContent = "";
      if (!manager) manager = await diskManagerPromise;
      const opened = await openBootDisks(manager);
      try {
        const lines: string[] = [];
        if (opened.hdd) {
          lines.push(`HDD opened: ${opened.hdd.meta.name} (${formatBytes(opened.hdd.open.capacityBytes)})`);
        }
        if (opened.cd) {
          lines.push(`CD opened: ${opened.cd.meta.name} (${formatBytes(opened.cd.open.capacityBytes)})`);
        }
        status.textContent = lines.length ? lines.join("\n") : "No mounts configured.";
      } finally {
        try {
          if (opened.hdd) await opened.client.closeDisk(opened.hdd.open.handle);
          if (opened.cd) await opened.client.closeDisk(opened.cd.open.handle);
        } finally {
          opened.client.close();
        }
      }
    },
  }) as HTMLButtonElement;

  const table = el(
    "table",
    {},
    el(
      "thead",
      {},
      el(
        "tr",
        {},
        el("th", { text: "Mounted" }),
        el("th", { text: "Name" }),
        el("th", { text: "Kind" }),
        el("th", { text: "Format" }),
        el("th", { text: "Size" }),
        el("th", { text: "Location" }),
        el("th", { text: "Actions" }),
      ),
    ),
    tableBody,
  );

  void refresh().catch((err) => {
    status.textContent = err instanceof Error ? err.message : String(err);
  });
  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Disks" }),
    headerLine,
    el("div", { class: "row" }, refreshBtn, adoptLegacyBtn, runtimeTestBtn),
    el("h3", { text: "Mounts" }),
    el(
      "div",
      { class: "row" },
      el("label", { text: "HDD:" }),
      hddSelect,
      el("label", { text: "CD:" }),
      cdSelect,
    ),
    el("h3", { text: "Create blank" }),
    el("div", { class: "row" }, el("label", { text: "Name:" }), createNameInput, el("label", { text: "GiB:" }), createSizeGiB, createBtn),
    el("h3", { text: "Import" }),
    el("div", { class: "row" }, importNameInput, importBtn, importConvertBtn, fileInput),
    el("div", { class: "row" }, progress, progressText),
    el("h3", { text: "Remote" }),
    el(
      "div",
      { class: "row" },
      el("label", { text: "URL:" }),
      remoteUrlInput,
      el("label", { text: "Block KiB:" }),
      remoteBlockKiB,
      el("label", { text: "Cache MiB:" }),
      remoteCacheMiB,
      addRemoteBtn,
    ),
    status,
    table,
  );
}

function renderAudioPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  let toneTimer: number | null = null;
  let tonePhase = 0;
  let toneGeneration = 0;
  let wasmBridge: unknown | null = null;
  let wasmTone: { free(): void } | null = null;
  let stopPerfSampling: (() => void) | null = null;

  let loopbackTimer: number | null = null;
  let syntheticMic: { stop(): void } | null = null;
  let hdaDemoWorker: Worker | null = null;
  let hdaDemoStats: { [k: string]: unknown } | null = null;

  function stopTone() {
    toneGeneration += 1;
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
    if (stopPerfSampling) {
      stopPerfSampling();
      stopPerfSampling = null;
    }
    if (wasmTone) {
      wasmTone.free();
      wasmTone = null;
    }
    if (wasmBridge && typeof (wasmBridge as { free?: () => void }).free === "function") {
      (wasmBridge as { free(): void }).free();
      wasmBridge = null;
    }
  }

  function stopLoopback(): void {
    if (loopbackTimer !== null) {
      window.clearInterval(loopbackTimer);
      loopbackTimer = null;
    }
    syntheticMic?.stop();
    syntheticMic = null;
    // Restore the currently-selected real microphone attachment (if any) so
    // the microphone panel remains functional after toggling loopback.
    if (micAttachment) {
      workerCoordinator.setMicrophoneRingBuffer(micAttachment.ringBuffer, micAttachment.sampleRate);
    } else {
      workerCoordinator.setMicrophoneRingBuffer(null, 0);
    }
    workerCoordinator.setAudioOutputRingBuffer(null, 0, 0, 0);
  }

  function stopHdaDemo(): void {
    if (!hdaDemoWorker) return;
    hdaDemoWorker.postMessage({ type: "audioOutputHdaDemo.stop" });
    hdaDemoWorker.terminate();
    hdaDemoWorker = null;
    hdaDemoStats = null;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroAudioHdaDemoStats = undefined;
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  async function startTone(output: Exclude<Awaited<ReturnType<typeof createAudioOutput>>, { enabled: false }>) {
    stopTone();
    stopHdaDemo();

    const freqHz = 440;
    const gain = 0.1;
    const channelCount = output.ringBuffer.channelCount;
    const sr = output.context.sampleRate;

    // Try to use the WASM-side bridge + sine generator if available; fall back to JS.
    const writeToneJs = (frames: number) => {
      const buf = new Float32Array(frames * channelCount);
      for (let i = 0; i < frames; i++) {
        const s = Math.sin(tonePhase * 2 * Math.PI) * gain;
        for (let c = 0; c < channelCount; c++) buf[i * channelCount + c] = s;
        tonePhase += freqHz / sr;
        if (tonePhase >= 1) tonePhase -= 1;
      }
      output.writeInterleaved(buf, sr);
    };

    let writeTone = writeToneJs;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroAudioToneBackend = "js";

    const gen = toneGeneration;
    void wasmInitPromise
      .then(({ api }) => {
        if (toneGeneration !== gen) return;
        if (
          typeof api.attach_worklet_bridge !== "function" ||
          typeof api.SineTone !== "function" ||
          !(output.ringBuffer.buffer instanceof SharedArrayBuffer)
        ) {
          return;
        }

        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        wasmBridge = (api.attach_worklet_bridge as any)(output.ringBuffer.buffer, output.ringBuffer.capacityFrames, channelCount);
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        wasmTone = new (api.SineTone as any)() as { free(): void; write: (...args: unknown[]) => number };

        writeTone = (frames: number) => {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (wasmTone as any).write(wasmBridge, frames, freqHz, sr, gain);
        };

        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (globalThis as any).__aeroAudioToneBackend = "wasm";
      })
      .catch(() => {
        // Keep JS fallback.
      });

    const buffering = createAdaptiveRingBufferTarget(output.ringBuffer.capacityFrames, sr);

    // Prefill ~100ms (or up to capacity) to avoid startup underruns, then allow
    // the adaptive target to converge downward.
    writeTone(Math.min(output.ringBuffer.capacityFrames, Math.floor(sr / 10)));

    const timer = window.setInterval(() => {
      const underrunFrames = output.getUnderrunCount();
      const level = output.getBufferLevelFrames();
      const target = buffering.update(level, underrunFrames);
      const need = Math.max(0, target - level);
      if (need > 0) writeTone(need);

      const metrics = output.getMetrics();
      status.textContent =
        `AudioContext: ${metrics.state}\n` +
        `sampleRate: ${metrics.sampleRate}\n` +
        `capacityFrames: ${metrics.capacityFrames}\n` +
        `targetFrames: ${target}\n` +
        `bufferLevelFrames: ${metrics.bufferLevelFrames}\n` +
        `targetMs: ${((target / sr) * 1000).toFixed(1)}\n` +
        `bufferLevelMs: ${((metrics.bufferLevelFrames / sr) * 1000).toFixed(1)}\n` +
        `underrunFrames: ${metrics.underrunCount}\n` +
        `overrunFrames: ${metrics.overrunCount}`;
    }, 20);
    (timer as unknown as { unref?: () => void }).unref?.();
    toneTimer = timer as unknown as number;
  }

  const button = el("button", {
    id: "init-audio-output",
    text: "Init audio output (test tone)",
    onclick: async () => {
      status.textContent = "";
      stopLoopback();
      stopHdaDemo();
      const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
      // Expose for Playwright smoke tests.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutput = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      try {
        await startTone(output);
        await output.resume();
        if (window.aero?.perf) {
          stopPerfSampling?.();
          stopPerfSampling = startAudioPerfSampling(output, perf);
        }
      } catch (err) {
        stopTone();
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }
      status.textContent = "Audio initialized and test tone started.";
    },
  });

  const workerButton = el("button", {
    id: "init-audio-output-worker",
    text: "Init audio output (worker tone)",
    onclick: async () => {
      status.textContent = "";
      stopTone();
      stopLoopback();
      stopHdaDemo();

      try {
        // Ensure the static config (if any) has been loaded before starting the
        // worker harness. Otherwise, `AeroConfigManager.init()` may emit an update
        // after we start workers and trigger an avoidable worker restart.
        await configInitPromise;
        workerCoordinator.start(configManager.getState().effective);
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: Math.floor(48_000 / 5),
      });
      // Expose for Playwright smoke tests.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutputWorker = output;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioToneBackendWorker = "cpu-worker-wasm";
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      try {
        // Prefill the ring with silence so the CPU worker has time to attach and begin writing
        // without incurring startup underruns.
        //
        // `createAudioOutput()` already writes a small startup padding; top up to capacity without
        // dropping frames so `overrunCount` stays at 0 (useful for CI smoke tests).
        const level = output.getBufferLevelFrames();
        const prefillFrames = Math.max(0, output.ringBuffer.capacityFrames - level);
        if (prefillFrames > 0) {
          // `SharedArrayBuffer` is guaranteed to be zero-initialized, and this demo always uses a
          // freshly allocated ring buffer. Avoid allocating/copying a large Float32Array of zeros
          // by simply advancing the write index to "claim" silent frames.
          Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
        }

        workerCoordinator.setAudioOutputRingBuffer(
          output.ringBuffer.buffer,
          output.context.sampleRate,
          output.ringBuffer.channelCount,
          output.ringBuffer.capacityFrames,
        );

        await output.resume();
        if (window.aero?.perf) {
          stopPerfSampling?.();
          stopPerfSampling = startAudioPerfSampling(output, perf);
        }
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }

      const timer = window.setInterval(() => {
        const metrics = output.getMetrics();
        status.textContent =
          `AudioContext: ${metrics.state}\n` +
          `sampleRate: ${metrics.sampleRate}\n` +
          `capacityFrames: ${metrics.capacityFrames}\n` +
          `bufferLevelFrames: ${metrics.bufferLevelFrames}\n` +
          `underrunFrames: ${metrics.underrunCount}\n` +
          `overrunFrames: ${metrics.overrunCount}\n` +
          `producer.bufferLevelFrames: ${workerCoordinator.getAudioProducerBufferLevelFrames()}\n` +
          `producer.underrunFrames: ${workerCoordinator.getAudioProducerUnderrunCount()}\n` +
          `producer.overrunFrames: ${workerCoordinator.getAudioProducerOverrunCount()}`;
      }, 50);
      (timer as unknown as { unref?: () => void }).unref?.();
      toneTimer = timer as unknown as number;

      status.textContent = "Audio initialized (worker tone backend).";
    },
  });

  const hdaDemoButton = el("button", {
    id: "init-audio-hda-demo",
    text: "Init audio output (HDA demo)",
    onclick: async () => {
      status.textContent = "";
      stopTone();
      stopLoopback();
      stopHdaDemo();

      // Best-effort: ensure this WASM build includes the HDA demo wrapper.
      try {
        const { api } = await wasmInitPromise;
        if (typeof api.HdaPlaybackDemo !== "function") {
          status.textContent = "HDA demo is unavailable in this WASM build (missing HdaPlaybackDemo export).";
          return;
        }
      } catch {
        status.textContent = "HDA demo is unavailable (WASM init failed).";
        return;
      }

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        // Give the worker + WASM init a bit more slack than the default (~200ms).
        ringBufferFrames: 16_384, // ~340ms @ 48k
      });
      // Expose for Playwright smoke tests / e2e assertions.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutputHdaDemo = output;
      // Back-compat: older tests/debug helpers look for `__aeroAudioOutput`.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutput = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioToneBackend = "wasm-hda";

      // Prefill the ring with silence so the worker has time to attach and start producing audio
      // without incurring startup underruns.
      //
      // `createAudioOutput()` already writes a small startup padding; top up to capacity without
      // dropping frames so `overrunCount` stays at 0 (useful for CI smoke tests).
      const level = output.getBufferLevelFrames();
      const prefillFrames = Math.max(0, output.ringBuffer.capacityFrames - level);
      if (prefillFrames > 0) {
        // `SharedArrayBuffer` is guaranteed to be zero-initialized, and this demo always uses a
        // freshly allocated ring buffer. Avoid allocating/copying a large Float32Array of zeros
        // by simply advancing the write index to "claim" silent frames.
        Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
      }

      // Start the CPU worker in a standalone "audio demo" mode.
      hdaDemoWorker = new Worker(new URL("./workers/cpu.worker.ts", import.meta.url), { type: "module" });
      hdaDemoWorker.addEventListener("message", (ev: MessageEvent<unknown>) => {
        const msg = ev.data as { type?: unknown } | null;
        if (!msg || msg.type !== "audioOutputHdaDemo.stats") return;
        hdaDemoStats = msg as { [k: string]: unknown };
        // Expose for Playwright/debugging.
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (globalThis as any).__aeroAudioHdaDemoStats = hdaDemoStats;
      });

      const workerReady = new Promise<void>((resolve, reject) => {
        const worker = hdaDemoWorker;
        if (!worker) {
          reject(new Error("Missing HDA demo worker"));
          return;
        }

        // Loading and instantiating the HDA demo WASM module can take a while when
        // there is no cached compilation artifact yet (common in CI).
        const timeoutMs = 45_000;
        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data as { type?: unknown; message?: unknown } | null | undefined;
          if (!data || typeof data !== "object") return;
          if (data.type === "audioOutputHdaDemo.ready") {
            cleanup();
            resolve();
          } else if (data.type === "audioOutputHdaDemo.error") {
            cleanup();
            reject(new Error(typeof data.message === "string" ? data.message : "HDA demo worker error"));
          }
        };
        const onError = (ev: ErrorEvent) => {
          cleanup();
          reject(new Error(ev.message || "HDA demo worker error"));
        };

        const timer = window.setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for HDA demo worker init (${timeoutMs}ms).`));
        }, timeoutMs);
        (timer as unknown as { unref?: () => void }).unref?.();

        const cleanup = () => {
          window.clearTimeout(timer);
          worker.removeEventListener("message", onMessage);
          worker.removeEventListener("error", onError);
        };

        worker.addEventListener("message", onMessage);
        worker.addEventListener("error", onError);
      });
      hdaDemoWorker.postMessage({
        type: "audioOutputHdaDemo.start",
        ringBuffer: output.ringBuffer.buffer,
        capacityFrames: output.ringBuffer.capacityFrames,
        channelCount: output.ringBuffer.channelCount,
        sampleRate: output.context.sampleRate,
        freqHz: 440,
        gain: 0.1,
      });

      try {
        await workerReady;
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        stopHdaDemo();
        return;
      }

      await output.resume();
      status.textContent = "Audio initialized and HDA playback demo started in CPU worker.";
        const timer = window.setInterval(() => {
          const metrics = output.getMetrics();
          const read = Atomics.load(output.ringBuffer.readIndex, 0) >>> 0;
          const write = Atomics.load(output.ringBuffer.writeIndex, 0) >>> 0;
          const demoStats = hdaDemoStats;
          const demoLines: string[] = [];
          if (demoStats) {
            const t = demoStats["targetFrames"];
            const lvl = demoStats["bufferLevelFrames"];
          if (typeof t === "number") demoLines.push(`worker.targetFrames: ${t}`);
          if (typeof lvl === "number") demoLines.push(`worker.bufferLevelFrames: ${lvl}`);
          const totalWritten = demoStats["totalFramesWritten"];
          if (typeof totalWritten === "number") demoLines.push(`hda.totalFramesWritten: ${totalWritten}`);
          const totalDropped = demoStats["totalFramesDropped"];
          if (typeof totalDropped === "number") demoLines.push(`hda.totalFramesDropped: ${totalDropped}`);
        }
        status.textContent =
          `AudioContext: ${metrics.state}\n` +
          `sampleRate: ${metrics.sampleRate}\n` +
          `capacityFrames: ${metrics.capacityFrames}\n` +
          `bufferLevelFrames: ${metrics.bufferLevelFrames}\n` +
          `underrunFrames: ${metrics.underrunCount}\n` +
          `overrunFrames: ${metrics.overrunCount}\n` +
          `ring.readFrameIndex: ${read}\n` +
          `ring.writeFrameIndex: ${write}` +
          (demoLines.length ? `\n${demoLines.join("\n")}` : "");
      }, 50);
      (timer as unknown as { unref?: () => void }).unref?.();
      toneTimer = timer as unknown as number;
    },
  });

  const loopbackButton = el("button", {
    id: "init-audio-loopback-synthetic",
    text: "Init audio loopback (synthetic mic)",
    onclick: async () => {
      status.textContent = "";
      stopTone();
      stopLoopback();
      stopHdaDemo();

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: 16_384, // ~340ms @ 48k; target buffering stays ~200ms.
      });
      // Expose for Playwright.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutputLoopback = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      // Start the synthetic microphone at the *actual* AudioContext rate so we
      // don't slowly drift and underrun if the browser ignores our requested
      // 48kHz.
      const mic = startSyntheticMic({
        sampleRate: output.context.sampleRate,
        bufferMs: 250,
        freqHz: 440,
        gain: 0.1,
      });
      syntheticMic = mic;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroSyntheticMic = mic;

      // Prefill ~200ms of silence so the AudioWorklet doesn't count underruns
      // while the workers spin up.
      const sr = output.context.sampleRate;
      const targetPrefillFrames = Math.min(output.ringBuffer.capacityFrames, Math.floor(sr / 5));
      const existingLevel = output.getBufferLevelFrames();
      const prefillFrames = Math.max(0, targetPrefillFrames - existingLevel);
      if (prefillFrames > 0) {
        // `SharedArrayBuffer` is guaranteed to be zero-initialized, and this demo always uses a
        // freshly allocated ring buffer. Avoid allocating/copying a large Float32Array of zeros
        // by simply advancing the write index to "claim" silent frames.
        Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
      }

      // Default to the worker-based loopback path; fall back to a main-thread
      // pump if the worker harness cannot start (e.g. shared WebAssembly.Memory
      // unsupported).
      let backend: "worker" | "main" = "worker";
      let workerError: string | null = null;
      try {
        // Ensure the static config (if any) has been loaded before starting the
        // worker harness. Otherwise, `AeroConfigManager.init()` may emit an update
        // after we start workers and trigger an avoidable worker restart.
        await configInitPromise;
        const base = configManager.getState().effective;
        // This debug path does not need a full guest RAM allocation; keep it
        // small so Playwright runs don't reserve hundreds of MiB per page.
        workerCoordinator.start({ ...base, enableWorkers: true, guestMemoryMiB: Math.min(base.guestMemoryMiB, 64) });

        workerCoordinator.setMicrophoneRingBuffer(mic.ringBuffer, mic.sampleRate);
        workerCoordinator.setAudioOutputRingBuffer(
          output.ringBuffer.buffer,
          sr,
          output.ringBuffer.channelCount,
          output.ringBuffer.capacityFrames,
        );
      } catch (err) {
        backend = "main";
        workerError = err instanceof Error ? err.message : String(err);

        const header = new Uint32Array(mic.ringBuffer, 0, MIC_HEADER_U32_LEN);
        const capacity = Atomics.load(header, MIC_CAPACITY_SAMPLES_INDEX) >>> 0;
        const data = new Float32Array(mic.ringBuffer, MIC_HEADER_BYTES, capacity);
        const micRb: MicRingBuffer = { sab: mic.ringBuffer, header, data, capacity };

        let tmpMono = new Float32Array(256);
        let tmpInterleaved = new Float32Array(256 * output.ringBuffer.channelCount);

        const timer = window.setInterval(() => {
          const target = Math.floor(output.context.sampleRate / 5);
          const level = output.getBufferLevelFrames();
          let need = Math.max(0, target - level);
          if (need === 0) return;

          while (need > 0) {
            const chunk = Math.min(need, 256);
            if (tmpMono.length < chunk) tmpMono = new Float32Array(chunk);
            const read = micRingBufferReadInto(micRb, tmpMono.subarray(0, chunk));
            if (read === 0) break;

            const cc = output.ringBuffer.channelCount;
            const outSamples = read * cc;
            if (tmpInterleaved.length < outSamples) tmpInterleaved = new Float32Array(outSamples);
            for (let i = 0; i < read; i++) {
              const s = tmpMono[i];
              const base = i * cc;
              for (let c = 0; c < cc; c++) tmpInterleaved[base + c] = s;
            }

            const written = output.writeInterleaved(tmpInterleaved.subarray(0, outSamples), mic.sampleRate);
            if (written === 0) break;
            need -= written;
          }
        }, 25);
        (timer as unknown as { unref?: () => void }).unref?.();
        loopbackTimer = timer as unknown as number;
      }

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioLoopbackBackend = backend;

      await output.resume();
      status.textContent = workerError
        ? `Audio loopback initialized (backend=${backend}). Worker init failed: ${workerError}`
        : `Audio loopback initialized (backend=${backend}).`;
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Audio" }),
    el("div", { class: "row" }, button, workerButton, hdaDemoButton, loopbackButton),
    status,
  );
}

function renderMicrophonePanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const stateLine = el("div", { class: "mono", text: "state=inactive" });
  const statsLine = el("div", { class: "mono", text: "" });

  const deviceSelect = el("select") as HTMLSelectElement;
  const echoCancellation = el("input", { type: "checkbox", checked: "" }) as HTMLInputElement;
  const noiseSuppression = el("input", { type: "checkbox", checked: "" }) as HTMLInputElement;
  const autoGainControl = el("input", { type: "checkbox", checked: "" }) as HTMLInputElement;
  const bufferMsInput = el("input", { type: "number", value: "80", min: "10", max: "500", step: "10" }) as HTMLInputElement;
  const mutedInput = el("input", { type: "checkbox" }) as HTMLInputElement;

  let mic: MicCapture | null = null;
  let lastWorkletStats: { buffered?: number; dropped?: number } = {};

  async function refreshDevices(): Promise<void> {
    deviceSelect.replaceChildren(el("option", { value: "", text: "default" }));
    if (!navigator.mediaDevices?.enumerateDevices) return;
    const devices = await navigator.mediaDevices.enumerateDevices();
    for (const dev of devices) {
      if (dev.kind !== "audioinput") continue;
      const label = dev.label || `mic (${dev.deviceId.slice(0, 8)}…)`;
      deviceSelect.append(el("option", { value: dev.deviceId, text: label }));
    }
  }

  function attachToWorkers(): void {
    if (micAttachment) {
      workerCoordinator.setMicrophoneRingBuffer(micAttachment.ringBuffer, micAttachment.sampleRate);
    } else {
      workerCoordinator.setMicrophoneRingBuffer(null, 0);
    }
  }

  function update(): void {
    const state = mic?.state ?? "inactive";
    stateLine.textContent = `state=${state}`;

    const buffered = lastWorkletStats.buffered ?? 0;
    const dropped = lastWorkletStats.dropped ?? 0;
    statsLine.textContent =
      `bufferedSamples=${buffered} droppedSamples=${dropped} ` +
      `device=${deviceSelect.value ? deviceSelect.value.slice(0, 8) + "…" : "default"}`;
  }

  const startButton = el("button", {
    text: "Start microphone",
    onclick: async () => {
      status.textContent = "";
      lastWorkletStats = {};

      try {
        if (!navigator.mediaDevices?.getUserMedia) {
          throw new Error("getUserMedia is unavailable in this browser.");
        }
        if (typeof SharedArrayBuffer === "undefined") {
          throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
        }

        if (mic) {
          await mic.stop();
          mic = null;
        }

        mic = new MicCapture({
          sampleRate: 48_000,
          bufferMs: Math.max(10, Number(bufferMsInput.value || 0) | 0),
          preferWorklet: true,
          deviceId: deviceSelect.value || undefined,
          echoCancellation: echoCancellation.checked,
          noiseSuppression: noiseSuppression.checked,
          autoGainControl: autoGainControl.checked,
        });

        mic.addEventListener("statechange", update);
        mic.addEventListener("devicechange", () => {
          void refreshDevices();
        });
        mic.addEventListener("message", (event) => {
          const data = (event as MessageEvent).data as unknown;
          if (!data || typeof data !== "object") return;
          const msg = data as { type?: unknown; buffered?: unknown; dropped?: unknown };
          if (msg.type === "stats") {
            lastWorkletStats = {
              buffered: typeof msg.buffered === "number" ? msg.buffered : undefined,
              dropped: typeof msg.dropped === "number" ? msg.dropped : undefined,
            };
            update();
          }
        });

        await mic.start();
        mic.setMuted(mutedInput.checked);

        micAttachment = { ringBuffer: mic.ringBuffer.sab, sampleRate: mic.actualSampleRate };
        attachToWorkers();
        update();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        micAttachment = null;
        attachToWorkers();
        update();
      }
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop microphone",
    onclick: async () => {
      status.textContent = "";
      try {
        await mic?.stop();
        mic = null;
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        micAttachment = null;
        attachToWorkers();
        update();
      }
    },
  }) as HTMLButtonElement;

  mutedInput.addEventListener("change", () => {
    mic?.setMuted(mutedInput.checked);
    update();
  });

  void refreshDevices().then(update);
  navigator.mediaDevices?.addEventListener?.("devicechange", () => void refreshDevices());

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Microphone (capture)" }),
    el("div", { class: "row" }, startButton, stopButton, el("label", { text: "device:" }), deviceSelect),
    el(
      "div",
      { class: "row" },
      el("label", { text: "echoCancellation:" }),
      echoCancellation,
      el("label", { text: "noiseSuppression:" }),
      noiseSuppression,
      el("label", { text: "autoGainControl:" }),
      autoGainControl,
      el("label", { text: "bufferMs:" }),
      bufferMsInput,
      el("label", { text: "mute:" }),
      mutedInput,
    ),
    stateLine,
    statsLine,
    status,
  );
}

function renderWebHidPassthroughPanel(): HTMLElement {
  const host = el("div", { class: "panel" });
  mountWebHidPassthroughPanel(host, webHidManager);
  return host;
}

function renderMicrobenchPanel(): HTMLElement {
  const output = el("pre", { text: "" });

  const runButton = el("button", {
    text: "Run microbench suite",
    onclick: async () => {
      output.textContent = "";
      runButton.disabled = true;
      try {
        if (!window.aero?.bench?.runMicrobenchSuite) {
          output.textContent = "window.aero.bench.runMicrobenchSuite is not available.";
          return;
        }
        const results = await window.aero.bench.runMicrobenchSuite();
        output.textContent = JSON.stringify(results, null, 2);
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        runButton.disabled = false;
      }
    },
  }) as HTMLButtonElement;

  const exportButton = el("button", {
    text: "Export perf JSON",
    onclick: () => {
      output.textContent = "";
      try {
        if (!window.aero?.perf?.export) {
          output.textContent = "window.aero.perf.export is not available.";
          return;
        }
        output.textContent = JSON.stringify(window.aero.perf.export(), null, 2);
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Microbench" }),
    el(
      "div",
      { class: "row" },
      runButton,
      exportButton,
      el("span", { class: "mono", text: "Runs deterministic WASM microbench hot paths (ALU, branch, memcpy, hash)." }),
    ),
    output,
  );
}

function renderInputPanel(): HTMLElement {
  const log = el("pre", { text: "" });
  const status = el("div", { class: "mono", text: "" });
  const canvas = el("canvas", {
    width: "640",
    height: "360",
    tabindex: "0",
    style: "border: 1px solid #444; background: #111; width: 640px; height: 360px;",
  }) as HTMLCanvasElement;

  const append = (line: string): void => {
    log.textContent = `${log.textContent ?? ""}${line}\n`;
    log.scrollTop = log.scrollHeight;
  };

  const inputTarget: InputBatchTarget = {
    postMessage: (msg, transfer) => {
      const words = new Int32Array(msg.buffer);
      const count = words[0] >>> 0;
      const base = 2;
      for (let i = 0; i < count; i += 1) {
        const off = base + i * 4;
        const type = words[off] >>> 0;
        if (type === InputEventType.KeyScancode) {
          const packed = words[off + 2] >>> 0;
          const len = words[off + 3] >>> 0;
          const bytes = [];
          for (let j = 0; j < len; j += 1) bytes.push((packed >>> (j * 8)) & 0xff);
          append(`kbd: ${bytes.map((b) => b.toString(16).padStart(2, "0")).join(" ")}`);
        } else if (type === InputEventType.KeyHidUsage) {
          const packed = words[off + 2] >>> 0;
          const usage = packed & 0xff;
          const pressed = ((packed >>> 8) & 1) !== 0;
          append(`hid: usage=0x${usage.toString(16).padStart(2, "0")} ${pressed ? "down" : "up"}`);
        } else if (type === InputEventType.GamepadReport) {
          const lo = words[off + 2] | 0;
          const hi = words[off + 3] | 0;

          const { buttons, hat, x, y, rx, ry } = decodeGamepadReport(lo, hi);
          append(`pad: buttons=0x${buttons.toString(16).padStart(4, "0")} hat=${formatGamepadHat(hat)} x=${x} y=${y} rx=${rx} ry=${ry}`);
        } else if (type === InputEventType.MouseButtons) {
          append(`mouse: buttons=0x${(words[off + 2] >>> 0).toString(16)}`);
        } else if (type === InputEventType.MouseWheel) {
          append(`mouse: wheel=${words[off + 2] | 0}`);
        }
      }

      const ioWorker = workerCoordinator.getIoWorker();
      if (ioWorker) {
        ioWorker.postMessage(msg, transfer);
      }
    },
  };

  const capture = new InputCapture(canvas, inputTarget);
  capture.start();

  const hint = el("div", {
    class: "mono",
    text: "Click the canvas to focus + request pointer lock. Keyboard/mouse/gamepad events are batched and forwarded to the I/O worker.",
  });

  const clear = el("button", {
    text: "Clear log",
    onclick: () => {
      log.textContent = "";
    },
  });

  const updateStatus = (): void => {
    status.textContent =
      `pointerLock=${capture.pointerLocked ? "yes" : "no"}  ` +
      `ioWorker=${workerCoordinator.getIoWorker() ? "ready" : "stopped"}  ` +
      `ioBatches=${workerCoordinator.getIoInputBatchCounter()}  ` +
      `ioEvents=${workerCoordinator.getIoInputEventCounter()}`;
  };
  updateStatus();
  const statusTimer = globalThis.setInterval(updateStatus, 250);
  (statusTimer as unknown as { unref?: () => void }).unref?.();

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Input capture (PS/2 + USB HID gamepad reports)" }),
    hint,
    status,
    el("div", { class: "row" }, clear),
    canvas,
    log,
  );
}

function renderWebUsbBrokerPanel(): HTMLElement {
  return renderWebUsbBrokerPanelUi(usbBroker);
}

function renderWebUsbPassthroughDemoWorkerPanel(): HTMLElement {
  const status = el("pre", { class: "mono", text: "" });
  const resultLine = el("pre", { class: "mono", text: "Result: (none yet)" });
  const bytesLine = el("pre", { class: "mono", text: "(no bytes)" });
  const errorLine = el("div", { class: "bad", text: "" });

  let attachedIoWorker: Worker | null = null;
  let lastResult: UsbPassthroughDemoResult | null = null;
  let selectedInfo: { vendorId: number; productId: number; productName?: string } | null = null;
  let selectedError: string | null = null;
  let lastRequest: UsbPassthroughDemoRunMessage["request"] | null = null;
  let pending = false;
  let configTotalLenHint: number | null = null;

  const runDeviceButton = el("button", {
    text: "Run GET_DESCRIPTOR(Device)",
    onclick: () => {
      lastRequest = "deviceDescriptor";
      lastResult = null;
      pending = true;
      refreshUi();
      attachedIoWorker?.postMessage({ type: "usb.demo.run", request: "deviceDescriptor", length: 18 } satisfies UsbPassthroughDemoRunMessage);
    },
  }) as HTMLButtonElement;

  const runConfigButton = el("button", {
    text: "Run GET_DESCRIPTOR(Configuration)",
    onclick: () => {
      lastRequest = "configDescriptor";
      lastResult = null;
      pending = true;
      refreshUi();
      attachedIoWorker?.postMessage({ type: "usb.demo.run", request: "configDescriptor", length: 255 } satisfies UsbPassthroughDemoRunMessage);
    },
  }) as HTMLButtonElement;

  const runConfigFullButton = el("button", {
    text: "Run GET_DESCRIPTOR(Configuration full)",
    onclick: () => {
      const len = configTotalLenHint;
      if (len === null) return;
      lastRequest = "configDescriptor";
      lastResult = null;
      pending = true;
      refreshUi();
      attachedIoWorker?.postMessage({
        type: "usb.demo.run",
        request: "configDescriptor",
        length: len,
      } satisfies UsbPassthroughDemoRunMessage);
    },
  }) as HTMLButtonElement;
  runConfigFullButton.hidden = true;

  const refreshUi = (): void => {
    const workerReady = !!attachedIoWorker;
    const selected = !!selectedInfo;
    const controlsDisabled = !workerReady || !selected || pending;
    runDeviceButton.disabled = controlsDisabled;
    runConfigButton.disabled = controlsDisabled;
    configTotalLenHint = null;
    runConfigFullButton.hidden = true;

    const selectedLine = selectedInfo
      ? `selected=${selectedInfo.productName ?? "(unnamed)"} vid=${hex16(selectedInfo.vendorId)} pid=${hex16(selectedInfo.productId)}`
      : selectedError
        ? `selected=(none) error=${selectedError}`
        : "selected=(none)";
    const requestLine = `lastRequest=${lastRequest ?? "(none)"}`;
    const resultStatus = lastResult?.status ?? (pending ? "pending" : "(none)");
    status.textContent =
      `ioWorker=${workerReady ? "ready" : "stopped"}\n` +
      `${selectedLine}\n` +
      `${requestLine}\n` +
      `lastResult=${resultStatus}`;

    if (!lastResult) {
      if (pending) {
        resultLine.textContent = "Result: pending";
        bytesLine.textContent = "(waiting)";
        errorLine.textContent = "";
        clearButton.disabled = false;
      } else {
        resultLine.textContent = "Result: (none yet)";
        bytesLine.textContent = "(no bytes)";
        errorLine.textContent = "";
        clearButton.disabled = true;
      }
      return;
    }

    clearButton.disabled = false;
    pending = false;

    switch (lastResult.status) {
      case "success": {
        const bytes = lastResult.data;
        const isDeviceDescriptor = bytes.length >= 12 && bytes[0] === 18 && bytes[1] === 1;
        const idVendor = isDeviceDescriptor ? bytes[8]! | (bytes[9]! << 8) : null;
        const idProduct = isDeviceDescriptor ? bytes[10]! | (bytes[11]! << 8) : null;
        const isConfigDescriptor = bytes.length >= 9 && bytes[0] === 9 && bytes[1] === 2;
        const totalLen = isConfigDescriptor ? bytes[2]! | (bytes[3]! << 8) : null;
        const numInterfaces = isConfigDescriptor ? bytes[4]! : null;

        if (idVendor !== null && idProduct !== null) {
          resultLine.textContent = `Result: success (device vid=${hex16(idVendor)} pid=${hex16(idProduct)})`;
        } else if (totalLen !== null && numInterfaces !== null) {
          const truncated = totalLen > bytes.byteLength;
          if (truncated) {
            configTotalLenHint = totalLen;
            runConfigFullButton.hidden = false;
            runConfigFullButton.disabled = controlsDisabled;
            runConfigFullButton.textContent = `Run GET_DESCRIPTOR(Configuration full, len=${totalLen})`;
          }
          resultLine.textContent = `Result: success (config totalLen=${totalLen} interfaces=${numInterfaces}${truncated ? ` truncated=${bytes.byteLength}` : ""})`;
        } else {
          resultLine.textContent = `Result: success (bytes=${bytes.byteLength})`;
        }
        bytesLine.textContent = formatHexBytes(bytes);
        errorLine.textContent = "";
        return;
      }
      case "stall":
        resultLine.textContent = `Result: stall${lastRequest ? ` (${lastRequest})` : ""}`;
        bytesLine.textContent = "(no bytes)";
        errorLine.textContent = "";
        return;
      case "error":
        resultLine.textContent = `Result: error${lastRequest ? ` (${lastRequest})` : ""}`;
        bytesLine.textContent = "(no bytes)";
        errorLine.textContent = lastResult.message;
        return;
      default: {
        const neverResult: never = lastResult;
        throw new Error(`Unknown demo result status: ${String((neverResult as { status?: unknown }).status)}`);
      }
    }
  };

  const onMessage = (ev: MessageEvent<unknown>): void => {
    if (!isUsbPassthroughDemoResultMessage(ev.data)) return;
    lastResult = ev.data.result;
    pending = false;
    refreshUi();
  };

  if (typeof MessageChannel !== "undefined") {
    // Listen for `usb.selected` broadcasts so the demo panel can disable Run buttons when no
    // device is selected and clear stale results when the selected device changes.
    const channel = new MessageChannel();
    usbBroker.attachWorkerPort(channel.port1, { attachRings: false });
    channel.port2.addEventListener("message", (ev: MessageEvent<unknown>) => {
      if (!isUsbSelectedMessage(ev.data)) return;
      const msg = ev.data;
      if (msg.ok) {
        const next = msg.info;
        if (!selectedInfo || selectedInfo.vendorId !== next.vendorId || selectedInfo.productId !== next.productId) {
          lastResult = null;
          lastRequest = "deviceDescriptor";
          pending = false;
        }
        selectedInfo = next;
        selectedError = null;
      } else {
        selectedInfo = null;
        selectedError = msg.error ?? null;
        lastResult = null;
        lastRequest = null;
        pending = false;
      }
      refreshUi();
    });
    channel.port2.start();
    // Unit tests run in the node environment; unref to avoid leaking handles.
    try {
      (channel.port1 as unknown as { unref?: () => void }).unref?.();
      (channel.port2 as unknown as { unref?: () => void }).unref?.();
    } catch {
      // ignore
    }
  }

  const ensureAttached = (): void => {
    const ioWorker = workerCoordinator.getIoWorker();
    if (ioWorker === attachedIoWorker) return;

    if (attachedIoWorker) {
      attachedIoWorker.removeEventListener("message", onMessage);
    }
    attachedIoWorker = ioWorker;
    lastResult = null;
    pending = false;
    if (attachedIoWorker && selectedInfo) lastRequest = "deviceDescriptor";
    if (attachedIoWorker) {
      attachedIoWorker.addEventListener("message", onMessage);
    } else {
      pending = false;
    }
    refreshUi();
  };

  const clearButton = el("button", {
    text: "Clear",
    onclick: () => {
      lastResult = null;
      pending = false;
      refreshUi();
    },
  }) as HTMLButtonElement;

  ensureAttached();
  const attachTimer = globalThis.setInterval(ensureAttached, 250);
  (attachTimer as unknown as { unref?: () => void }).unref?.();
  refreshUi();

  const hint = el("div", {
    class: "hint",
    text:
      "Demo: select a WebUSB device via the broker panel. The I/O worker runs a WASM-side UsbPassthroughDemo which queues GET_DESCRIPTOR actions (auto-run on selection; rerun via buttons) and reports the result back as usb.demoResult. For configuration descriptors, the panel can rerun with wTotalLength when the initial read is truncated.",
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "WebUSB passthrough demo (IO worker + UsbBroker)" }),
    hint,
    status,
    el("div", { class: "row" }, runDeviceButton, runConfigButton, runConfigFullButton, clearButton),
    resultLine,
    bytesLine,
    errorLine,
  );
}

function renderWebUsbUhciHarnessWorkerPanel(): HTMLElement {
  const status = el("pre", { class: "mono", text: "" });
  const deviceDesc = el("pre", { class: "mono", text: "(none yet)" });
  const configDesc = el("pre", { class: "mono", text: "(none yet)" });

  const lastActionLine = el("pre", { class: "mono", text: "Last action: (none)" });
  const lastCompletionLine = el("pre", { class: "mono", text: "Last completion: (none)" });
  const errorLine = el("div", { class: "bad", text: "" });

  function describeAction(action: UsbHostAction): string {
    switch (action.kind) {
      case "controlIn":
        return `controlIn id=${action.id} bmRequestType=${hex8(action.setup.bmRequestType)} bRequest=${hex8(action.setup.bRequest)} wValue=${hex16(
          action.setup.wValue,
        )} wIndex=${hex16(action.setup.wIndex)} wLength=${action.setup.wLength}`;
      case "controlOut":
        return `controlOut id=${action.id} bytes=${action.data.byteLength}`;
      case "bulkIn":
        return `bulkIn id=${action.id} ep=${hex8(action.endpoint)} len=${action.length}`;
      case "bulkOut":
        return `bulkOut id=${action.id} ep=${hex8(action.endpoint)} bytes=${action.data.byteLength}`;
      default: {
        const neverAction: never = action;
        return `unknown action ${(neverAction as unknown as { kind?: unknown }).kind ?? "?"}`;
      }
    }
  }

  function describeCompletion(completion: UsbHostCompletion): string {
    if (completion.status === "stall") return `${completion.kind} id=${completion.id} status=stall`;
    if (completion.status === "error") return `${completion.kind} id=${completion.id} status=error message=${completion.message}`;
    if (completion.kind === "controlIn" || completion.kind === "bulkIn") {
      return `${completion.kind} id=${completion.id} status=success bytes=${completion.data.byteLength}`;
    }
    return `${completion.kind} id=${completion.id} status=success bytesWritten=${completion.bytesWritten}`;
  }

  let attachedIoWorker: Worker | null = null;
  let snapshot: WebUsbUhciHarnessRuntimeSnapshot | null = null;

  const refreshUi = (): void => {
    const workerReady = !!attachedIoWorker;
    const enabled = snapshot?.enabled ?? false;
    const blocked = snapshot?.blocked ?? true;
    const available = snapshot?.available ?? false;

    status.textContent =
      `ioWorker=${workerReady ? "ready" : "stopped"}\n` +
      `harnessAvailable=${available}\n` +
      `status=${enabled ? "running" : "stopped"}\n` +
      `blocked=${blocked}\n` +
      `ticks=${snapshot?.tickCount ?? 0}\n` +
      `actions=${snapshot?.actionsForwarded ?? 0}\n` +
      `completions=${snapshot?.completionsApplied ?? 0}\n` +
      `pending=${snapshot?.pendingCompletions ?? 0}\n`;

    if (snapshot?.lastAction) {
      lastActionLine.textContent = `Last action: ${describeAction(snapshot.lastAction)}`;
    } else {
      lastActionLine.textContent = "Last action: (none)";
    }

    if (snapshot?.lastCompletion) {
      lastCompletionLine.textContent = `Last completion: ${describeCompletion(snapshot.lastCompletion)}`;
    } else {
      lastCompletionLine.textContent = "Last completion: (none)";
    }

    deviceDesc.textContent = snapshot?.deviceDescriptor ? formatHexBytes(snapshot.deviceDescriptor) : "(none yet)";
    configDesc.textContent = snapshot?.configDescriptor ? formatHexBytes(snapshot.configDescriptor) : "(none yet)";

    errorLine.textContent = snapshot?.lastError ? snapshot.lastError : "";

    startButton.disabled = !workerReady || enabled;
    stopButton.disabled = !workerReady || (!enabled && !snapshot);
  };

  const startButton = el("button", {
    text: "Start harness (IO worker)",
    onclick: () => {
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.harness.start" });
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop/Reset",
    onclick: () => {
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.harness.stop" });
    },
  }) as HTMLButtonElement;

  const onMessage = (ev: MessageEvent<unknown>): void => {
    if (!isUsbUhciHarnessStatusMessage(ev.data)) return;
    snapshot = ev.data.snapshot;
    refreshUi();
  };

  const ensureAttached = (): void => {
    const ioWorker = workerCoordinator.getIoWorker();
    if (ioWorker === attachedIoWorker) return;

    if (attachedIoWorker) {
      attachedIoWorker.removeEventListener("message", onMessage);
    }
    attachedIoWorker = ioWorker;
    snapshot = null;
    if (attachedIoWorker) {
      attachedIoWorker.addEventListener("message", onMessage);
    }
    refreshUi();
  };

  ensureAttached();
  const attachTimer = globalThis.setInterval(ensureAttached, 250);
  (attachTimer as unknown as { unref?: () => void }).unref?.();

  refreshUi();

  const hint = el("div", {
    class: "hint",
    text:
      "Dev-only smoke test: start workers, select a WebUSB device via the broker panel, then start the UHCI harness. " +
      "The harness runs in the I/O worker, emits usb.action messages, and receives usb.completion replies from the main thread UsbBroker.",
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "UHCI passthrough harness (IO worker + UsbBroker)" }),
    hint,
    el("div", { class: "row" }, startButton, stopButton),
    status,
    lastActionLine,
    lastCompletionLine,
    el("div", { class: "mono", text: "Device descriptor (latest):" }),
    deviceDesc,
    el("div", { class: "mono", text: "Configuration descriptor (latest):" }),
    configDesc,
    errorLine,
  );
}

function renderWorkersPanel(report: PlatformFeatureReport): HTMLElement {
  const support = workerCoordinator.checkSupport();

  const statusList = el("ul");
  const vmStateLine = el("div", { class: "mono", text: "" });
  const heartbeatLine = el("div", { class: "mono", text: "" });
  const diskLine = el("div", { class: "mono", text: "" });
  const frameLine = el("div", { class: "mono", text: "" });
  const sharedFramebufferLine = el("div", { class: "mono", text: "" });
  const gpuMetricsLine = el("div", { class: "mono", text: "" });
  const error = el("pre", { text: "" });
  const guestRamValue = el("span", { class: "mono", text: "" });

  const VM_SNAPSHOT_PATH = "state/worker-vm-autosave.snap";
  const snapshotLine = el("div", { class: "mono", text: "" });
  let snapshotInFlight = false;
  const snapshotSaveButton = el("button", {
    text: "Save snapshot",
    onclick: async () => {
      error.textContent = "";
      snapshotInFlight = true;
      update();
      snapshotLine.textContent = `snapshot: saving → ${VM_SNAPSHOT_PATH}…`;
      try {
        await workerCoordinator.snapshotSaveToOpfs(VM_SNAPSHOT_PATH);
        snapshotLine.textContent = `snapshot: saved → ${VM_SNAPSHOT_PATH}`;
      } catch (err) {
        snapshotLine.textContent = "snapshot: save failed";
        error.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        snapshotInFlight = false;
        update();
      }
    },
  }) as HTMLButtonElement;
  const snapshotLoadButton = el("button", {
    text: "Load snapshot",
    onclick: async () => {
      error.textContent = "";
      snapshotInFlight = true;
      update();
      snapshotLine.textContent = `snapshot: restoring ← ${VM_SNAPSHOT_PATH}…`;
      try {
        await workerCoordinator.snapshotRestoreFromOpfs(VM_SNAPSHOT_PATH);
        snapshotLine.textContent = `snapshot: restored ← ${VM_SNAPSHOT_PATH}`;
      } catch (err) {
        snapshotLine.textContent = "snapshot: restore failed";
        error.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        snapshotInFlight = false;
        update();
      }
    },
  }) as HTMLButtonElement;

  const jitDemoLine = el("div", { class: "mono", text: "jit: (idle)" });
  const jitDemoError = el("pre", { text: "" });

  const forceJitCspBlock = el("input", { type: "checkbox" }) as HTMLInputElement;
  const forceJitCspLabel = el("label", { class: "mono", text: "force jit_dynamic_wasm=false" });

  const JIT_DEMO_WASM_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
  let jitDemoInFlight = false;
  let jitClient: JitWorkerClient | null = null;
  let jitClientWorker: Worker | null = null;

  async function runJitCompileDemo(): Promise<void> {
    const jitWorker = workerCoordinator.getWorker("jit");
    if (!jitWorker) {
      jitDemoError.textContent = "JIT worker is not running.";
      return;
    }

    if (!jitClient || jitClientWorker !== jitWorker) {
      jitClient?.destroy();
      jitClient = new JitWorkerClient(jitWorker);
      jitClientWorker = jitWorker;
    }

    jitDemoError.textContent = "";
    jitDemoLine.textContent = "jit: compiling…";
    jitDemoInFlight = true;
    update();

    const wasmBytes = JIT_DEMO_WASM_BYTES.slice().buffer;

    let response: JitWorkerResponse;
    try {
      response = await jitClient.compile(wasmBytes, { timeoutMs: 5000 });
    } catch (err) {
      jitDemoError.textContent = err instanceof Error ? err.message : String(err);
      jitDemoLine.textContent = "jit: demo failed";
      return;
    } finally {
      jitDemoInFlight = false;
      update();
    }

    // Expose for Playwright / devtools.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroJitDemo = response;

    if (response.type === "jit:error") {
      jitDemoLine.textContent = `jit: error (${response.code ?? "unknown"}) in ${response.durationMs ?? 0}ms`;
      jitDemoError.textContent = response.message;
      return;
    }

    // Verify that the module is usable in this realm (compilation happens in the JIT worker).
    try {
      if (!(response.module instanceof WebAssembly.Module)) {
        throw new Error("Response module is not a WebAssembly.Module.");
      }
      // Instantiation is cheap for the empty module, but keep it async.
      await WebAssembly.instantiate(response.module, {});
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      jitDemoLine.textContent = "jit: compiled, but validation failed";
      jitDemoError.textContent = message;
      return;
    }

    const cached = response.cached ? " (cached)" : "";
    jitDemoLine.textContent = `jit: compiled demo module in ${response.durationMs.toFixed(2)}ms${cached}`;
  }

  const jitDemoButton = el("button", {
    text: "Test JIT compile",
    onclick: () => {
      void runJitCompileDemo();
    },
  }) as HTMLButtonElement;

  let booting = false;

  const startButton = el("button", {
    text: "Start workers",
    onclick: async () => {
      error.textContent = "";
      booting = true;
      update();
      const config = configManager.getState().effective;
      try {
        const platformFeatures = forceJitCspBlock.checked ? { ...report, jit_dynamic_wasm: false } : report;
        const diskManager = await diskManagerPromise;
        const selection = await getBootDiskSelection(diskManager);
        bootDiskSelection = selection;

        workerCoordinator.start(config, { platformFeatures });
        const ioWorker = workerCoordinator.getIoWorker();
        if (ioWorker) {
          usbBroker.attachWorkerPort(ioWorker);
          wireIoWorkerForWebHid(ioWorker, webHidManager);
          void webHidManager.resyncAttachedDevices();
          syncWebHidInputReportRing(ioWorker);
          attachedIoWorker = ioWorker;
          ioWorker.postMessage({
            type: "setBootDisks",
            mounts: selection.mounts,
            hdd: selection.hdd ?? null,
            cd: selection.cd ?? null,
          });
        }
        const gpuWorker = workerCoordinator.getWorker("gpu");
        const frameStateSab = workerCoordinator.getFrameStateSab();
        const sharedFramebuffer = workerCoordinator.getSharedFramebuffer();
        const scanoutStateInfo = workerCoordinator.getScanoutState();
        if (gpuWorker && frameStateSab && sharedFramebuffer) {
          // Reset any previously transferred canvas before re-attaching it to a
          // new worker.
          if (canvasTransferred) resetVgaCanvas();

          let offscreen: OffscreenCanvas | undefined;
          useWorkerPresentation = false;
          if (
            report.offscreenCanvas &&
            "transferControlToOffscreen" in vgaCanvas &&
            typeof (vgaCanvas as unknown as { transferControlToOffscreen?: unknown }).transferControlToOffscreen ===
              "function"
          ) {
            try {
              offscreen = (vgaCanvas as unknown as HTMLCanvasElement & { transferControlToOffscreen: () => OffscreenCanvas })
                .transferControlToOffscreen();
              canvasTransferred = true;
              useWorkerPresentation = true;
            } catch {
              // Ignore and fall back to main-thread presentation.
              offscreen = undefined;
              canvasTransferred = false;
              useWorkerPresentation = false;
            }
          }

          frameScheduler?.stop();
          frameScheduler = startFrameScheduler({
            gpuWorker,
            sharedFrameState: frameStateSab,
            sharedFramebuffer: sharedFramebuffer.sab,
            sharedFramebufferOffsetBytes: sharedFramebuffer.offsetBytes,
            ...(scanoutStateInfo
              ? { scanoutState: scanoutStateInfo.sab, scanoutStateOffsetBytes: scanoutStateInfo.offsetBytes }
              : {}),
            canvas: offscreen,
            initOptions: offscreen
              ? {
                  forceBackend: "webgl2_raw",
                  disableWebGpu: true,
                  outputWidth: 640,
                  outputHeight: 480,
                  dpr: window.devicePixelRatio || 1,
                }
              : undefined,
            showDebugOverlay: true,
          });
          schedulerWorker = gpuWorker;
          schedulerFrameStateSab = frameStateSab;
          schedulerSharedFramebuffer = sharedFramebuffer;
        }
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
        bootDiskSelection = null;
      } finally {
        booting = false;
      }
      update();
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop workers",
    onclick: async () => {
      jitClient?.destroy();
      jitClient = null;
      jitClientWorker = null;
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      workerCoordinator.stop();
      bootDiskSelection = null;
      useWorkerPresentation = false;
      teardownVgaPresenter();
      if (canvasTransferred) resetVgaCanvas();
      update();
    },
  }) as HTMLButtonElement;

  const restartButton = el("button", {
    text: "Restart VM",
    onclick: async () => {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      try {
        // Keep restart behavior consistent with the initial start button: use the
        // latest disk mounts from DiskManager, even if the user changed them since
        // the last boot.
        const diskManager = await diskManagerPromise;
        bootDiskSelection = await getBootDiskSelection(diskManager);
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
        bootDiskSelection = null;
      }
      try {
        workerCoordinator.restart();
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const resetButton = el("button", {
    text: "Reset VM",
    onclick: () => {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      workerCoordinator.reset("ui");
      update();
    },
  }) as HTMLButtonElement;

  const powerOffButton = el("button", {
    text: "Power off",
    onclick: () => {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      workerCoordinator.powerOff();
      update();
    },
  }) as HTMLButtonElement;

  const hint = el("div", {
    class: "mono",
    text: support.ok
      ? "Runs 4 module workers (cpu/gpu/io/jit). CPU increments a shared WebAssembly.Memory counter + emits ring-buffer heartbeats."
      : support.reason ?? "SharedArrayBuffer unavailable.",
  });

  const createVgaCanvas = (): HTMLCanvasElement => {
    const canvas = el("canvas") as HTMLCanvasElement;
    canvas.style.width = "640px";
    canvas.style.height = "480px";
    canvas.style.border = "1px solid #333";
    canvas.style.background = "#000";
    canvas.style.imageRendering = "pixelated";
    return canvas;
  };

  let vgaCanvas = createVgaCanvas();
  const vgaCanvasRow = el("div", { class: "row" }, vgaCanvas);
  let canvasTransferred = false;
  let useWorkerPresentation = false;

  function resetVgaCanvas(): void {
    // `transferControlToOffscreen()` is one-shot per HTMLCanvasElement. When the
    // worker presentation path is used, recreate the canvas so stop/start cycles
    // continue to work.
    vgaCanvas = createVgaCanvas();
    vgaCanvasRow.replaceChildren(vgaCanvas);
    canvasTransferred = false;
  }

  const vgaInfoLine = el("div", { class: "mono", text: "" });

  let vgaPresenter: VgaPresenter | null = null;
  let vgaShared: ReturnType<typeof wrapSharedFramebuffer> | null = null;
  let vgaSab: SharedArrayBuffer | null = null;
  let schedulerWorker: Worker | null = null;
  let schedulerFrameStateSab: SharedArrayBuffer | null = null;
  let schedulerSharedFramebuffer: { sab: SharedArrayBuffer; offsetBytes: number } | null = null;
  let attachedIoWorker: Worker | null = null;

  workerCoordinator.addEventListener("fatal", (ev) => {
    const detail = ev.detail;
    frameScheduler?.stop();
    frameScheduler = null;
    schedulerWorker = null;
    schedulerFrameStateSab = null;
    schedulerSharedFramebuffer = null;
    error.textContent = JSON.stringify(detail, null, 2);
  });

  workerCoordinator.addEventListener("nonfatal", (ev) => {
    const detail = ev.detail;
    if (detail.role === "gpu" && (detail.kind === "gpu_device_lost" || detail.kind === "gpu_error")) {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
    }
    // Surface nonfatal events for debugging (without clobbering an existing fatal error).
    if (!error.textContent) {
      error.textContent = JSON.stringify(detail, null, 2);
    }
  });

  function ensureVgaPresenter(): void {
    const sab = workerCoordinator.getVgaFramebuffer();
    if (!sab) return;

    if (sab !== vgaSab) {
      vgaSab = sab;
      vgaShared = wrapSharedFramebuffer(sab, 0);
      if (vgaPresenter) {
        vgaPresenter.setSharedFramebuffer(vgaShared);
      }
    }

    if (useWorkerPresentation) {
      // Worker owns the canvas; main-thread presenter must be disabled.
      if (vgaPresenter) {
        vgaPresenter.destroy();
        vgaPresenter = null;
      }
      return;
    }

    if (!vgaPresenter && vgaShared) {
      vgaPresenter = new VgaPresenter(vgaCanvas, { scaleMode: "auto", integerScaling: true, maxPresentHz: 60 });
      vgaPresenter.setSharedFramebuffer(vgaShared);
      vgaPresenter.start();
    }
  }

  function teardownVgaPresenter(): void {
    if (vgaPresenter) {
      vgaPresenter.destroy();
      vgaPresenter = null;
    }
    vgaShared = null;
    vgaSab = null;
    vgaInfoLine.textContent = "";
  }

  function update(): void {
    const statuses = workerCoordinator.getWorkerStatuses();
    const anyActive = Object.values(statuses).some((s) => s.state !== "stopped");
    const config = configManager.getState().effective;

    startButton.disabled = booting || !support.ok || !report.wasmThreads || !config.enableWorkers || anyActive;
    stopButton.disabled = !anyActive;
    restartButton.disabled = !support.ok || !report.wasmThreads || !config.enableWorkers;
    resetButton.disabled = !anyActive;
    powerOffButton.disabled = !anyActive;

    const snapshotSupported = report.opfsSyncAccessHandle;
    // Worker VM snapshots pause CPU → IO → NET and reset shared NET rings. Require all three
    // workers to be READY before enabling snapshot controls so we don't deadlock waiting for a
    // still-starting NET worker to respond to snapshot RPCs.
    const snapshotReady = statuses.cpu.state === "ready" && statuses.io.state === "ready" && statuses.net.state === "ready";
    snapshotSaveButton.disabled = !snapshotSupported || snapshotInFlight || !snapshotReady;
    snapshotLoadButton.disabled = !snapshotSupported || snapshotInFlight || !snapshotReady;
    if (!snapshotSupported) {
      snapshotLine.textContent = "snapshot: unavailable (OPFS sync access handles unsupported)";
    } else if (!snapshotInFlight && !snapshotLine.textContent) {
      snapshotLine.textContent = `snapshot: path=${VM_SNAPSHOT_PATH}`;
    }

    const vmState = workerCoordinator.getVmState();
    const pendingRestart = workerCoordinator.getPendingFullRestart();
    vmStateLine.textContent = pendingRestart
      ? `vmState=${vmState} (restart in ${Math.max(0, Math.round(pendingRestart.atMs - performance.now()))}ms)`
      : `vmState=${vmState}`;
    jitDemoButton.disabled = statuses.jit.state !== "ready" || jitDemoInFlight;
    forceJitCspBlock.disabled = anyActive;

    const ioWorker = workerCoordinator.getIoWorker();
    if (ioWorker !== attachedIoWorker) {
      if (attachedIoWorker) {
        usbBroker.detachWorkerPort(attachedIoWorker);
      }
      if (ioWorker) {
        usbBroker.attachWorkerPort(ioWorker);
        wireIoWorkerForWebHid(ioWorker, webHidManager);
        // io.worker waits for the first `setBootDisks` message before reporting READY.
        // Ensure we always send *something* so non-VM worker harnesses (audio demos, etc)
        // don't wedge the io worker in "starting" forever.
        const selection = bootDiskSelection;
        ioWorker.postMessage({
          type: "setBootDisks",
          mounts: selection?.mounts ?? {},
          hdd: selection?.hdd ?? null,
          cd: selection?.cd ?? null,
        });
        void webHidManager.resyncAttachedDevices();
      }
      syncWebHidInputReportRing(ioWorker);
      attachedIoWorker = ioWorker;
    }

    statusList.replaceChildren(
      ...Object.entries(statuses).map(([role, status]) => {
        const roleName = role as WorkerRole;
        const wasm = workerCoordinator.getWorkerWasmStatus(roleName);
        const wasmSuffix =
          roleName === "cpu" && status.state !== "stopped"
            ? wasm
              ? ` wasm(${wasm.variant}) add(20,22)=${wasm.value}`
              : " wasm(pending)"
            : "";
        return el("li", {
          text: `${roleName}: ${status.state}${status.error ? ` (${status.error})` : ""}${wasmSuffix}`,
        });
      }),
    );

    heartbeatLine.textContent =
      `config[v${workerCoordinator.getConfigVersion()}]  ` +
      `status[HeartbeatCounter]=${workerCoordinator.getHeartbeatCounter()}  ` +
      `ring[Heartbeat]=${workerCoordinator.getLastHeartbeatFromRing()}  ` +
      `guestI32[0]=${workerCoordinator.getGuestCounter0()}`;

    if (!bootDiskSelection) {
      diskLine.textContent = "disks: (not configured)";
    } else {
      const parts: string[] = [];
      if (bootDiskSelection.hdd) {
        parts.push(`hdd=${bootDiskSelection.hdd.name} (${formatBytes(bootDiskSelection.hdd.sizeBytes)})`);
      } else if (bootDiskSelection.mounts.hddId) {
        parts.push(`hdd=${bootDiskSelection.mounts.hddId} (missing)`);
      }
      if (bootDiskSelection.cd) {
        parts.push(`cd=${bootDiskSelection.cd.name} (${formatBytes(bootDiskSelection.cd.sizeBytes)})`);
      } else if (bootDiskSelection.mounts.cdId) {
        parts.push(`cd=${bootDiskSelection.mounts.cdId} (missing)`);
      }
      diskLine.textContent = parts.length ? `disks: ${parts.join(" ")}` : "disks: (no mounts)";
    }

    const frameStateSab = workerCoordinator.getFrameStateSab();
    if (!frameStateSab) {
      frameLine.textContent = "frame: (uninitialized)";
    } else {
      const frameState = new Int32Array(frameStateSab);
      frameLine.textContent = `frame: status=${Atomics.load(frameState, FRAME_STATUS_INDEX)} seq=${Atomics.load(frameState, FRAME_SEQ_INDEX)}`;
    }

    const sharedFramebufferInfo = workerCoordinator.getSharedFramebuffer();
    if (!sharedFramebufferInfo) {
      sharedFramebufferLine.textContent = "shared framebuffer: (uninitialized)";
    } else {
      const header = new Int32Array(
        sharedFramebufferInfo.sab,
        sharedFramebufferInfo.offsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const seq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
      const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
      sharedFramebufferLine.textContent = `shared framebuffer: seq=${seq} active=${active}`;
    }

    const scanoutStateInfo = workerCoordinator.getScanoutState();

    if (!frameScheduler) {
      gpuMetricsLine.textContent = "gpu metrics: (uninitialized)";
    } else {
      const metrics = frameScheduler.getMetrics();
      gpuMetricsLine.textContent =
        `gpu metrics: received=${metrics.framesReceived} presented=${metrics.framesPresented} dropped=${metrics.framesDropped}`;
    }
    guestRamValue.textContent =
      config.guestMemoryMiB % 1024 === 0 ? `${config.guestMemoryMiB / 1024} GiB` : `${config.guestMemoryMiB} MiB`;

    // Keep the GPU frame scheduler attached to the current GPU worker instance.
    // This runs before `ensureVgaPresenter()` so we don't accidentally create a
    // main-thread context right before attempting `transferControlToOffscreen()`.
    const gpuWorker = workerCoordinator.getWorker("gpu");
    if (!gpuWorker || !frameStateSab || !sharedFramebufferInfo) {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
    } else if (
      schedulerWorker !== gpuWorker ||
      schedulerFrameStateSab !== frameStateSab ||
      schedulerSharedFramebuffer?.sab !== sharedFramebufferInfo.sab ||
      schedulerSharedFramebuffer?.offsetBytes !== sharedFramebufferInfo.offsetBytes
    ) {
      let offscreen: OffscreenCanvas | undefined;
      if (useWorkerPresentation) {
        // Always recreate the canvas before transferring to a new worker since
        // `transferControlToOffscreen()` is one-shot per HTMLCanvasElement.
        if (canvasTransferred) resetVgaCanvas();

        if (
          report.offscreenCanvas &&
          "transferControlToOffscreen" in vgaCanvas &&
          typeof (vgaCanvas as unknown as { transferControlToOffscreen?: unknown }).transferControlToOffscreen === "function"
        ) {
          try {
            offscreen = (vgaCanvas as unknown as HTMLCanvasElement & { transferControlToOffscreen: () => OffscreenCanvas }).transferControlToOffscreen();
            canvasTransferred = true;
          } catch {
            offscreen = undefined;
            canvasTransferred = false;
            useWorkerPresentation = false;
          }
        } else {
          canvasTransferred = false;
          useWorkerPresentation = false;
        }
      } else if (canvasTransferred) {
        // If we somehow ended up in main-thread presentation mode with a transferred
        // canvas, recover by recreating it.
        teardownVgaPresenter();
        resetVgaCanvas();
      }

      frameScheduler?.stop();
      frameScheduler = startFrameScheduler({
        gpuWorker,
        sharedFrameState: frameStateSab,
        sharedFramebuffer: sharedFramebufferInfo.sab,
        sharedFramebufferOffsetBytes: sharedFramebufferInfo.offsetBytes,
        ...(scanoutStateInfo
          ? { scanoutState: scanoutStateInfo.sab, scanoutStateOffsetBytes: scanoutStateInfo.offsetBytes }
          : {}),
        canvas: offscreen,
        initOptions: offscreen
          ? {
              forceBackend: "webgl2_raw",
              disableWebGpu: true,
              outputWidth: 640,
              outputHeight: 480,
              dpr: window.devicePixelRatio || 1,
            }
          : undefined,
        showDebugOverlay: true,
      });
      schedulerWorker = gpuWorker;
      schedulerFrameStateSab = frameStateSab;
      schedulerSharedFramebuffer = sharedFramebufferInfo;
    }

    if (anyActive) {
      ensureVgaPresenter();
      if (vgaShared) {
        const w = Atomics.load(vgaShared.header, HEADER_INDEX_WIDTH);
        const h = Atomics.load(vgaShared.header, HEADER_INDEX_HEIGHT);
        const frame = Atomics.load(vgaShared.header, HEADER_INDEX_FRAME_COUNTER);
        vgaInfoLine.textContent = `vga ${w}x${h} frame=${frame}`;
      }
    } else {
      teardownVgaPresenter();
    }

    const lastFatal = workerCoordinator.getLastFatalEvent();
    if (vmState === "running") {
      // Clear stale error output after a successful restart.
      if (error.textContent) error.textContent = "";
    } else if (lastFatal) {
      error.textContent = JSON.stringify(lastFatal, null, 2);
    }
  }

  update();
  const updateTimer = globalThis.setInterval(update, 250);
  (updateTimer as unknown as { unref?: () => void }).unref?.();

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Workers" }),
    hint,
    el(
      "div",
      { class: "row" },
      el("label", { text: "Guest RAM:" }),
      guestRamValue,
      startButton,
      stopButton,
      restartButton,
      resetButton,
      powerOffButton,
      jitDemoButton,
      forceJitCspBlock,
      forceJitCspLabel,
    ),
    el("div", { class: "row" }, snapshotSaveButton, snapshotLoadButton),
    snapshotLine,
    vgaCanvasRow,
    vgaInfoLine,
    vmStateLine,
    heartbeatLine,
    diskLine,
    frameLine,
    sharedFramebufferLine,
    gpuMetricsLine,
    jitDemoLine,
    jitDemoError,
    statusList,
    error,
  );
}

function renderIpcDemoPanel(): HTMLElement {
  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "IPC demo (SharedArrayBuffer ring buffers)" }),
    el(
      "p",
      {},
      "Open the high-rate command/event ring-buffer demo: ",
      el("a", { href: "./demo/ipc_demo.html" }, "./demo/ipc_demo.html"),
      ".",
    ),
  );
}

render();

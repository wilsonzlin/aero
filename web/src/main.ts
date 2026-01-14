import "./style.css";

import { installAeroGlobals } from "./aero";
import { startFrameScheduler, type FrameSchedulerHandle } from "./main/frameScheduler";
import { GpuRuntime } from "./gpu/gpuRuntime";
import { fnv1a32Hex } from "./utils/fnv1a";
import { createTarArchive } from "./utils/tar";
import { encodeWavPcm16 } from "./utils/wav";
import { encodeLinearRgba8ToSrgbInPlace } from "./utils/srgb";
import { perf } from "./perf/perf";
import { createAdaptiveRingBufferTarget, createAudioOutput, startAudioPerfSampling, type AudioOutputMetrics } from "./platform/audio";
import { MicCapture, micRingBufferReadInto, type MicRingBuffer } from "./audio/mic_capture";
import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX as MIC_DROPPED_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  READ_POS_INDEX as MIC_READ_POS_INDEX,
  WRITE_POS_INDEX as MIC_WRITE_POS_INDEX,
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
import { negateI32Saturating } from "./input/int32";
import { inputRecordReplay, installInputRecordReplayGlobalApi } from "./input/record_replay";
import { decodeGamepadReport, formatGamepadHat } from "./input/gamepad";
import { installPerfHud } from "./perf/hud_entry";
import {
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  addHeaderI32,
  copyFrameFromMessageV1,
  initFramebufferHeader,
  isFramebufferCopyMessageV1,
  requiredFramebufferBytes,
  storeHeaderI32,
  wrapSharedFramebuffer,
} from "./display/framebuffer_protocol";
import { VgaPresenter } from "./display/vga_presenter";
import { SharedLayoutPresenter } from "./display/shared_layout_presenter";
import { installAeroGlobal } from "./runtime/aero_global";
import { createWebGpuCanvasContext, requestWebGpuDevice } from "./platform/webgpu";
import { WorkerCoordinator } from "./runtime/coordinator";
import { installNetTraceBackendOnAeroGlobal } from "./net/trace_backend";
import { installIoInputTelemetryBackendOnAeroGlobal } from "./runtime/io_input_telemetry_backend";
import { installBootDeviceBackendOnAeroGlobal } from "./runtime/boot_device_backend";
import { initWasm, type WasmApi, type WasmVariant } from "./runtime/wasm_loader";
import { precompileWasm } from "./runtime/wasm_preload";
import { IO_IPC_HID_IN_QUEUE_KIND, type WorkerRole } from "./runtime/shared_layout";
import { DiskManager, type RemoteCacheStatusSerializable } from "./storage/disk_manager";
import { pruneRemoteCachesAndRefresh } from "./storage/remote_cache_ui_actions";
import type { DiskImageMetadata, MountConfig } from "./storage/metadata";
import { OPFS_DISKS_PATH, OPFS_LEGACY_IMAGES_DIR } from "./storage/metadata";
import { RuntimeDiskClient, type OpenResult } from "./storage/runtime_disk_client";
import { type JitWorkerResponse } from "./workers/jit_protocol";
import { JitWorkerClient } from "./workers/jit_worker_client";
import { DemoVmWorkerClient } from "./workers/demo_vm_worker_client";
import { openRingByKind } from "./ipc/ipc";
import {
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  isGpuWorkerMessageBase,
  type GpuRuntimeScreenshotResponseMessage,
} from "./ipc/gpu-protocol";
import { SHARED_FRAMEBUFFER_HEADER_U32_LEN, SharedFramebufferHeaderIndex } from "./ipc/shared-layout";
import { mountSettingsPanel } from "./ui/settings_panel";
import { mountStatusPanel } from "./ui/status_panel";
import { mountInputDiagnosticsPanel, readInputDiagnosticsSnapshotFromStatus } from "./ui/input_diagnostics_panel";
import { installNetTraceUI } from "./net/trace_ui";
import { renderWebUsbPanel } from "./usb/webusb_panel";
import { renderWebUsbUhciHarnessPanel } from "./usb/webusb_uhci_harness_panel";
import { isUsbUhciHarnessStatusMessage, type WebUsbUhciHarnessRuntimeSnapshot } from "./usb/webusb_harness_runtime";
import { isUsbEhciHarnessStatusMessage, type WebUsbEhciHarnessRuntimeSnapshot } from "./usb/webusb_ehci_harness_runtime";
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
installInputRecordReplayGlobalApi();

if (new URLSearchParams(location.search).has("trace")) perf.traceStart();
perf.instant("boot:main:start", "p");

installAeroGlobals();

const workerCoordinator = new WorkerCoordinator();
installNetTraceBackendOnAeroGlobal(workerCoordinator);
installIoInputTelemetryBackendOnAeroGlobal(workerCoordinator);
installBootDeviceBackendOnAeroGlobal(workerCoordinator);
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
      // WebHID passthrough currently targets the legacy IO worker USB/HID stack. In
      // `vmRuntime=machine`, the IO worker runs in a host-only stub mode and does not
      // initialize guest device models, so WebHID passthrough is not yet supported.
      //
      // Keep detach/cleanup messages as no-ops so users can still remove already-attached
      // devices after switching runtimes, but fail fast on attach so the UI can surface a
      // clear error instead of silently doing nothing.
      const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
      if (vmRuntime === "machine") {
        const kind = (message as unknown as { type?: unknown } | null)?.type;
        if (kind === "hid:attach" || kind === "hid:attachHub") {
          throw new Error("WebHID passthrough is currently unavailable in vmRuntime=machine.");
        }
        return;
      }

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
  // WebHID passthrough is not supported in vmRuntime=machine (see target wiring above).
  // Disable the SharedArrayBuffer fast path so we don't write into the HID ring buffer when
  // the IO worker is running in host-only stub mode.
  const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
  if (vmRuntime === "machine") {
    webHidManager.setInputReportRing(null);
    return;
  }

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
    const crossOriginIsolated = (globalThis as unknown as { crossOriginIsolated?: unknown }).crossOriginIsolated;
    if (crossOriginIsolated !== true) return false;
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

type BootDiskSelection = { mounts: MountConfig; hdd?: DiskImageMetadata; cd?: DiskImageMetadata };
type OpenedDisk = { meta: DiskImageMetadata; open: OpenResult };
type OpenedBootDisks = { client: RuntimeDiskClient; mounts: MountConfig; hdd?: OpenedDisk; cd?: OpenedDisk };

let autoAdoptedLegacyOpfsImages: Promise<void> | null = null;
// Best-effort compatibility: `AeroConfig.activeDiskImage` is deprecated but older links/harnesses
// may still set it via `?disk=...`. Treat it as an initial mount hint (once per session) so those
// flows keep working, without using it as a runtime mode toggle.
let legacyActiveDiskImageApplied: string | null = null;

async function ensureLegacyOpfsImagesAdopted(manager: DiskManager): Promise<void> {
  // Auto-adopt legacy images once per session so users upgrading from the v1 disk storage flow see
  // their existing OPFS `images/` files.
  if (!autoAdoptedLegacyOpfsImages) {
    autoAdoptedLegacyOpfsImages =
      manager.backend === "opfs"
        ? manager
            .adoptLegacyOpfsImages()
            .then(() => undefined)
            .catch(() => undefined)
        : Promise.resolve();
  }
  await autoAdoptedLegacyOpfsImages;
}

function resolveLegacyActiveDiskImageRef(disks: DiskImageMetadata[], ref: string): DiskImageMetadata | undefined {
  const trimmed = ref.trim();
  if (!trimmed) return undefined;
  // Try to match by disk ID, name, or local filename (supports legacy OPFS `images/` adoptions).
  const fileName = trimmed.includes("/") ? trimmed.split("/").filter(Boolean).at(-1) ?? trimmed : trimmed;
  return (
    disks.find((d) => d.id === trimmed) ??
    disks.find((d) => d.name === trimmed) ??
    disks.find((d) => d.source === "local" && d.fileName === trimmed) ??
    (fileName && fileName !== trimmed ? disks.find((d) => d.source === "local" && d.fileName === fileName) : undefined)
  );
}

async function maybeApplyLegacyActiveDiskImageMountHint(
  manager: DiskManager,
  disks: DiskImageMetadata[],
  mounts: MountConfig,
): Promise<MountConfig> {
  const legacyConfigState = configManager.getState();
  const legacyActiveDiskImage = legacyConfigState.effective.activeDiskImage;
  const legacyActiveDiskImageLocked = legacyConfigState.lockedKeys.has("activeDiskImage");
  const mountsEmpty = !mounts.hddId && !mounts.cdId;

  // IMPORTANT: Avoid letting a stale stored/static `activeDiskImage` override user-selected mounts
  // in DiskManager. Only apply it automatically when:
  // - the user explicitly set it via URL query param (locked key), OR
  // - mounts are currently empty (fresh profile / first-run migration).
  if (
    !legacyActiveDiskImage ||
    legacyActiveDiskImageApplied === legacyActiveDiskImage ||
    (!legacyActiveDiskImageLocked && !mountsEmpty)
  ) {
    return mounts;
  }

  const resolved = resolveLegacyActiveDiskImageRef(disks, legacyActiveDiskImage);
  if (!resolved) return mounts;

  const nextMounts: MountConfig = { ...mounts };
  if (resolved.kind === "hdd") nextMounts.hddId = resolved.id;
  if (resolved.kind === "cd") nextMounts.cdId = resolved.id;
  const changed = nextMounts.hddId !== mounts.hddId || nextMounts.cdId !== mounts.cdId;
  if (changed) {
    try {
      mounts = await manager.setMounts(nextMounts);
    } catch {
      // ignore (best-effort; invalid refs should not break the disks panel)
      return mounts;
    }
  }

  legacyActiveDiskImageApplied = legacyActiveDiskImage;
  return mounts;
}

async function getBootDiskSelection(manager: DiskManager): Promise<BootDiskSelection> {
  await ensureLegacyOpfsImagesAdopted(manager);
  const [disks, mounts] = await Promise.all([manager.listDisks(), manager.getMounts()]);
  const nextMounts = await maybeApplyLegacyActiveDiskImageMountHint(manager, disks, mounts);
  const byId = new Map(disks.map((d) => [d.id, d]));
  return {
    mounts: nextMounts,
    hdd: nextMounts.hddId ? byId.get(nextMounts.hddId) : undefined,
    cd: nextMounts.cdId ? byId.get(nextMounts.cdId) : undefined,
  };
}

async function openBootDisks(manager: DiskManager): Promise<OpenedBootDisks> {
  await ensureLegacyOpfsImagesAdopted(manager);
  const [disks, mounts] = await Promise.all([manager.listDisks(), manager.getMounts()]);
  const nextMounts = await maybeApplyLegacyActiveDiskImageMountHint(manager, disks, mounts);
  const byId = new Map(disks.map((d) => [d.id, d]));
  const client = new RuntimeDiskClient();
  const opened: OpenedBootDisks = { client, mounts: nextMounts };

  try {
    if (nextMounts.hddId) {
      const meta = byId.get(nextMounts.hddId);
      if (!meta) throw new Error(`Mounted HDD disk not found: ${nextMounts.hddId}`);
      opened.hdd = { meta, open: await client.open(meta, { mode: "cow" }) };
    }
    if (nextMounts.cdId) {
      const meta = byId.get(nextMounts.cdId);
      if (!meta) throw new Error(`Mounted CD disk not found: ${nextMounts.cdId}`);
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
type MicAttachmentInfo = {
  ringBuffer: SharedArrayBuffer;
  sampleRate: number;
  /** Hashed deviceId (fnv1a32 of UTF-8). Avoids exporting raw device IDs. */
  deviceIdHash: string | null;
  backend: "worklet" | "script" | null;
  audioContextState: string | null;
  workletInitError: string | null;
  trackLabel: string | null;
  trackEnabled: boolean | null;
  trackMuted: boolean | null;
  trackReadyState: string | null;
  trackSettings: Record<string, unknown> | null;
  trackConstraints: Record<string, unknown> | null;
  trackCapabilities: Record<string, unknown> | null;
  bufferMs: number | null;
  echoCancellation: boolean | null;
  noiseSuppression: boolean | null;
  autoGainControl: boolean | null;
  muted: boolean | null;
};
let micAttachment: MicAttachmentInfo | null = null;

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

  const query = new URLSearchParams(location.search);
  // Enable the input diagnostics panel with any truthy `?input` value:
  //   - `?input=1`, `?input=true`, or even just `?input`
  // Disable explicitly with:
  //   - `?input=0` or `?input=false`
  const inputParam = query.get("input");
  const showInputDiagnostics = inputParam !== null && inputParam !== "0" && inputParam !== "false";

  const report = detectPlatformFeatures();
  const missing = explainMissingRequirements(report);

  const settingsHost = el("div", { class: "panel" });
  mountSettingsPanel(settingsHost, configManager, report);

  const statusHost = el("div", { class: "panel" });
  mountStatusPanel(statusHost, configManager, workerCoordinator);

  const nodes: Node[] = [
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
    renderMachineWorkerPanel(),
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
    ...(showInputDiagnostics ? [renderInputDiagnosticsPanel()] : []),
    renderInputPanel(),
    renderWebUsbBrokerPanel(),
    renderWebUsbPassthroughDemoWorkerPanel(),
    renderWebUsbUhciHarnessWorkerPanel(),
    renderWebUsbEhciHarnessWorkerPanel(),
    renderWorkersPanel(report),
    renderIpcDemoPanel(),
    renderMicrobenchPanel(),
  ];

  app.replaceChildren(...nodes);
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
      (globalThis as unknown as { __aeroWasmApi?: unknown }).__aeroWasmApi = api;
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
  let writable: FileSystemWritableFileStream;
  let truncateFallback = false;
  try {
    writable = await handle.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await handle.createWritable();
    truncateFallback = true;
  }
  if (truncateFallback) {
    // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
    // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
    try {
      await writable.truncate(0);
    } catch {
      // ignore
    }
  }
  // `FileSystemWritableFileStream.write()` only accepts ArrayBuffer-backed views in the
  // lib.dom typings. Snapshot buffers coming from threaded WASM builds can be backed by
  // SharedArrayBuffer, so clone into an ArrayBuffer-backed Uint8Array before writing.
  const payload = ensureArrayBufferBacked(bytes);

  try {
    await writable.write(payload);
    await writable.close();
  } catch (err) {
    try {
      await writable.abort(err);
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

function downloadJson(value: unknown, filename: string): void {
  const text = JSON.stringify(value, null, 2);
  downloadFile(new Blob([text], { type: "application/json" }), filename);
}

async function rgba8ToPngBlob(width: number, height: number, rgba8: Uint8Array): Promise<Blob> {
  const expectedBytes = width * height * 4;
  if (rgba8.byteLength !== expectedBytes) {
    throw new Error(`Invalid RGBA8 screenshot buffer size: got ${rgba8.byteLength}, expected ${expectedBytes}`);
  }

  // `GpuRuntimeScreenshotResponseMessage` is defined as a deterministic readback of the **source**
  // framebuffer bytes (pre-color-management). Internally the GPU worker treats those bytes as
  // linear RGBA8 (and decodes sRGB scanout/cursor formats to linear before blending/present).
  //
  // Canvas2D `putImageData` expects sRGB-encoded bytes, so encode linear->sRGB here so the saved
  // PNG matches what users expect to see when viewing the screenshot.

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) {
    throw new Error("2D canvas context unavailable; cannot encode screenshot.");
  }

  // `ImageData` does not accept SharedArrayBuffer-backed views. While GPU-worker
  // screenshots are expected to arrive as ArrayBuffer, defensively clone when
  // necessary so this helper is safe for any caller.
  const arrayBufferBacked = ensureArrayBufferBacked(rgba8);
  // Clone before encoding so callers that keep the original RGBA8 buffer (e.g. for hashing)
  // are unaffected.
  const srgb = new Uint8Array(arrayBufferBacked.byteLength);
  srgb.set(arrayBufferBacked);
  encodeLinearRgba8ToSrgbInPlace(srgb);
  const clamped = new Uint8ClampedArray(srgb.buffer, srgb.byteOffset, srgb.byteLength);
  ctx.putImageData(new ImageData(clamped, width, height), 0, 0);

  return await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (blob) => {
        if (blob) resolve(blob);
        else reject(new Error("Failed to encode screenshot as PNG (canvas.toBlob returned null)."));
      },
      "image/png",
      1.0,
    );
  });
}

function parseOptionalBoolParam(search: URLSearchParams, key: string): boolean | undefined {
  const raw = search.get(key);
  if (raw === null) return undefined;
  const normalized = raw.trim().toLowerCase();
  if (normalized === "" || normalized === "1" || normalized === "true") return true;
  if (normalized === "0" || normalized === "false") return false;
  return true;
}

function renderMachinePanel(): HTMLElement {
  const status = el("pre", { text: "Initializing canonical machine…" });
  const vgaInfo = el("pre", { text: "" });
  const inputHint = el("div", {
    class: "mono",
    text:
      "Tip: click the canvas to focus + request pointer lock (keyboard/mouse will be forwarded to the guest). " +
      "VBE mode test: add ?machineVbe=1280x720 to boot straight into a Bochs VBE 32bpp mode (requires VGA; set ?machineVga=1 or ?machineAerogpu=0). " +
      "GPU config test: add ?machineAerogpu=1 (or =0 to disable) to override the default GPU device model. " +
      "SMP test: add ?machineCpuCount=2 to request a 2-vCPU machine (requires native SMP support).",
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
  const testState = {
    ready: false,
    vgaSupported: false,
    framesPresented: 0,
    sharedFramesPublished: 0,
    transport: "none" as "none" | "ptr" | "copy",
    width: 0,
    height: 0,
    strideBytes: 0,
    error: null as string | null,
  };
  (globalThis as unknown as { __aeroMachinePanelTest?: typeof testState }).__aeroMachinePanelTest = testState;

  function setError(msg: string): void {
    error.textContent = msg;
    testState.error = msg;
    console.error(msg);
  }

  // Avoid pathological allocations if the guest (or a buggy WASM build) reports
  // an absurd scanout mode. Keep the UI responsive rather than attempting to
  // allocate multi-gigabyte buffers.
  const MAX_FRAME_BYTES = 32 * 1024 * 1024; // ~4K@60-ish upper bound for a demo panel.

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

    // Reserve enough room for the fixed tail instructions (sti + hlt/jmp) plus the 0x55AA boot
    // signature at bytes 510..511. Truncate overly-long messages rather than throwing.
    const FOOTER_BYTES = 4;
    for (const b of msgBytes) {
      // Per-byte encoding: mov al, imm8 (2 bytes) + out dx, al (1 byte).
      if (off + 3 > 510 - FOOTER_BYTES) break;
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

  function buildVbeBootSector(opts: { message: string; width: number; height: number }): Uint8Array {
    const msgBytes = encoder.encode(opts.message);
    const width = Math.max(1, Math.min(0xffff, Math.trunc(opts.width)));
    const height = Math.max(1, Math.min(0xffff, Math.trunc(opts.height)));
    const sector = new Uint8Array(512);
    let off = 0;

    // Program a Bochs VBE mode (WxHx32) and write a single red pixel at (0,0).
    // cld
    sector[off++] = 0xfc;

    // mov dx, 0x01CE  (Bochs VBE index port)
    sector.set([0xba, 0xce, 0x01], off);
    off += 3;

    const writeVbeReg = (index: number, value: number) => {
      // mov ax, imm16 (index)
      sector.set([0xb8, index & 0xff, (index >>> 8) & 0xff], off);
      off += 3;
      // out dx, ax
      sector[off++] = 0xef;
      // inc dx (0x01CF)
      sector[off++] = 0x42;
      // mov ax, imm16 (value)
      sector.set([0xb8, value & 0xff, (value >>> 8) & 0xff], off);
      off += 3;
      // out dx, ax
      sector[off++] = 0xef;
      // dec dx (back to 0x01CE)
      sector[off++] = 0x4a;
    };

    // XRES = width
    writeVbeReg(0x0001, width);
    // YRES = height
    writeVbeReg(0x0002, height);
    // BPP = 32
    writeVbeReg(0x0003, 32);
    // ENABLE = 0x0041 (enable + LFB)
    writeVbeReg(0x0004, 0x0041);
    // BANK = 0
    writeVbeReg(0x0005, 0);

    // mov ax, 0xA000 ; mov es, ax ; xor di, di
    sector.set([0xb8, 0x00, 0xa0, 0x8e, 0xc0, 0x31, 0xff], off);
    off += 7;

    // Write a red pixel at (0,0) in BGRX format expected by the SVGA renderer.
    // mov al, 0x00 ; stosb ; stosb ; mov al, 0xff ; stosb ; mov al, 0x00 ; stosb
    sector.set([0xb0, 0x00, 0xaa, 0xaa, 0xb0, 0xff, 0xaa, 0xb0, 0x00, 0xaa], off);
    off += 10;

    // Serial output (COM1).
    // mov dx, 0x3f8
    sector.set([0xba, 0xf8, 0x03], off);
    off += 3;
    const FOOTER_BYTES = 4;
    for (const b of msgBytes) {
      // Per-byte encoding: mov al, imm8 (2 bytes) + out dx, al (1 byte).
      if (off + 3 > 510 - FOOTER_BYTES) break;
      sector.set([0xb0, b, 0xee], off); // mov al, imm8 ; out dx, al
      off += 3;
    }

    // cli; hlt; jmp $
    sector[off++] = 0xfa;
    sector[off++] = 0xf4;
    sector.set([0xeb, 0xfe], off);
    off += 2;

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xaa;
    return sector;
  }

  wasmInitPromise
    .then(({ api, variant, wasmMemory }) => {
      const ramSizeBytes = 2 * 1024 * 1024;
      const bootMessage = "Hello from aero-machine\\n";
      const search = typeof window !== "undefined" ? new URLSearchParams(window.location.search) : new URLSearchParams();
      const cpuCount = (() => {
        const raw = search.get("machineCpuCount");
        if (raw === null) return 1;
        const n = Number.parseInt(raw.trim(), 10);
        if (!Number.isFinite(n) || n < 1 || n > 255) return 1;
        return n;
      })();
      const enableAerogpuOverride = parseOptionalBoolParam(search, "machineAerogpu");
      const enableVgaOverride = parseOptionalBoolParam(search, "machineVga");
      // `Machine.new_with_config` is optional across wasm builds. Stash the property in a local so
      // TypeScript can safely narrow before invoking it (property reads are not stable).
      const newWithConfig = api.Machine.new_with_config;
      const newWithCpuCount = api.Machine.new_with_cpu_count;
      const wantsGraphicsOverride = enableAerogpuOverride !== undefined || enableVgaOverride !== undefined;
      const machine = (() => {
        if (wantsGraphicsOverride && typeof newWithConfig === "function") {
          const enableAerogpu =
            enableAerogpuOverride ?? (enableVgaOverride !== undefined ? !enableVgaOverride : false);
          return newWithConfig(ramSizeBytes, enableAerogpu, enableVgaOverride, cpuCount !== 1 ? cpuCount : undefined);
        }
        if (cpuCount !== 1 && typeof newWithCpuCount === "function") {
          return newWithCpuCount(ramSizeBytes, cpuCount);
        }
        return new api.Machine(ramSizeBytes);
      })();
      const vbeRaw = search.get("machineVbe");
      let diskImage = buildSerialBootSector(bootMessage);
      // Bochs VBE programming requires the legacy VGA/VBE device model. If the canonical machine
      // was constructed without VGA, ignore `machineVbe` so we keep visible text output via the
      // `0xB8000` fallback scanout.
      //
      // `vga_width()` returns 0 when the machine has no VGA device model attached.
      const vgaDevicePresent = (() => {
        try {
          const vgaWidth = machine.vga_width;
          return typeof vgaWidth === "function" && vgaWidth.call(machine) > 0;
        } catch {
          return false;
        }
      })();
      if (vbeRaw && vgaDevicePresent) {
        const match = /^(\d+)x(\d+)$/.exec(vbeRaw.trim());
        if (match) {
          const width = Number.parseInt(match[1] ?? "", 10);
          const height = Number.parseInt(match[2] ?? "", 10);
          const requiredBytes = width * height * 4;
          if (
            Number.isFinite(width) &&
            Number.isFinite(height) &&
            width > 0 &&
            height > 0 &&
            Number.isFinite(requiredBytes) &&
            requiredBytes > 0 &&
            requiredBytes <= MAX_FRAME_BYTES
          ) {
            diskImage = buildVbeBootSector({ message: bootMessage, width, height });
          }
        }
      }
      machine.set_disk_image(diskImage);
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
      const canUseInputCapture =
        typeof machine.inject_key_scancode_bytes === "function" || typeof machine.inject_keyboard_bytes === "function";

      let stopInputCapture: (() => void) | null = null;
      if (canUseInputCapture) {
        let inputCapture: InputCapture | null = null;
        // Avoid per-event allocations when falling back to `inject_keyboard_bytes` (older WASM builds).
        // Preallocate small scancode buffers for len=1..4.
        const packedScancodeScratch = [
          new Uint8Array(0),
          new Uint8Array(1),
          new Uint8Array(2),
          new Uint8Array(3),
          new Uint8Array(4),
        ];
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
                  const bytes = packedScancodeScratch[len]!;
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
                  const dyDown = negateI32Saturating(dyPs2);
                  machine.inject_mouse_motion(dx, dyDown, 0);
                }
              } else if (type === InputEventType.MouseWheel) {
                const dz = words[off + 2] | 0;
                if (typeof machine.inject_ps2_mouse_motion === "function") {
                  machine.inject_ps2_mouse_motion(0, 0, dz);
                } else if (typeof machine.inject_mouse_motion === "function") {
                  machine.inject_mouse_motion(0, 0, dz);
                }
              } else if (type === InputEventType.MouseButtons) {
                // DOM `MouseEvent.buttons` bitfield:
                // - bit0 left, bit1 right, bit2 middle, bit3 back, bit4 forward.
                //
                // The canonical Machine PS/2 mouse model can surface back/forward (IntelliMouse
                // Explorer extensions) when the guest enables it, so preserve the low 5 bits.
                const buttons = words[off + 2] & 0xff;
                const mask = buttons & 0x1f;
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
          onBeforeSendBatch: inputRecordReplay.captureHook,
        });
        capture.start();
        inputCapture = capture;

        stopInputCapture = () => {
          const current = inputCapture;
          if (!current) return;
          try {
            current.stop();
          } catch {
            // ignore
          }
          inputCapture = null;
        };
      } else {
        // Older WASM builds may not expose scancode injection helpers; fall back to the
        // simple `inject_browser_key` + mouse helpers (when available) so the demo remains usable.
        let running = true;

        const requestPointerLock = (): void => {
          const fn = (canvas as unknown as { requestPointerLock?: unknown }).requestPointerLock;
          if (typeof fn !== "function") return;
          try {
            fn.call(canvas);
          } catch {
            // ignore
          }
        };

        const onClick = (): void => {
          canvas.focus();
          requestPointerLock();
        };

        const onKeyDown = (ev: KeyboardEvent): void => {
          if (!running) return;
          try {
            machine.inject_browser_key(ev.code, true);
            ev.preventDefault();
            ev.stopPropagation();
          } catch {
            // ignore
          }
        };

        const onKeyUp = (ev: KeyboardEvent): void => {
          if (!running) return;
          try {
            machine.inject_browser_key(ev.code, false);
            ev.preventDefault();
            ev.stopPropagation();
          } catch {
            // ignore
          }
        };

        const onMouseMove = (ev: MouseEvent): void => {
          if (!running) return;
          if (typeof machine.inject_mouse_motion !== "function") return;
          const dx = ev.movementX | 0;
          const dy = ev.movementY | 0;
          if (dx === 0 && dy === 0) return;
          try {
            machine.inject_mouse_motion(dx, dy, 0);
          } catch {
            // ignore
          }
        };

        const onWheel = (ev: WheelEvent): void => {
          if (!running) return;
          if (typeof machine.inject_mouse_motion !== "function") return;
          const wheel = ev.deltaY === 0 ? 0 : ev.deltaY < 0 ? 1 : -1;
          if (wheel === 0) return;
          try {
            machine.inject_mouse_motion(0, 0, wheel);
            ev.preventDefault();
            ev.stopPropagation();
          } catch {
            // ignore
          }
        };

        const onMouseDown = (ev: MouseEvent): void => {
          if (!running) return;
          if (typeof machine.inject_mouse_button !== "function") return;
          try {
            machine.inject_mouse_button(ev.button | 0, true);
            ev.preventDefault();
            ev.stopPropagation();
          } catch {
            // ignore
          }
        };

        const onMouseUp = (ev: MouseEvent): void => {
          if (!running) return;
          if (typeof machine.inject_mouse_button !== "function") return;
          try {
            machine.inject_mouse_button(ev.button | 0, false);
            ev.preventDefault();
            ev.stopPropagation();
          } catch {
            // ignore
          }
        };

        const onContextMenu = (ev: MouseEvent): void => {
          ev.preventDefault();
          ev.stopPropagation();
        };

        canvas.addEventListener("click", onClick);
        canvas.addEventListener("keydown", onKeyDown);
        canvas.addEventListener("keyup", onKeyUp);
        canvas.addEventListener("mousemove", onMouseMove);
        canvas.addEventListener("wheel", onWheel, { passive: false });
        canvas.addEventListener("mousedown", onMouseDown);
        canvas.addEventListener("mouseup", onMouseUp);
        canvas.addEventListener("contextmenu", onContextMenu);

        stopInputCapture = () => {
          running = false;
          canvas.removeEventListener("click", onClick);
          canvas.removeEventListener("keydown", onKeyDown);
          canvas.removeEventListener("keyup", onKeyUp);
          canvas.removeEventListener("mousemove", onMouseMove);
          canvas.removeEventListener("wheel", onWheel);
          canvas.removeEventListener("mousedown", onMouseDown);
          canvas.removeEventListener("mouseup", onMouseUp);
          canvas.removeEventListener("contextmenu", onContextMenu);
          try {
            if (typeof document !== "undefined" && document.pointerLockElement === canvas) {
              document.exitPointerLock?.();
            }
          } catch {
            // ignore
          }
        };
      }

      const hasDisplayPresent = typeof (machine as unknown as { display_present?: unknown }).display_present === "function";
      const hasDisplaySize =
        typeof (machine as unknown as { display_width?: unknown }).display_width === "function" &&
        typeof (machine as unknown as { display_height?: unknown }).display_height === "function";
      const hasDisplayPtr =
        !!wasmMemory &&
        typeof (machine as unknown as { display_framebuffer_ptr?: unknown }).display_framebuffer_ptr === "function" &&
        typeof (machine as unknown as { display_framebuffer_len_bytes?: unknown }).display_framebuffer_len_bytes === "function";
      const hasDisplayCopy =
        typeof (machine as unknown as { display_framebuffer_copy_rgba8888?: unknown }).display_framebuffer_copy_rgba8888 === "function";
      const hasDisplay = hasDisplayPresent && hasDisplaySize && (hasDisplayPtr || hasDisplayCopy);

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

      testState.vgaSupported = hasDisplay || hasVga;
      vgaInfo.textContent =
        hasDisplay
          ? "vga: ready (using display_* scanout)"
          : hasVga
            ? "vga: ready"
            : "vga: unavailable (WASM build missing scanout exports)";

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
      let sharedVgaFrameCounter = 0;

      function ensureSharedVga(width: number, height: number, strideBytes: number): ReturnType<typeof wrapSharedFramebuffer> | null {
        if (typeof SharedArrayBuffer === "undefined") return null;

        let requiredBytes: number;
        try {
          requiredBytes = requiredFramebufferBytes(width, height, strideBytes);
        } catch {
          return null;
        }
        if (!Number.isFinite(requiredBytes) || requiredBytes <= 0 || requiredBytes > MAX_FRAME_BYTES + 4096) {
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
          // Keep the frame counter monotonic across SharedArrayBuffer resizes so tests/harnesses
          // can treat it as a stable "frames published" counter.
          storeHeaderI32(sharedVga.header, HEADER_INDEX_FRAME_COUNTER, sharedVgaFrameCounter);
          sharedVgaWidth = width;
          sharedVgaHeight = height;
          sharedVgaStrideBytes = strideBytes;

          // Expose the SAB for harnesses / debugging (optional).
          (globalThis as unknown as { __aeroMachineVgaFramebuffer?: unknown }).__aeroMachineVgaFramebuffer = sharedVgaSab;
          return sharedVga;
        }

        if (!sharedVga) {
          sharedVga = wrapSharedFramebuffer(sharedVgaSab, 0);
          initFramebufferHeader(sharedVga.header, { width, height, strideBytes });
          storeHeaderI32(sharedVga.header, HEADER_INDEX_FRAME_COUNTER, sharedVgaFrameCounter);
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

      function tryPresentScanoutFrame(kind: "display" | "vga"): boolean {
        if (kind === "display") {
          if (!hasDisplayPresent || !hasDisplaySize || (!hasDisplayPtr && !hasDisplayCopy)) return false;
        } else {
          if (!hasVga || vgaFailed) return false;
        }

        const presentFn =
          kind === "display"
            ? (machine as unknown as { display_present?: () => void }).display_present
            : (machine as unknown as { vga_present?: () => void }).vga_present;
        const widthFn =
          kind === "display"
            ? (machine as unknown as { display_width?: () => number }).display_width
            : (machine as unknown as { vga_width?: () => number }).vga_width;
        const heightFn =
          kind === "display"
            ? (machine as unknown as { display_height?: () => number }).display_height
            : (machine as unknown as { vga_height?: () => number }).vga_height;
        if (typeof presentFn !== "function" || typeof widthFn !== "function" || typeof heightFn !== "function") return false;

        presentFn.call(machine);

        const width = widthFn.call(machine) >>> 0;
        const height = heightFn.call(machine) >>> 0;
        if (width === 0 || height === 0) return false;

        const strideFn =
          kind === "display"
            ? (machine as unknown as { display_stride_bytes?: () => number }).display_stride_bytes
            : (machine as unknown as { vga_stride_bytes?: () => number }).vga_stride_bytes;
        // Some older WASM builds may not expose a stride helper; assume tightly packed RGBA8888.
        let strideBytes = (typeof strideFn === "function" ? strideFn.call(machine) : width * 4) >>> 0;
        if (strideBytes < width * 4) return false;

        const requiredDstBytes = width * height * 4;
        let requiredSrcBytes = strideBytes * height;
        if (
          !Number.isFinite(requiredDstBytes) ||
          requiredDstBytes <= 0 ||
          requiredDstBytes > MAX_FRAME_BYTES ||
          !Number.isFinite(requiredSrcBytes) ||
          requiredSrcBytes <= 0 ||
          requiredSrcBytes > MAX_FRAME_BYTES
        ) {
          return false;
        }

        let src: Uint8Array | null = null;
        let transport: "ptr" | "copy" | "none" = "none";

        const ptrFn =
          kind === "display"
            ? (machine as unknown as { display_framebuffer_ptr?: () => number }).display_framebuffer_ptr
            : (machine as unknown as { vga_framebuffer_ptr?: () => number }).vga_framebuffer_ptr;
        const lenFn =
          kind === "display"
            ? (machine as unknown as { display_framebuffer_len_bytes?: () => number }).display_framebuffer_len_bytes
            : (machine as unknown as { vga_framebuffer_len_bytes?: () => number }).vga_framebuffer_len_bytes;

        if (wasmMemory && typeof ptrFn === "function" && typeof lenFn === "function") {
          const ptr = ptrFn.call(machine) >>> 0;
          const lenBytes = lenFn.call(machine) >>> 0;
          if (ptr !== 0) {
            let effectiveStrideBytes = strideBytes;
            let effectiveRequiredSrcBytes = requiredSrcBytes;
            // Some builds may report a stride but still expose a tightly-packed framebuffer length.
            // If the length matches `width*height*4`, treat it as tightly packed to avoid rejecting
            // the scanout entirely.
            if (lenBytes < effectiveRequiredSrcBytes && lenBytes === requiredDstBytes) {
              effectiveStrideBytes = width * 4;
              effectiveRequiredSrcBytes = requiredDstBytes;
            }
            if (lenBytes >= effectiveRequiredSrcBytes) {
              const buf = wasmMemory.buffer;
              if (ptr + effectiveRequiredSrcBytes <= buf.byteLength) {
                strideBytes = effectiveStrideBytes;
                requiredSrcBytes = effectiveRequiredSrcBytes;
                src = new Uint8Array(buf, ptr, requiredSrcBytes);
                transport = "ptr";
              }
            }
          }
        }

        if (!src) {
          const copyFn =
            kind === "display"
              ? (machine as unknown as { display_framebuffer_copy_rgba8888?: () => Uint8Array }).display_framebuffer_copy_rgba8888
              : (machine as unknown as { vga_framebuffer_copy_rgba8888?: () => Uint8Array }).vga_framebuffer_copy_rgba8888;
          const legacyFn =
            kind === "vga"
              ? (machine as unknown as { vga_framebuffer_rgba8888_copy?: () => Uint8Array | null }).vga_framebuffer_rgba8888_copy
              : undefined;

          // Fall back to a JS-owned copy if we cannot access WASM linear memory (or if the ptr/len
          // fast path fails validation).
          const copied =
            typeof copyFn === "function" ? copyFn.call(machine) : typeof legacyFn === "function" ? legacyFn.call(machine) : null;
          if (copied && copied.byteLength) {
            let effectiveStrideBytes = strideBytes;
            let effectiveRequiredSrcBytes = requiredSrcBytes;
            if (copied.byteLength < effectiveRequiredSrcBytes && copied.byteLength === requiredDstBytes) {
              // Some helpers return tight-packed buffers even if a stride is reported.
              effectiveStrideBytes = width * 4;
              effectiveRequiredSrcBytes = requiredDstBytes;
            }
            if (copied.byteLength >= effectiveRequiredSrcBytes) {
              strideBytes = effectiveStrideBytes;
              requiredSrcBytes = effectiveRequiredSrcBytes;
              src = copied;
              transport = "copy";
            }
          }
        }

        if (!src) return false;

        testState.transport = transport;

        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
        }

        if (!imageDataBytes || dstWidth !== width || dstHeight !== height || imageDataBytes.byteLength !== requiredDstBytes) {
          dstWidth = width;
          dstHeight = height;
          imageDataBytes = new Uint8ClampedArray(requiredDstBytes);
          imageData = new ImageData(imageDataBytes, width, height);
        }
        if (!imageData || !imageDataBytes) return false;

        // Optional: also publish the scanout into a SharedArrayBuffer-backed framebuffer so
        // existing shared-framebuffer plumbing can consume it (e.g. GPU-worker harnesses).
        const shared = ensureSharedVga(width, height, strideBytes);
        if (shared) {
          shared.pixelsU8.set(src.subarray(0, requiredSrcBytes));
          sharedVgaFrameCounter = (sharedVgaFrameCounter + 1) >>> 0;
          storeHeaderI32(shared.header, HEADER_INDEX_FRAME_COUNTER, sharedVgaFrameCounter);
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

        // Canvas2D expects sRGB-encoded bytes; the scanout/export APIs are treated as linear RGBA8.
        encodeLinearRgba8ToSrgbInPlace(
          new Uint8Array(imageDataBytes.buffer, imageDataBytes.byteOffset, imageDataBytes.byteLength),
        );

        ctx2.putImageData(imageData, 0, 0);
        testState.framesPresented += 1;
        testState.width = width;
        testState.height = height;
        testState.strideBytes = strideBytes;
        const pointerLock = typeof document !== "undefined" && document.pointerLockElement === canvas ? "yes" : "no";
        vgaInfo.textContent =
          `vga: ${width}x${height} stride=${strideBytes} ` +
          `frames=${testState.framesPresented} transport=${testState.transport} src=${kind}` +
          (testState.sharedFramesPublished ? ` shared=${testState.sharedFramesPublished}` : "") +
          ` pointerLock=${pointerLock}`;
        return true;
      }

      function presentVgaFrame(): void {
        if (vgaFailed) return;
        try {
          // Prefer unified `display_*` scanout when available; fall back to legacy VGA scanout exports.
          if (tryPresentScanoutFrame("display")) return;
          void tryPresentScanoutFrame("vga");
        } catch (err) {
          vgaFailed = true;
          const message = err instanceof Error ? err.message : String(err);
          vgaInfo.textContent = `vga: error (${message})`;
          setError(`Machine demo scanout present failed: ${message}`);
        }
      }

      const timer = window.setInterval(() => {
        const machineAny = machine as unknown as Record<string, unknown>;
        const runSlice = machineAny.run_slice ?? machineAny.runSlice;
        if (typeof runSlice !== "function") {
          throw new Error("Machine missing run_slice/runSlice export.");
        }
        const exit = (runSlice as (maxInsts: number) => unknown).call(machine, 50_000) as any;
        const exitKind = exit.kind;
        const exitExecuted = exit.executed;
        const exitDetail = exit.detail;

        // Avoid copying serial output into JS when empty.
        const serialLenFn = machineAny.serial_output_len ?? machineAny.serialOutputLen;
        const shouldReadSerial = (() => {
          if (typeof serialLenFn !== "function") return true;
          try {
            const n = (serialLenFn as () => number).call(machine);
            return typeof n === "number" && Number.isFinite(n) && n > 0;
          } catch {
            return true;
          }
        })();
        if (shouldReadSerial) {
          const serialOutputFn = machineAny.serial_output ?? machineAny.serialOutput;
          if (typeof serialOutputFn !== "function") {
            throw new Error("Machine missing serial_output/serialOutput export.");
          }
          const bytes = (serialOutputFn as () => unknown).call(machine) as Uint8Array;
          if (bytes.byteLength) {
            output.textContent = `${output.textContent ?? ""}${decoder.decode(bytes)}`;
          }
        }

        presentVgaFrame();

        status.textContent = `run_slice: kind=${exitKind} executed=${exitExecuted} detail=${exitDetail}`;
        exit.free();

        // `RunExitKind::Completed` is 0 and `RunExitKind::Halted` is 1.
        // Keep ticking while halted so injected interrupts (keyboard/mouse) can wake the CPU.
        if (exitKind !== 0 && exitKind !== 1) {
          window.clearInterval(timer);
          stopInputCapture?.();
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
    el("h2", { text: "Machine (canonical VM) – serial + display scanout demo" }),
    status,
    inputHint,
    el("div", { class: "row" }, canvas),
    vgaInfo,
    output,
    error,
  );
}

function renderMachineWorkerPanel(): HTMLElement {
  const status = el("pre", { text: "Machine worker demo: idle" });
  const vgaInfo = el("pre", { text: "" });
  const canvas = el("canvas", { id: "canonical-machine-vga-worker-canvas" }) as HTMLCanvasElement;
  canvas.tabIndex = 0;
  canvas.style.width = "640px";
  canvas.style.height = "400px";
  canvas.style.border = "1px solid rgba(127, 127, 127, 0.5)";
  canvas.style.background = "#000";
  canvas.style.imageRendering = "pixelated";
  canvas.addEventListener("click", () => canvas.focus());

  const output = el("pre", { text: "" });
  const error = el("pre", { text: "" });

  const decoder = new TextDecoder();

  // Expose worker demo state for Playwright smoke tests.
  const testState = {
    ready: true,
    running: false,
    transport: "none" as "none" | "shared" | "copy",
    framesPresented: 0,
    width: 0,
    height: 0,
    strideBytes: 0,
    error: null as string | null,
  };
  (globalThis as unknown as { __aeroMachineWorkerPanelTest?: typeof testState }).__aeroMachineWorkerPanelTest = testState;

  let worker: Worker | null = null;
  let presenter: VgaPresenter | null = null;
  let shared: ReturnType<typeof wrapSharedFramebuffer> | null = null;
  let sharedPollTimer: number | null = null;
  let inputAttached = false;

  const postToWorker = (msg: unknown): void => {
    const w = worker;
    if (!w) return;
    try {
      w.postMessage(msg);
    } catch {
      // ignore
    }
  };

  const onKeyDown = (ev: KeyboardEvent): void => {
    if (!testState.running) return;
    if (ev.repeat) return;
    postToWorker({ type: "machineVga.inject_browser_key", code: ev.code, pressed: true });
    ev.preventDefault();
    ev.stopPropagation();
  };

  const onKeyUp = (ev: KeyboardEvent): void => {
    if (!testState.running) return;
    postToWorker({ type: "machineVga.inject_browser_key", code: ev.code, pressed: false });
    ev.preventDefault();
    ev.stopPropagation();
  };

  const onMouseMove = (ev: MouseEvent): void => {
    if (!testState.running) return;
    // Use `movementX/Y` so pointer-lock deltas work when enabled.
    const dx = ev.movementX | 0;
    const dy = ev.movementY | 0;
    if (dx === 0 && dy === 0) return;
    postToWorker({ type: "machineVga.inject_mouse_motion", dx, dy, wheel: 0 });
    ev.preventDefault();
    ev.stopPropagation();
  };

  const onWheel = (ev: WheelEvent): void => {
    if (!testState.running) return;
    const dy = ev.deltaY;
    if (!Number.isFinite(dy) || dy === 0) return;
    // PS/2 wheel: positive is wheel up, whereas WheelEvent.deltaY is positive down.
    const wheel = (-Math.sign(dy)) | 0;
    if (wheel === 0) return;
    postToWorker({ type: "machineVga.inject_mouse_motion", dx: 0, dy: 0, wheel });
    ev.preventDefault();
    ev.stopPropagation();
  };

  const onMouseDown = (ev: MouseEvent): void => {
    if (!testState.running) return;
    postToWorker({ type: "machineVga.inject_mouse_button", button: ev.button | 0, pressed: true });
    ev.preventDefault();
    ev.stopPropagation();
  };

  const onMouseUp = (ev: MouseEvent): void => {
    if (!testState.running) return;
    postToWorker({ type: "machineVga.inject_mouse_button", button: ev.button | 0, pressed: false });
    ev.preventDefault();
    ev.stopPropagation();
  };

  const onContextMenu = (ev: MouseEvent): void => {
    // Avoid the browser context menu stealing focus.
    ev.preventDefault();
    ev.stopPropagation();
  };

  const detachInput = (): void => {
    if (!inputAttached) return;
    inputAttached = false;
    canvas.removeEventListener("keydown", onKeyDown);
    canvas.removeEventListener("keyup", onKeyUp);
    canvas.removeEventListener("mousemove", onMouseMove);
    canvas.removeEventListener("wheel", onWheel);
    canvas.removeEventListener("mousedown", onMouseDown);
    canvas.removeEventListener("mouseup", onMouseUp);
    canvas.removeEventListener("contextmenu", onContextMenu);
  };

  const attachInput = (): void => {
    if (inputAttached) return;
    inputAttached = true;
    canvas.addEventListener("keydown", onKeyDown);
    canvas.addEventListener("keyup", onKeyUp);
    canvas.addEventListener("mousemove", onMouseMove);
    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("mousedown", onMouseDown);
    canvas.addEventListener("mouseup", onMouseUp);
    canvas.addEventListener("contextmenu", onContextMenu);
  };

  canvas.addEventListener("dblclick", () => {
    const fn = (canvas as unknown as { requestPointerLock?: unknown }).requestPointerLock;
    if (typeof fn !== "function") return;
    try {
      fn.call(canvas);
    } catch {
      // ignore
    }
  });

  const stop = (): void => {
    testState.running = false;
    testState.transport = "none";
    testState.framesPresented = 0;
    testState.width = 0;
    testState.height = 0;
    testState.strideBytes = 0;

    if (worker) {
      try {
        worker.postMessage({ type: "machineVga.stop" });
      } catch {
        // ignore
      }
      try {
        worker.terminate();
      } catch {
        // ignore
      }
      worker = null;
    }
    detachInput();
    if (presenter) {
      presenter.destroy();
      presenter = null;
    }
    if (sharedPollTimer !== null) {
      window.clearInterval(sharedPollTimer);
      sharedPollTimer = null;
    }
    shared = null;
    vgaInfo.textContent = "";
  };

  const start = (): void => {
    stop();
    error.textContent = "";
    output.textContent = "";
    testState.error = null;
    testState.running = true;

    status.textContent = "Machine worker demo: starting…";
    const w = new Worker(new URL("./workers/machine_vga.worker.ts", import.meta.url), { type: "module" });
    worker = w;
    attachInput();

    w.addEventListener("message", (ev: MessageEvent<unknown>) => {
      const data = ev.data as unknown;
      if (!data || typeof data !== "object") return;
      const msg = data as Record<string, unknown>;

      if (msg.type === "machineVga.ready") {
        const transport = msg.transport === "shared" ? "shared" : "copy";
        status.textContent = `Machine worker demo: ready (transport=${transport})`;
        testState.transport = transport;

        if (!presenter) {
          presenter = new VgaPresenter(canvas, { scaleMode: "auto", integerScaling: true, maxPresentHz: 60 });
          presenter.start();
        }

        if (transport === "shared") {
          const sab = msg.framebuffer;
          if (typeof SharedArrayBuffer === "undefined" || !(sab instanceof SharedArrayBuffer)) {
            error.textContent = "machineVga.ready missing SharedArrayBuffer framebuffer";
            testState.error = error.textContent;
            return;
          }
          shared = wrapSharedFramebuffer(sab, 0);
          presenter.setSharedFramebuffer(shared);

          if (sharedPollTimer === null) {
            sharedPollTimer = window.setInterval(() => {
              if (!shared) return;
            const w = Atomics.load(shared.header, HEADER_INDEX_WIDTH);
            const h = Atomics.load(shared.header, HEADER_INDEX_HEIGHT);
            const stride = Atomics.load(shared.header, HEADER_INDEX_STRIDE_BYTES);
            const frame = Atomics.load(shared.header, HEADER_INDEX_FRAME_COUNTER);
            vgaInfo.textContent = `vga: ${w}x${h} stride=${stride} frame=${frame}`;
            const frameCount = frame >>> 0;
            testState.framesPresented = frameCount;
            testState.width = Math.max(0, w | 0);
            testState.height = Math.max(0, h | 0);
            testState.strideBytes = Math.max(0, stride | 0);
            presenter?.presentLatestFrame();
          }, 50);
            (sharedPollTimer as unknown as { unref?: () => void }).unref?.();
          }
          return;
        }

        // Copy-frame transport: presenter will consume frames via `pushCopyFrame`.
        shared = null;
        presenter.setSharedFramebuffer(null);
        if (sharedPollTimer !== null) {
          window.clearInterval(sharedPollTimer);
          sharedPollTimer = null;
        }
        return;
      }

      if (msg.type === "machineVga.serial") {
        const bytes = msg.data;
        if (bytes instanceof Uint8Array && bytes.byteLength) {
          output.textContent = `${output.textContent ?? ""}${decoder.decode(bytes)}`;
        }
        return;
      }

      if (msg.type === "machineVga.status") {
        const detail = msg.detail;
        if (typeof detail === "string") {
          status.textContent = `Machine worker demo: ${detail}`;
        }
        return;
      }

      if (msg.type === "machineVga.error") {
        const message = typeof msg.message === "string" ? msg.message : String(msg.message);
        error.textContent = message;
        testState.error = message;
        status.textContent = "Machine worker demo: error";
        stop();
        return;
      }

      if (isFramebufferCopyMessageV1(msg)) {
        if (!presenter) return;
        presenter.pushCopyFrame(copyFrameFromMessageV1(msg));
        presenter.presentLatestFrame();
        vgaInfo.textContent = `vga: ${msg.width}x${msg.height} stride=${msg.strideBytes} frame=${msg.frameCounter}`;
        testState.framesPresented = Math.max(testState.framesPresented, msg.frameCounter >>> 0);
        testState.width = msg.width | 0;
        testState.height = msg.height | 0;
        testState.strideBytes = msg.strideBytes | 0;
        return;
      }
    });

    w.addEventListener("error", (ev) => {
      const message = String((ev as ErrorEvent).message ?? "Worker error");
      error.textContent = message;
      testState.error = message;
      status.textContent = "Machine worker demo: error";
      stop();
    });

    w.postMessage({
      type: "machineVga.start",
      message: "Hello from machine_vga.worker\\n",
      ramSizeBytes: 2 * 1024 * 1024,
      ...(typeof window !== "undefined"
        ? (() => {
            const search = new URLSearchParams(window.location.search);
            const out: Record<string, unknown> = {};

            const vbeRaw = search.get("machineWorkerVbe");
            if (vbeRaw) {
              const match = /^(\d+)x(\d+)$/.exec(vbeRaw.trim());
              if (match) {
                const width = Number.parseInt(match[1] ?? "", 10);
                const height = Number.parseInt(match[2] ?? "", 10);
                if (Number.isFinite(width) && Number.isFinite(height) && width > 0 && height > 0) {
                  out.vbeMode = { width, height };
                }
              }
            }

            const enableAerogpu = parseOptionalBoolParam(search, "machineWorkerAerogpu");
            if (enableAerogpu !== undefined) {
              out.enableAerogpu = enableAerogpu;
            }

            const enableVga = parseOptionalBoolParam(search, "machineWorkerVga");
            if (enableVga !== undefined) {
              out.enableVga = enableVga;
            }

            const cpuRaw = search.get("machineWorkerCpuCount");
            if (cpuRaw !== null) {
              const n = Number.parseInt(cpuRaw.trim(), 10);
              if (Number.isFinite(n) && n >= 1 && n <= 255) {
                out.cpuCount = n;
              }
            }

            return out;
          })()
        : {}),
    });
  };

  const startButton = el("button", { id: "canonical-machine-vga-worker-start", text: "Start" }) as HTMLButtonElement;
  const stopButton = el("button", { id: "canonical-machine-vga-worker-stop", text: "Stop" }) as HTMLButtonElement;
  startButton.addEventListener("click", start);
  stopButton.addEventListener("click", stop);

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Machine (canonical VM) – worker display scanout demo" }),
    el("div", {
      class: "hint",
      text:
        "Runs the canonical aero-machine VM inside a Dedicated Worker and publishes display scanout via the framebuffer protocol. " +
        "(Prefers unified display_* exports when present; falls back to legacy vga_*.) " +
        "VBE mode test: add ?machineWorkerVbe=1280x720 to boot the worker into a Bochs VBE 32bpp mode (requires VGA; set ?machineWorkerVga=1 or ?machineWorkerAerogpu=0). " +
        "GPU config test: add ?machineWorkerAerogpu=1 (or =0 to disable) to override the default GPU device model. " +
        "SMP test: add ?machineWorkerCpuCount=2 to request a 2-vCPU machine (requires native SMP support).",
    }),
    el("div", { class: "row" }, startButton, stopButton),
    status,
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
  const testState = {
    ready: false,
    streaming: false,
    error: null as string | null,
  };
  (globalThis as unknown as { __aeroDemoVmSnapshot?: typeof testState }).__aeroDemoVmSnapshot = testState;

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
    const anyVm = current as unknown as Record<string, unknown>;
    const fn = anyVm.serial_output_len ?? anyVm.serialOutputLen;
    if (typeof fn !== "function") return null;
    try {
      const value = (fn as () => unknown).call(current);
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
        const anyVm = current as unknown as Record<string, unknown>;
        const runSteps = anyVm.run_steps ?? anyVm.runSteps;
        if (typeof runSteps !== "function") throw new Error("DemoVm missing run_steps/runSteps export.");
        (runSteps as (steps: number) => void).call(current, STEPS_PER_TICK);
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
        {
          const anyVm = vm as unknown as Record<string, unknown>;
          const restore = anyVm.restore_snapshot ?? anyVm.restoreSnapshot;
          if (typeof restore !== "function") throw new Error("DemoVm missing restore_snapshot/restoreSnapshot export.");
          (restore as (bytes: Uint8Array) => void).call(vm, bytes);
        }
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
        const len = (() => {
          const anyVm = vm as unknown as Record<string, unknown>;
          const serialOutput = anyVm.serial_output ?? anyVm.serialOutput;
          if (typeof serialOutput !== "function") throw new Error("DemoVm missing serial_output/serialOutput export.");
          const bytes = (serialOutput as () => unknown).call(vm);
          if (!(bytes instanceof Uint8Array)) throw new Error("DemoVm serial_output did not return Uint8Array.");
          return bytes.byteLength;
        })();
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
      const bytes = (() => {
        const anyVm = vm as unknown as Record<string, unknown>;
        const snapshotFull = anyVm.snapshot_full ?? anyVm.snapshotFull;
        if (typeof snapshotFull !== "function") throw new Error("DemoVm missing snapshot_full/snapshotFull export.");
        const out = (snapshotFull as () => unknown).call(vm);
        if (!(out instanceof Uint8Array)) throw new Error("DemoVm snapshot_full did not return Uint8Array.");
        return out;
      })();
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
      const anyVm = vm as unknown as Record<string, unknown>;
      const runSteps = anyVm.run_steps ?? anyVm.runSteps;
      if (typeof runSteps !== "function") throw new Error("DemoVm missing run_steps/runSteps export.");
      (runSteps as (steps: number) => void).call(vm, 50_000);
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

    await ensureLegacyOpfsImagesAdopted(manager);

    disks = await manager.listDisks();
    mounts = await manager.getMounts();
    mounts = await maybeApplyLegacyActiveDiskImageMountHint(manager, disks, mounts);

    // Propagate the current DiskManager mount selection to the runtime workers (CPU + IO) via the
    // coordinator. This is the canonical disk selection flow; `activeDiskImage` is deprecated and
    // is no longer used as a "VM mode" toggle.
    //
    // Note: `workerCoordinator.setBootDisks(...)` is safe to call even when workers are not running;
    // it caches the selection so newly spawned workers inherit it.
    //
    // Avoid re-sending identical selections because the legacy IO worker remount path closes and
    // re-opens disk handles (expensive; can also perturb in-flight I/O). The coordinator is
    // responsible for suppressing no-op updates, so we can always forward the latest metadata here
    // (important for cases like resize/remote config changes where the selected disk ID stays the
    // same but fields like size/format change).
    try {
      const byId = new Map(disks.map((d) => [d.id, d]));
      const hdd = mounts.hddId ? byId.get(mounts.hddId) ?? null : null;
      const cd = mounts.cdId ? byId.get(mounts.cdId) ?? null : null;
      workerCoordinator.setBootDisks(mounts, hdd, cd);
    } catch {
      // ignore best-effort runtime sync
    }
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
  const remoteCacheUnbounded = el("input", { type: "checkbox" }) as HTMLInputElement;
  remoteCacheUnbounded.addEventListener("change", () => {
    remoteCacheMiB.disabled = remoteCacheUnbounded.checked;
  });

  const remoteCachesStatus = el("div", { class: "mono muted", text: "" });
  const remoteCachesPruneStatus = el("pre", { class: "mono", text: "" });
  const pruneOlderThanDaysInput = el("input", { type: "number", min: "0", step: "1", value: "30" }) as HTMLInputElement;
  const pruneMaxCachesInput = el("input", { type: "number", min: "0", step: "1", placeholder: "(optional)" }) as HTMLInputElement;
  const pruneDryRunInput = el("input", { type: "checkbox", checked: "true" }) as HTMLInputElement;
  const pruneRemoteCachesBtn = el("button", { text: "Dry-run prune" }) as HTMLButtonElement;
  const remoteCachesTableBody = el("tbody");
  const remoteCachesTable = el(
    "table",
    {},
    el(
      "thead",
      {},
      el(
        "tr",
        {},
        el("th", { text: "Cache key" }),
        el("th", { text: "Cached bytes" }),
        el("th", { text: "Last accessed" }),
        el("th", { text: "Delivery" }),
        el("th", { text: "Image" }),
      ),
    ),
    remoteCachesTableBody,
  );

  let remoteCaches: RemoteCacheStatusSerializable[] = [];
  let remoteCacheCorruptKeys: string[] = [];

  function updatePruneRemoteCachesButtonLabel(): void {
    pruneRemoteCachesBtn.textContent = pruneDryRunInput.checked ? "Dry-run prune" : "Prune now";
  }
  pruneDryRunInput.addEventListener("change", updatePruneRemoteCachesButtonLabel);
  updatePruneRemoteCachesButtonLabel();

  function renderRemoteCachesTable(): void {
    remoteCachesTableBody.replaceChildren();

    if (!manager) {
      remoteCachesStatus.textContent = "";
      remoteCachesTableBody.append(el("tr", {}, el("td", { colspan: "5", class: "muted", text: "Initializing…" })));
      return;
    }

    const totalBytes = remoteCaches.reduce((sum, c) => sum + c.cachedBytes, 0);
    remoteCachesStatus.textContent =
      `Remote caches: ${remoteCaches.length} valid, ${remoteCacheCorruptKeys.length} corrupt` +
      (remoteCaches.length > 0 ? `, ~${formatBytes(totalBytes)} cached` : "");

    if (remoteCaches.length === 0 && remoteCacheCorruptKeys.length === 0) {
      remoteCachesTableBody.append(el("tr", {}, el("td", { colspan: "5", class: "muted", text: "No caches found." })));
      return;
    }

    for (const c of remoteCaches) {
      const last =
        typeof c.lastAccessedAtMs === "number" && Number.isFinite(c.lastAccessedAtMs) && c.lastAccessedAtMs > 0
          ? new Date(c.lastAccessedAtMs).toLocaleString()
          : "unknown";
      const imageLabel = `${c.imageId}@${c.imageVersion}`;
      remoteCachesTableBody.append(
        el(
          "tr",
          {},
          el("td", { class: "mono", title: c.cacheKey, text: c.cacheKey }),
          el("td", { text: formatBytes(c.cachedBytes) }),
          el("td", { class: "mono", text: last }),
          el("td", { class: "mono", text: c.deliveryType }),
          el("td", { class: "mono", title: imageLabel, text: imageLabel }),
        ),
      );
    }

    // Append corrupt entries last.
    for (const key of remoteCacheCorruptKeys) {
      remoteCachesTableBody.append(
        el(
          "tr",
          { class: "missing" },
          el("td", { class: "mono", title: key, text: key }),
          el("td", { class: "muted", text: "unknown" }),
          el("td", { class: "muted", text: "unknown" }),
          el("td", { class: "muted", text: "unknown" }),
          el("td", { class: "muted", text: "(corrupt)" }),
        ),
      );
    }
  }

  function formatRemoteCachePruneResult(
    result: { pruned: number; examined: number; prunedKeys?: string[] },
    dryRun: boolean,
  ): string {
    const header = `${dryRun ? "Dry-run prune" : "Prune"}: pruned=${result.pruned.toLocaleString()} examined=${result.examined.toLocaleString()}`;
    if (!dryRun) return header;

    const keys = Array.isArray(result.prunedKeys) ? result.prunedKeys : [];
    if (keys.length === 0) return `${header}\n(no caches matched)`;

    const maxShown = 50;
    const shown = keys.slice(0, maxShown);
    const remaining = keys.length - shown.length;
    const lines = [header, "prunedKeys:", ...shown];
    if (remaining > 0) lines.push(`…+${remaining} more`);
    return lines.join("\n");
  }

  async function refreshRemoteCaches(): Promise<void> {
    try {
      if (!manager) manager = await diskManagerPromise;
      const res = await manager.listRemoteCaches();
      remoteCaches = res.caches;
      remoteCacheCorruptKeys = res.corruptKeys;
      renderRemoteCachesTable();
    } catch (err) {
      remoteCachesStatus.textContent = `Remote cache list failed: ${err instanceof Error ? err.message : String(err)}`;
      remoteCachesTableBody.replaceChildren(
        el("tr", {}, el("td", { colspan: "5", class: "muted", text: "Failed to list caches." })),
      );
    }
  }

  async function pruneRemoteCachesFromUi(dryRun: boolean): Promise<void> {
    status.textContent = "";
    remoteCachesPruneStatus.textContent = "";

    const olderThanDays = Number(pruneOlderThanDaysInput.value);
    if (!Number.isFinite(olderThanDays) || olderThanDays < 0) {
      const msg = "Invalid olderThanDays (must be a non-negative number).";
      status.textContent = msg;
      remoteCachesPruneStatus.textContent = msg;
      return;
    }

    let maxCaches: number | undefined;
    const rawMax = pruneMaxCachesInput.value.trim();
    if (rawMax) {
      const n = Number(rawMax);
      if (!Number.isFinite(n) || !Number.isInteger(n) || n < 0) {
        const msg = "Invalid maxCaches (must be a non-negative integer).";
        status.textContent = msg;
        remoteCachesPruneStatus.textContent = msg;
        return;
      }
      maxCaches = n;
    }

    if (!manager) manager = await diskManagerPromise;

    pruneRemoteCachesBtn.disabled = true;
    remoteCachesPruneStatus.textContent = dryRun ? "Dry-run prune in progress…" : "Pruning in progress…";
    try {
      const outcome = await pruneRemoteCachesAndRefresh({
        manager,
        olderThanDays,
        maxCaches,
        dryRun,
        refresh: refreshRemoteCaches,
      });
      if (!outcome.supported) {
        remoteCachesPruneStatus.textContent = outcome.message;
        return;
      }
      remoteCachesPruneStatus.textContent = formatRemoteCachePruneResult(outcome.result, dryRun);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      status.textContent = msg;
      remoteCachesPruneStatus.textContent = `Remote cache prune failed: ${msg}`;
    } finally {
      pruneRemoteCachesBtn.disabled = false;
    }
  }

  const refreshRemoteCachesBtn = el("button", {
    text: "Refresh remote caches",
    onclick: () => {
      remoteCachesStatus.textContent = "";
      void refreshRemoteCaches();
    },
  }) as HTMLButtonElement;

  pruneRemoteCachesBtn.addEventListener("click", () => {
    void pruneRemoteCachesFromUi(pruneDryRunInput.checked);
  });

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
      let cacheLimitBytes: number | null | undefined;
      if (remoteCacheUnbounded.checked) {
        cacheLimitBytes = null;
      } else {
        const rawCacheMiB = remoteCacheMiB.value.trim();
        if (rawCacheMiB) {
          const cacheLimitMiB = Number(rawCacheMiB);
          if (!Number.isFinite(cacheLimitMiB) || !Number.isInteger(cacheLimitMiB) || cacheLimitMiB < 0) {
            status.textContent = "Invalid cache size.";
            return;
          }
          const bytes = cacheLimitMiB * 1024 * 1024;
          if (!Number.isSafeInteger(bytes) || bytes < 0) {
            status.textContent = "Invalid cache size.";
            return;
          }
          cacheLimitBytes = bytes;
        } else {
          cacheLimitBytes = undefined;
        }
      }
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
  // `AeroConfigManager.init()` asynchronously loads an optional deployment config file. That config
  // may still include legacy `activeDiskImage` values (deprecated) which we treat as a one-time
  // mount hint. Re-run refresh after init so those hints can be applied even if the first refresh
  // happened before the fetch completed.
  void configInitPromise
    .then(() => refresh())
    .catch(() => {
      // ignore (best-effort)
    });
  // Populate the remote cache table on first render (best-effort).
  void refreshRemoteCaches().catch(() => {});
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
      el("label", { text: "Cache MiB (0=disabled):" }),
      remoteCacheMiB,
      el("label", { text: "Unbounded:" }),
      remoteCacheUnbounded,
      addRemoteBtn,
    ),
    el("h3", { text: "Remote caches" }),
    el("div", { class: "row" }, refreshRemoteCachesBtn),
    el(
      "div",
      { class: "row" },
      el("label", { text: "Older than days:" }),
      pruneOlderThanDaysInput,
      el("label", { text: "Max caches:" }),
      pruneMaxCachesInput,
      el("label", { text: "Dry run:" }),
      pruneDryRunInput,
      pruneRemoteCachesBtn,
    ),
    remoteCachesPruneStatus,
    remoteCachesStatus,
    remoteCachesTable,
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
  type WasmSineTone = InstanceType<NonNullable<WasmApi["SineTone"]>> & { free?: () => void };
  let wasmTone: WasmSineTone | null = null;
  let stopPerfSampling: (() => void) | null = null;

  let loopbackTimer: number | null = null;
  let syntheticMic: { stop(): void } | null = null;
  let hdaDemoWorker: Worker | null = null;
  let hdaDemoStats: { [k: string]: unknown } | null = null;
  let virtioSndDemoWorker: Worker | null = null;
  let virtioSndDemoStats: { [k: string]: unknown } | null = null;

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
    wasmTone?.free?.();
    wasmTone = null;
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
    (globalThis as unknown as { __aeroAudioHdaDemoStats?: unknown }).__aeroAudioHdaDemoStats = undefined;
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  function stopVirtioSndDemo(): void {
    if (!virtioSndDemoWorker) return;
    virtioSndDemoWorker.postMessage({ type: "audioOutputVirtioSndDemo.stop" });
    virtioSndDemoWorker.terminate();
    virtioSndDemoWorker = null;
    virtioSndDemoStats = null;
    (globalThis as unknown as { __aeroAudioVirtioSndDemoStats?: unknown }).__aeroAudioVirtioSndDemoStats = undefined;
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  async function startTone(output: Exclude<Awaited<ReturnType<typeof createAudioOutput>>, { enabled: false }>) {
    stopTone();
    stopHdaDemo();
    stopVirtioSndDemo();

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
    (globalThis as unknown as { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "js";

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

        const bridge = api.attach_worklet_bridge(output.ringBuffer.buffer, output.ringBuffer.capacityFrames, channelCount);
        const tone = new api.SineTone();
        wasmBridge = bridge;
        wasmTone = tone;

        writeTone = (frames: number) => {
          tone.write(bridge, frames, freqHz, sr, gain);
        };

        (globalThis as unknown as { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "wasm";
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
        `baseLatencySeconds: ${metrics.baseLatencySeconds ?? "n/a"}\n` +
        `outputLatencySeconds: ${metrics.outputLatencySeconds ?? "n/a"}\n` +
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
      stopVirtioSndDemo();
      const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
      // Expose for Playwright smoke tests.
      (globalThis as unknown as { __aeroAudioOutput?: unknown }).__aeroAudioOutput = output;
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
      stopVirtioSndDemo();

      try {
        // Ensure the static config (if any) has been loaded before starting the
        // worker harness. Otherwise, `AeroConfigManager.init()` may emit an update
        // after we start workers and trigger an avoidable worker restart.
        await configInitPromise;
        const base = configManager.getState().effective;
        // This audio-only debug path does not need a large guest RAM allocation or a VRAM aperture.
        // Keep allocations tiny so dev/harness pages don't reserve hundreds of MiB per tab.
        //
        // Note: the worker runtime always reserves a fixed wasm32 runtime region (see
        // `web/src/runtime/shared_layout.ts`), so keeping `guestMemoryMiB` at the minimum
        // still has a meaningful impact on total SharedArrayBuffer pressure in CI.
        workerCoordinator.start({ ...base, guestMemoryMiB: 1, vramMiB: 0 });
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
      (globalThis as unknown as { __aeroAudioOutputWorker?: unknown }).__aeroAudioOutputWorker = output;
      (globalThis as unknown as { __aeroAudioToneBackendWorker?: unknown }).__aeroAudioToneBackendWorker = "cpu-worker-wasm";
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
          `baseLatencySeconds: ${metrics.baseLatencySeconds ?? "n/a"}\n` +
          `outputLatencySeconds: ${metrics.outputLatencySeconds ?? "n/a"}\n` +
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
      stopVirtioSndDemo();

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
      (globalThis as unknown as { __aeroAudioOutputHdaDemo?: unknown }).__aeroAudioOutputHdaDemo = output;
      // Back-compat: older tests/debug helpers look for `__aeroAudioOutput`.
      (globalThis as unknown as { __aeroAudioOutput?: unknown }).__aeroAudioOutput = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      (globalThis as unknown as { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "wasm-hda";

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
        (globalThis as unknown as { __aeroAudioHdaDemoStats?: unknown }).__aeroAudioHdaDemoStats = hdaDemoStats;
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
          `baseLatencySeconds: ${metrics.baseLatencySeconds ?? "n/a"}\n` +
          `outputLatencySeconds: ${metrics.outputLatencySeconds ?? "n/a"}\n` +
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

  const virtioSndDemoButton = el("button", {
    id: "init-audio-virtio-snd-demo",
    text: "Init audio output (virtio-snd demo)",
    onclick: async () => {
      status.textContent = "";
      stopTone();
      stopLoopback();
      stopHdaDemo();
      stopVirtioSndDemo();

      // Best-effort: ensure this WASM build includes the virtio-snd demo wrapper.
      try {
        const { api } = await wasmInitPromise;
        if (typeof api.VirtioSndPlaybackDemo !== "function") {
          status.textContent =
            "virtio-snd demo is unavailable in this WASM build (missing VirtioSndPlaybackDemo export).";
          return;
        }
      } catch {
        status.textContent = "virtio-snd demo is unavailable (WASM init failed).";
        return;
      }

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        // Match the HDA demo buffering (~340ms @ 48k).
        ringBufferFrames: 16_384,
      });
      // Expose for Playwright smoke tests / e2e assertions.
      (globalThis as unknown as { __aeroAudioOutputVirtioSndDemo?: unknown }).__aeroAudioOutputVirtioSndDemo = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      (globalThis as unknown as { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "wasm-virtio-snd";

      // Prefill the ring with silence so the worker has time to attach and start producing audio
      // without incurring startup underruns.
      const level = output.getBufferLevelFrames();
      const prefillFrames = Math.max(0, output.ringBuffer.capacityFrames - level);
      if (prefillFrames > 0) {
        Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
      }

      virtioSndDemoWorker = new Worker(new URL("./workers/cpu.worker.ts", import.meta.url), { type: "module" });
      virtioSndDemoWorker.addEventListener("message", (ev: MessageEvent<unknown>) => {
        const msg = ev.data as { type?: unknown } | null;
        if (!msg || msg.type !== "audioOutputVirtioSndDemo.stats") return;
        virtioSndDemoStats = msg as { [k: string]: unknown };
        // Expose for Playwright/debugging.
        (globalThis as unknown as { __aeroAudioVirtioSndDemoStats?: unknown }).__aeroAudioVirtioSndDemoStats =
          virtioSndDemoStats;
      });

      const workerReady = new Promise<void>((resolve, reject) => {
        const worker = virtioSndDemoWorker;
        if (!worker) {
          reject(new Error("Missing virtio-snd demo worker"));
          return;
        }

        const timeoutMs = 45_000;
        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data as { type?: unknown; message?: unknown } | null | undefined;
          if (!data || typeof data !== "object") return;
          if (data.type === "audioOutputVirtioSndDemo.ready") {
            cleanup();
            resolve();
          } else if (data.type === "audioOutputVirtioSndDemo.error") {
            cleanup();
            reject(new Error(typeof data.message === "string" ? data.message : "virtio-snd demo worker error"));
          }
        };
        const onError = (ev: ErrorEvent) => {
          cleanup();
          reject(new Error(ev.message || "virtio-snd demo worker error"));
        };

        const timer = window.setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for virtio-snd demo worker init (${timeoutMs}ms).`));
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

      virtioSndDemoWorker.postMessage({
        type: "audioOutputVirtioSndDemo.start",
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
        stopVirtioSndDemo();
        return;
      }

      await output.resume();
      status.textContent = "Audio initialized and virtio-snd playback demo started in CPU worker.";
      const timer = window.setInterval(() => {
        const metrics = output.getMetrics();
        const read = Atomics.load(output.ringBuffer.readIndex, 0) >>> 0;
        const write = Atomics.load(output.ringBuffer.writeIndex, 0) >>> 0;
        const demoStats = virtioSndDemoStats;
        const demoLines: string[] = [];
        if (demoStats) {
          const t = demoStats["targetFrames"];
          const lvl = demoStats["bufferLevelFrames"];
          if (typeof t === "number") demoLines.push(`worker.targetFrames: ${t}`);
          if (typeof lvl === "number") demoLines.push(`worker.bufferLevelFrames: ${lvl}`);
          const totalWritten = demoStats["totalFramesWritten"];
          if (typeof totalWritten === "number") demoLines.push(`virtioSnd.totalFramesWritten: ${totalWritten}`);
          const totalDropped = demoStats["totalFramesDropped"];
          if (typeof totalDropped === "number") demoLines.push(`virtioSnd.totalFramesDropped: ${totalDropped}`);
        }
        status.textContent =
          `AudioContext: ${metrics.state}\n` +
          `sampleRate: ${metrics.sampleRate}\n` +
          `baseLatencySeconds: ${metrics.baseLatencySeconds ?? "n/a"}\n` +
          `outputLatencySeconds: ${metrics.outputLatencySeconds ?? "n/a"}\n` +
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
      stopVirtioSndDemo();

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: 16_384, // ~340ms @ 48k; target buffering stays ~200ms.
      });
      // Expose for Playwright.
      (globalThis as unknown as { __aeroAudioOutputLoopback?: unknown }).__aeroAudioOutputLoopback = output;
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
      (globalThis as unknown as { __aeroSyntheticMic?: unknown }).__aeroSyntheticMic = mic;

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
        // This debug path does not need a full guest RAM allocation or VRAM aperture; keep it
        // tiny so dev/harness pages don't reserve hundreds of MiB per tab.
        workerCoordinator.start({
          ...base,
          enableWorkers: true,
          guestMemoryMiB: 1,
          vramMiB: 0,
        });

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

      (globalThis as unknown as { __aeroAudioLoopbackBackend?: unknown }).__aeroAudioLoopbackBackend = backend;

      await output.resume();
      status.textContent = workerError
        ? `Audio loopback initialized (backend=${backend}). Worker init failed: ${workerError}`
        : `Audio loopback initialized (backend=${backend}).`;
    },
  });

  function getBuildInfoForExport(): { version: string; gitSha: string; builtAt: string } {
    // eslint-disable-next-line no-undef
    return typeof __AERO_BUILD_INFO__ !== "undefined"
      ? // eslint-disable-next-line no-undef
        __AERO_BUILD_INFO__
      : { version: "dev", gitSha: "unknown", builtAt: "unknown" };
  }

  function snapshotHostEnvForExport(): Record<string, unknown> {
    const navAny = navigator as unknown as {
      userAgentData?: {
        platform?: unknown;
        mobile?: unknown;
        brands?: unknown;
      };
      deviceMemory?: unknown;
      platform?: unknown;
      hardwareConcurrency?: unknown;
    };
    const userAgentData = navAny.userAgentData
      ? {
          platform: typeof navAny.userAgentData.platform === "string" ? navAny.userAgentData.platform : null,
          mobile: typeof navAny.userAgentData.mobile === "boolean" ? navAny.userAgentData.mobile : null,
          brands: Array.isArray(navAny.userAgentData.brands) ? navAny.userAgentData.brands : null,
        }
      : null;
    const deviceMemoryGiB = typeof navAny.deviceMemory === "number" && Number.isFinite(navAny.deviceMemory) ? navAny.deviceMemory : null;
    const platform = typeof navAny.platform === "string" ? navAny.platform : null;
    const hardwareConcurrency =
      typeof navAny.hardwareConcurrency === "number" && Number.isFinite(navAny.hardwareConcurrency) ? navAny.hardwareConcurrency : null;
    return {
      userAgent: navigator.userAgent,
      userAgentData,
      platform,
      hardwareConcurrency,
      deviceMemoryGiB,
      isSecureContext: typeof isSecureContext === "boolean" ? isSecureContext : false,
      crossOriginIsolated: typeof crossOriginIsolated === "boolean" ? crossOriginIsolated : false,
      location: {
        origin: typeof location?.origin === "string" ? location.origin : null,
        pathname: typeof location?.pathname === "string" ? location.pathname : null,
      },
    };
  }

  async function snapshotHostMediaDevicesForExport(): Promise<Record<string, unknown>> {
    const encoder = new TextEncoder();
    try {
      const media = navigator.mediaDevices;
      if (!media) throw new Error("navigator.mediaDevices is unavailable.");

      const supportedConstraints = typeof media.getSupportedConstraints === "function" ? media.getSupportedConstraints() : null;

      let microphonePermissionState: string | null = null;
      try {
        // Permissions API is not available in all browsers. Best-effort only.
        const navAny = navigator as unknown as { permissions?: { query?: (desc: { name: string }) => Promise<unknown> } };
        const status = await navAny.permissions?.query?.({ name: "microphone" });
        const state = (status as { state?: unknown } | null)?.state;
        if (typeof state === "string") microphonePermissionState = state;
      } catch {
        // ignore
      }

      if (typeof media.enumerateDevices !== "function") {
        throw new Error("navigator.mediaDevices.enumerateDevices is unavailable.");
      }
      const devices = await media.enumerateDevices();

      return {
        ok: true,
        microphonePermissionState,
        supportedConstraints,
        devices: devices.map((d) => ({
          kind: d.kind,
          label: d.label,
          deviceIdHash: d.deviceId ? fnv1a32Hex(encoder.encode(d.deviceId)) : "",
          groupIdHash: d.groupId ? fnv1a32Hex(encoder.encode(d.groupId)) : "",
        })),
      };
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      return { ok: false, error: message };
    }
  }

  function snapshotAudioOutput(out: unknown): unknown {
    if (!out || (typeof out !== "object" && typeof out !== "function")) return null;
    const o = out as Record<string, unknown>;
    const getMetrics = o.getMetrics;
    const metrics = typeof getMetrics === "function" ? (getMetrics as () => unknown).call(out) : null;
    const ringRaw = o.ringBuffer;
    const ring = ringRaw && (typeof ringRaw === "object" || typeof ringRaw === "function") ? (ringRaw as Record<string, unknown>) : null;
    let ringCounters: unknown = null;
    if (ring) {
      const readIndex = ring.readIndex;
      const writeIndex = ring.writeIndex;
      const underrunCount = ring.underrunCount;
      const overrunCount = ring.overrunCount;
      try {
        if (
          readIndex instanceof Uint32Array &&
          writeIndex instanceof Uint32Array &&
          underrunCount instanceof Uint32Array &&
          overrunCount instanceof Uint32Array
        ) {
          ringCounters = {
            readFrameIndex: Atomics.load(readIndex, 0) >>> 0,
            writeFrameIndex: Atomics.load(writeIndex, 0) >>> 0,
            underrunCount: Atomics.load(underrunCount, 0) >>> 0,
            overrunCount: Atomics.load(overrunCount, 0) >>> 0,
          };
        }
      } catch {
        // ignore; best-effort only.
      }
    }
    return {
      enabled: typeof o.enabled === "boolean" ? o.enabled : null,
      message: typeof o.message === "string" ? o.message : null,
      metrics,
      ring: ring
        ? {
            channelCount: typeof ring.channelCount === "number" ? ring.channelCount : null,
            capacityFrames: typeof ring.capacityFrames === "number" ? ring.capacityFrames : null,
            counters: ringCounters,
          }
        : null,
    };
  }

  type AudioOutputWavSnapshotMeta = {
    sampleRate: number;
    audioContextState: string | null;
    baseLatencySeconds: number | null;
    outputLatencySeconds: number | null;
    channelCount: number;
    capacityFrames: number;
    readFrameIndex: number;
    writeFrameIndex: number;
    underrunCount: number;
    overrunCount: number;
    availableFrames: number;
    framesCaptured: number;
    signal: AudioSignalStats;
  };

  type MicWavSnapshotMeta = {
    sampleRate: number;
    deviceIdHash: string | null;
    backend: "worklet" | "script" | null;
    audioContextState: string | null;
    workletInitError: string | null;
    trackLabel: string | null;
    trackEnabled: boolean | null;
    trackMuted: boolean | null;
    trackReadyState: string | null;
    trackSettings: Record<string, unknown> | null;
    trackConstraints: Record<string, unknown> | null;
    trackCapabilities: Record<string, unknown> | null;
    bufferMs: number | null;
    echoCancellation: boolean | null;
    noiseSuppression: boolean | null;
    autoGainControl: boolean | null;
    muted: boolean | null;
    capacitySamples: number;
    readPos: number;
    writePos: number;
    availableSamples: number;
    samplesCaptured: number;
    droppedSamples: number;
    signal: AudioSignalStats;
  };

  type AudioSignalStats = {
    /**
     * Peak absolute sample value (max |s|), overall across all channels.
     *
     * Typically in `[0, 1]` for unclipped Web Audio float samples.
     */
    peakAbs: number;
    /**
     * RMS sample value, overall across all channels.
     *
     * Typically in `[0, 1]` for unclipped Web Audio float samples.
     */
    rms: number;
    /**
     * Mean sample value, overall across all channels.
     *
     * Non-zero DC offsets can indicate bugs in mixing/resampling or bias in the input signal.
     */
    dcOffset: number;
    peakAbsPerChannel: number[];
    rmsPerChannel: number[];
    dcOffsetPerChannel: number[];
  };

  function computeAudioSignalStats(interleaved: Float32Array, channelCount: number): AudioSignalStats {
    const cc = channelCount >>> 0;
    if (cc === 0) {
      return {
        peakAbs: 0,
        rms: 0,
        dcOffset: 0,
        peakAbsPerChannel: [],
        rmsPerChannel: [],
        dcOffsetPerChannel: [],
      };
    }
    const frames = Math.floor(interleaved.length / cc);
    if (frames <= 0) {
      const zeros = Array.from({ length: cc }, () => 0);
      return {
        peakAbs: 0,
        rms: 0,
        dcOffset: 0,
        peakAbsPerChannel: zeros,
        rmsPerChannel: zeros,
        dcOffsetPerChannel: zeros,
      };
    }

    const sum = new Float64Array(cc);
    const sumSq = new Float64Array(cc);
    const peak = new Float64Array(cc);
    let totalSum = 0;
    let totalSumSq = 0;
    let totalPeak = 0;

    for (let i = 0; i < interleaved.length; i += cc) {
      for (let c = 0; c < cc; c += 1) {
        let s = interleaved[i + c] ?? 0;
        if (!Number.isFinite(s)) s = 0;
        const abs = Math.abs(s);
        if (abs > peak[c]) peak[c] = abs;
        if (abs > totalPeak) totalPeak = abs;
        sum[c] += s;
        sumSq[c] += s * s;
        totalSum += s;
        totalSumSq += s * s;
      }
    }

    const denomPerChannel = frames;
    const denomTotal = frames * cc;
    const peakAbsPerChannel = Array.from(peak, (v) => (Number.isFinite(v) ? v : 0));
    const rmsPerChannel = Array.from(sumSq, (v) => {
      const ms = v / denomPerChannel;
      return Number.isFinite(ms) && ms > 0 ? Math.sqrt(ms) : 0;
    });
    const dcOffsetPerChannel = Array.from(sum, (v) => {
      const mean = v / denomPerChannel;
      return Number.isFinite(mean) ? mean : 0;
    });

    const totalRms = (() => {
      const ms = totalSumSq / denomTotal;
      return Number.isFinite(ms) && ms > 0 ? Math.sqrt(ms) : 0;
    })();
    const totalDc = (() => {
      const mean = totalSum / denomTotal;
      return Number.isFinite(mean) ? mean : 0;
    })();

    return {
      peakAbs: Number.isFinite(totalPeak) ? totalPeak : 0,
      rms: totalRms,
      dcOffset: totalDc,
      peakAbsPerChannel,
      rmsPerChannel,
      dcOffsetPerChannel,
    };
  }

  function formatDbfsFromLinear(value: number): string {
    const v = typeof value === "number" && Number.isFinite(value) ? value : 0;
    if (v <= 0) return "-inf";
    return (20 * Math.log10(v)).toFixed(1);
  }

  function formatMsOrNa(valueSeconds: number | null): string {
    if (typeof valueSeconds !== "number" || !Number.isFinite(valueSeconds)) return "n/a";
    return (valueSeconds * 1000).toFixed(1);
  }

  function snapshotAudioOutputWav(
    out: unknown,
    opts: { maxSeconds: number },
  ):
    | { ok: true; wav: Uint8Array; meta: AudioOutputWavSnapshotMeta }
    | { ok: false; error: string; context?: Record<string, unknown> } {
    if (!out || (typeof out !== "object" && typeof out !== "function")) return { ok: false, error: "Audio output missing." };
    const o = out as Record<string, unknown>;
    const ringRaw = o.ringBuffer;
    const ring = ringRaw && (typeof ringRaw === "object" || typeof ringRaw === "function") ? (ringRaw as Record<string, unknown>) : null;
    if (!ring) return { ok: false, error: "Audio output has no ringBuffer." };
    const samples = ring.samples;
    if (!(samples instanceof Float32Array)) return { ok: false, error: "Audio output ringBuffer.samples missing." };
    const readIndexRaw = ring.readIndex;
    const writeIndexRaw = ring.writeIndex;
    if (!(readIndexRaw instanceof Uint32Array) || !(writeIndexRaw instanceof Uint32Array)) {
      return { ok: false, error: "Audio output ringBuffer indices missing." };
    }
    const cc = typeof ring.channelCount === "number" ? ring.channelCount >>> 0 : 0;
    const cap = typeof ring.capacityFrames === "number" ? ring.capacityFrames >>> 0 : 0;
    if (cc === 0 || cap === 0) return { ok: false, error: "Audio output ringBuffer has invalid channelCount/capacityFrames." };

    const getMetrics = o.getMetrics;
    const metrics = (typeof getMetrics === "function" ? (getMetrics as () => unknown).call(out) : null) as AudioOutputMetrics | null;
    const ctx = o.context as { sampleRate?: unknown; state?: unknown } | undefined;
    const sampleRate =
      typeof metrics?.sampleRate === "number"
        ? metrics.sampleRate
        : typeof ctx?.sampleRate === "number"
          ? ctx.sampleRate
          : 0;
    if (!Number.isFinite(sampleRate) || sampleRate <= 0) return { ok: false, error: "Audio output sample rate unavailable." };

    const audioContextState =
      typeof metrics?.state === "string"
        ? metrics.state
        : typeof ctx?.state === "string"
          ? ctx.state
          : null;
    const baseLatencySeconds = typeof metrics?.baseLatencySeconds === "number" && Number.isFinite(metrics.baseLatencySeconds) ? metrics.baseLatencySeconds : null;
    const outputLatencySeconds =
      typeof metrics?.outputLatencySeconds === "number" && Number.isFinite(metrics.outputLatencySeconds) ? metrics.outputLatencySeconds : null;

    const readIndex = readIndexRaw;
    const writeIndex = writeIndexRaw;
    let read: number;
    let write: number;
    try {
      read = Atomics.load(readIndex, 0) >>> 0;
      write = Atomics.load(writeIndex, 0) >>> 0;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      return { ok: false, error: `Failed to read audio output ring indices: ${message}` };
    }
    let available = (write - read) >>> 0;
    if (available > cap) available = cap;

    let underrunCount = 0;
    let overrunCount = 0;
    try {
      if (ring.underrunCount instanceof Uint32Array) {
        const view = ring.underrunCount as Uint32Array;
        underrunCount = Atomics.load(view, 0) >>> 0;
      }
      if (ring.overrunCount instanceof Uint32Array) {
        const view = ring.overrunCount as Uint32Array;
        overrunCount = Atomics.load(view, 0) >>> 0;
      }
    } catch {
      // ignore; best-effort only.
    }

    if (available === 0) {
      return {
        ok: false,
        error: `Audio output ringBuffer is empty (read=${read} write=${write} cap=${cap} ctx=${audioContextState ?? "n/a"}).`,
        context: {
          sampleRate,
          audioContextState,
          baseLatencySeconds,
          outputLatencySeconds,
          channelCount: cc,
          capacityFrames: cap,
          readFrameIndex: read,
          writeFrameIndex: write,
          availableFrames: available,
          underrunCount,
          overrunCount,
        },
      };
    }

    const maxFrames = Math.max(1, Math.floor(sampleRate * Math.max(0.1, opts.maxSeconds)));
    const frames = Math.min(available, maxFrames);
    const start = read % cap;
    const firstFrames = Math.min(frames, cap - start);
    const secondFrames = frames - firstFrames;

    const interleaved = new Float32Array(frames * cc);
    interleaved.set(samples.subarray(start * cc, (start + firstFrames) * cc), 0);
    if (secondFrames > 0) {
      interleaved.set(samples.subarray(0, secondFrames * cc), firstFrames * cc);
    }

    const wav = encodeWavPcm16(interleaved, sampleRate, cc);
    const signal = computeAudioSignalStats(interleaved, cc);
    const meta: AudioOutputWavSnapshotMeta = {
      sampleRate,
      audioContextState,
      baseLatencySeconds,
      outputLatencySeconds,
      channelCount: cc,
      capacityFrames: cap,
      readFrameIndex: read,
      writeFrameIndex: write,
      underrunCount,
      overrunCount,
      availableFrames: available,
      framesCaptured: frames,
      signal,
    };
    return { ok: true, wav, meta };
  }

  function snapshotMicAttachment(): unknown {
    const att = micAttachment;
    if (!att) return null;
    const sab = att.ringBuffer;
    if (!(sab instanceof SharedArrayBuffer)) return { sampleRate: att.sampleRate, error: "Invalid mic ring buffer type." };
    try {
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);
      const capacitySamples = Atomics.load(header, MIC_CAPACITY_SAMPLES_INDEX) >>> 0;
      const readPos = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
      const writePos = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;
      const droppedSamples = Atomics.load(header, MIC_DROPPED_SAMPLES_INDEX) >>> 0;
      const available = (writePos - readPos) >>> 0;
      const bufferedSamples = Math.min(available, capacitySamples);
      return {
        sampleRate: att.sampleRate,
        deviceIdHash: att.deviceIdHash,
        backend: att.backend,
        audioContextState: att.audioContextState,
        workletInitError: att.workletInitError,
        trackLabel: att.trackLabel,
        trackEnabled: att.trackEnabled,
        trackMuted: att.trackMuted,
        trackReadyState: att.trackReadyState,
        trackSettings: att.trackSettings,
        trackConstraints: att.trackConstraints,
        trackCapabilities: att.trackCapabilities,
        bufferMs: att.bufferMs,
        echoCancellation: att.echoCancellation,
        noiseSuppression: att.noiseSuppression,
        autoGainControl: att.autoGainControl,
        muted: att.muted,
        capacitySamples,
        readPos,
        writePos,
        bufferedSamples,
        droppedSamples,
      };
    } catch {
      return { sampleRate: att.sampleRate, error: "Failed to snapshot microphone ring counters." };
    }
  }

  function snapshotMicAttachmentWav(opts: {
    maxSeconds: number;
  }): { ok: true; wav: Uint8Array; meta: MicWavSnapshotMeta } | { ok: false; error: string; context?: Record<string, unknown> } {
    const att = micAttachment;
    if (!att) return { ok: false, error: "Microphone capture is not active." };
    const sab = att.ringBuffer;
    if (!(sab instanceof SharedArrayBuffer)) return { ok: false, error: "Invalid mic ring buffer type." };
    const sampleRate = att.sampleRate;
    if (!Number.isFinite(sampleRate) || sampleRate <= 0) return { ok: false, error: "Microphone sample rate unavailable." };

    try {
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);
      const capacitySamples = Atomics.load(header, MIC_CAPACITY_SAMPLES_INDEX) >>> 0;
      const readPos = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
      const writePos = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;
      const droppedSamples = Atomics.load(header, MIC_DROPPED_SAMPLES_INDEX) >>> 0;

      let available = (writePos - readPos) >>> 0;
      if (available > capacitySamples) available = capacitySamples;
      if (available === 0) {
        return {
          ok: false,
          error: `Microphone ring buffer is empty (readPos=${readPos} writePos=${writePos} cap=${capacitySamples} dropped=${droppedSamples}).`,
          context: {
            sampleRate,
            capacitySamples,
            readPos,
            writePos,
            availableSamples: available,
            droppedSamples,
            backend: att.backend,
            audioContextState: att.audioContextState,
            muted: att.muted,
          },
        };
      }

      const data = new Float32Array(sab, MIC_HEADER_BYTES, capacitySamples);
      const maxSamples = Math.max(1, Math.floor(sampleRate * Math.max(0.1, opts.maxSeconds)));
      const toRead = Math.min(available, maxSamples);
      const start = readPos % capacitySamples;
      const first = Math.min(toRead, capacitySamples - start);
      const second = toRead - first;
      const mono = new Float32Array(toRead);
      mono.set(data.subarray(start, start + first), 0);
      if (second > 0) mono.set(data.subarray(0, second), first);

      const wav = encodeWavPcm16(mono, sampleRate, 1);
      const signal = computeAudioSignalStats(mono, 1);
      const meta: MicWavSnapshotMeta = {
        sampleRate,
        deviceIdHash: att.deviceIdHash,
        backend: att.backend,
        audioContextState: att.audioContextState,
        workletInitError: att.workletInitError,
        trackLabel: att.trackLabel,
        trackEnabled: att.trackEnabled,
        trackMuted: att.trackMuted,
        trackReadyState: att.trackReadyState,
        trackSettings: att.trackSettings,
        trackConstraints: att.trackConstraints,
        trackCapabilities: att.trackCapabilities,
        bufferMs: att.bufferMs,
        echoCancellation: att.echoCancellation,
        noiseSuppression: att.noiseSuppression,
        autoGainControl: att.autoGainControl,
        muted: att.muted,
        capacitySamples,
        readPos,
        writePos,
        availableSamples: available,
        samplesCaptured: toRead,
        droppedSamples,
        signal,
      };
      return { ok: true, wav, meta };
    } catch (err) {
      return { ok: false, error: err instanceof Error ? err.message : String(err) };
    }
  }

  function snapshotWorkerCoordinator(): unknown {
    try {
      const wasm = {
        cpu: workerCoordinator.getWorkerWasmStatus("cpu") ?? null,
        gpu: workerCoordinator.getWorkerWasmStatus("gpu") ?? null,
        io: workerCoordinator.getWorkerWasmStatus("io") ?? null,
        jit: workerCoordinator.getWorkerWasmStatus("jit") ?? null,
        net: workerCoordinator.getWorkerWasmStatus("net") ?? null,
      };
      const serialOutputBytes = workerCoordinator.getSerialOutputBytes();
      const serialText = workerCoordinator.getSerialOutputText();
      const SERIAL_TAIL_MAX_CHARS = 8192;
      const serialOutputTruncated = serialText.length > SERIAL_TAIL_MAX_CHARS;
      const serialOutputTail = serialOutputTruncated ? serialText.slice(-SERIAL_TAIL_MAX_CHARS) : serialText;
      return {
        vmState: workerCoordinator.getVmState(),
        statuses: workerCoordinator.getWorkerStatuses(),
        wasm,
        configVersion: workerCoordinator.getConfigVersion(),
        configAckVersions: workerCoordinator.getWorkerConfigAckVersions(),
        heartbeatCounter: workerCoordinator.getHeartbeatCounter(),
        lastHeartbeatFromRing: workerCoordinator.getLastHeartbeatFromRing(),
        serialOutputBytes,
        serialOutputTruncated,
        serialOutputTail,
        lastFatal: workerCoordinator.getLastFatalEvent(),
        lastNonFatal: workerCoordinator.getLastNonFatalEvent(),
        pendingFullRestart: workerCoordinator.getPendingFullRestart(),
      };
    } catch (err) {
      return { error: err instanceof Error ? err.message : String(err) };
    }
  }

  function snapshotConfigForExport(): Record<string, unknown> {
    try {
      const cfg = configManager.getState();
      const effective = { ...cfg.effective } as Record<string, unknown>;
      if (typeof effective.l2TunnelToken === "string" && effective.l2TunnelToken.length) {
        effective.l2TunnelToken = "<redacted>";
      }
      return {
        effective,
        lockedKeys: Array.from(cfg.lockedKeys),
        issues: cfg.issues,
        capabilities: cfg.capabilities,
      };
    } catch (err) {
      return { error: err instanceof Error ? err.message : String(err) };
    }
  }

  function snapshotRingBufferOwnersForExport(): unknown {
    try {
      return {
        audioOutput: {
          effective: workerCoordinator.getAudioRingBufferOwner(),
          override: workerCoordinator.getAudioRingBufferOwnerOverride(),
          default: workerCoordinator.getAudioRingBufferOwnerDefault(),
        },
        microphone: {
          effective: workerCoordinator.getMicrophoneRingBufferOwner(),
          override: workerCoordinator.getMicrophoneRingBufferOwnerOverride(),
          default: workerCoordinator.getMicrophoneRingBufferOwnerDefault(),
        },
      };
    } catch (err) {
      return { error: err instanceof Error ? err.message : String(err) };
    }
  }

  function snapshotRingBuffersForExport(): unknown {
    try {
      return workerCoordinator.getRingBufferAttachmentSnapshot();
    } catch (err) {
      return { error: err instanceof Error ? err.message : String(err) };
    }
  }

  function snapshotWorkerProducerForExport(): unknown {
    try {
      return {
        bufferLevelFrames: workerCoordinator.getAudioProducerBufferLevelFrames(),
        underrunCount: workerCoordinator.getAudioProducerUnderrunCount(),
        overrunCount: workerCoordinator.getAudioProducerOverrunCount(),
      };
    } catch (err) {
      return { error: err instanceof Error ? err.message : String(err) };
    }
  }

  const exportMetricsButton = el("button", {
    text: "Export audio metrics (json)",
    onclick: async () => {
      status.textContent = "";
      try {
        const g = globalThis as unknown as {
          __aeroAudioOutput?: unknown;
          __aeroAudioOutputWorker?: unknown;
          __aeroAudioOutputHdaDemo?: unknown;
          __aeroAudioOutputVirtioSndDemo?: unknown;
          __aeroAudioOutputLoopback?: unknown;
        };

        const timeIso = new Date().toISOString();
        const ts = timeIso.replaceAll(":", "-").replaceAll(".", "-");

        const hostMediaDevices = await snapshotHostMediaDevicesForExport();

        const report = {
          timeIso,
          build: getBuildInfoForExport(),
          userAgent: navigator.userAgent,
          crossOriginIsolated: typeof crossOriginIsolated === "boolean" ? crossOriginIsolated : false,
          host: snapshotHostEnvForExport(),
          hostMediaDevices,
          ringBufferOwners: snapshotRingBufferOwnersForExport(),
          ringBuffers: snapshotRingBuffersForExport(),
          // Include all known audio outputs so QA can tell which ring was actually active.
          audioOutputs: {
            __aeroAudioOutput: snapshotAudioOutput(g.__aeroAudioOutput),
            __aeroAudioOutputWorker: snapshotAudioOutput(g.__aeroAudioOutputWorker),
            __aeroAudioOutputHdaDemo: snapshotAudioOutput(g.__aeroAudioOutputHdaDemo),
            __aeroAudioOutputVirtioSndDemo: snapshotAudioOutput(g.__aeroAudioOutputVirtioSndDemo),
            __aeroAudioOutputLoopback: snapshotAudioOutput(g.__aeroAudioOutputLoopback),
          },
          workerProducer: snapshotWorkerProducerForExport(),
          microphone: snapshotMicAttachment(),
          workers: snapshotWorkerCoordinator(),
          config: snapshotConfigForExport(),
        };

        downloadJson(report, `aero-audio-metrics-${ts}.json`);
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  type HdaCodecDebugStateResultMessage = {
    type: "hda.codecDebugStateResult";
    requestId: number;
    ok: boolean;
    state?: unknown;
    error?: string;
  };

  type HdaSnapshotStateResultMessage = {
    type: "hda.snapshotStateResult";
    requestId: number;
    ok: boolean;
    bytes?: Uint8Array;
    error?: string;
  };

  type HdaTickStatsResultMessage = {
    type: "hda.tickStatsResult";
    requestId: number;
    ok: boolean;
    stats?: { tickClampEvents: number; tickClampedFramesTotal: number; tickDroppedFramesTotal: number };
    error?: string;
  };

  type VirtioSndSnapshotStateResultMessage = {
    type: "virtioSnd.snapshotStateResult";
    requestId: number;
    ok: boolean;
    bytes?: Uint8Array;
    error?: string;
  };

  let hdaCodecDebugRequestId = 1;
  let hdaSnapshotStateRequestId = 1;
  let hdaTickStatsRequestId = 1;
  let virtioSndSnapshotStateRequestId = 1;
  let qaBundleScreenshotRequestId = 1;
  const exportHdaCodecStateButton = el("button", {
    text: "Export HDA codec state (json)",
    onclick: async () => {
      status.textContent = "";
      try {
        const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
        if (vmRuntime === "machine") {
          throw new Error("HDA codec state export is unavailable in vmRuntime=machine.");
        }
        const ioWorker = workerCoordinator.getIoWorker();
        if (!ioWorker) {
          throw new Error("I/O worker is not running. Start workers before exporting HDA codec state.");
        }

        const requestId = hdaCodecDebugRequestId++;
        const response = await new Promise<HdaCodecDebugStateResultMessage>((resolve, reject) => {
          const timeout = window.setTimeout(() => {
            cleanup();
            reject(new Error("Timed out waiting for IO worker HDA codec debug state response."));
          }, 3000);
          (timeout as unknown as { unref?: () => void }).unref?.();

          const onMessage = (ev: MessageEvent<unknown>) => {
            const msg = ev.data as Partial<HdaCodecDebugStateResultMessage> | null;
            if (!msg || msg.type !== "hda.codecDebugStateResult") return;
            if (msg.requestId !== requestId) return;
            cleanup();
            resolve(msg as HdaCodecDebugStateResultMessage);
          };

          const cleanup = () => {
            window.clearTimeout(timeout);
            ioWorker.removeEventListener("message", onMessage);
          };

          ioWorker.addEventListener("message", onMessage);
          ioWorker.postMessage({ type: "hda.codecDebugState", requestId });
        });

        if (!response.ok) {
          throw new Error(response.error || "Failed to fetch HDA codec debug state.");
        }

        const timeIso = new Date().toISOString();
        const ts = timeIso.replaceAll(":", "-").replaceAll(".", "-");
        downloadJson(
          { timeIso, build: getBuildInfoForExport(), state: response.state ?? null },
          `aero-hda-codec-state-${ts}.json`,
        );
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  const exportHdaControllerStateButton = el("button", {
    text: "Export HDA controller state (bin)",
    onclick: async () => {
      status.textContent = "";
      try {
        const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
        if (vmRuntime === "machine") {
          throw new Error("HDA controller state export is unavailable in vmRuntime=machine.");
        }
        const ioWorker = workerCoordinator.getIoWorker();
        if (!ioWorker) {
          throw new Error("I/O worker is not running. Start workers before exporting HDA controller state.");
        }

        const requestId = hdaSnapshotStateRequestId++;
        const response = await new Promise<HdaSnapshotStateResultMessage>((resolve, reject) => {
          const timeout = window.setTimeout(() => {
            cleanup();
            reject(new Error("Timed out waiting for IO worker HDA snapshot state response."));
          }, 3000);
          (timeout as unknown as { unref?: () => void }).unref?.();

          const onMessage = (ev: MessageEvent<unknown>) => {
            const msg = ev.data as Partial<HdaSnapshotStateResultMessage> | null;
            if (!msg || msg.type !== "hda.snapshotStateResult") return;
            if (msg.requestId !== requestId) return;
            cleanup();
            resolve(msg as HdaSnapshotStateResultMessage);
          };

          const cleanup = () => {
            window.clearTimeout(timeout);
            ioWorker.removeEventListener("message", onMessage);
          };

          ioWorker.addEventListener("message", onMessage);
          ioWorker.postMessage({ type: "hda.snapshotState", requestId });
        });

        if (!response.ok) {
          throw new Error(response.error || "Failed to fetch HDA snapshot state.");
        }
        if (!(response.bytes instanceof Uint8Array)) {
          throw new Error("Invalid HDA snapshot state response.");
        }

        const timeIso = new Date().toISOString();
        const ts = timeIso.replaceAll(":", "-").replaceAll(".", "-");
        const payload = ensureArrayBufferBacked(response.bytes);
        downloadFile(new Blob([payload], { type: "application/octet-stream" }), `aero-hda-controller-state-${ts}.bin`);
        status.textContent = "Saved HDA controller state.";
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  const exportQaBundleButton = el("button", {
    text: "Export audio QA bundle (tar)",
    onclick: async () => {
      status.textContent = "";
      try {
        const g = globalThis as unknown as {
          __aeroAudioOutput?: unknown;
          __aeroAudioOutputWorker?: unknown;
          __aeroAudioOutputHdaDemo?: unknown;
          __aeroAudioOutputVirtioSndDemo?: unknown;
          __aeroAudioOutputLoopback?: unknown;
        };
        const encoder = new TextEncoder();
        const timeIso = new Date().toISOString();
        const ts = timeIso.replaceAll(":", "-").replaceAll(".", "-");
        const dir = `aero-audio-qa-${ts}`;
        const mtimeSec = Math.trunc(Date.now() / 1000);

        status.textContent = "Exporting audio QA bundle…";

        const hostMediaDevices = await snapshotHostMediaDevicesForExport();

        const metricsReport = {
          timeIso,
          build: getBuildInfoForExport(),
          userAgent: navigator.userAgent,
          crossOriginIsolated: typeof crossOriginIsolated === "boolean" ? crossOriginIsolated : false,
          host: snapshotHostEnvForExport(),
          hostMediaDevices,
          ringBufferOwners: snapshotRingBufferOwnersForExport(),
          ringBuffers: snapshotRingBuffersForExport(),
          audioOutputs: {
            __aeroAudioOutput: snapshotAudioOutput(g.__aeroAudioOutput),
            __aeroAudioOutputWorker: snapshotAudioOutput(g.__aeroAudioOutputWorker),
            __aeroAudioOutputHdaDemo: snapshotAudioOutput(g.__aeroAudioOutputHdaDemo),
            __aeroAudioOutputVirtioSndDemo: snapshotAudioOutput(g.__aeroAudioOutputVirtioSndDemo),
            __aeroAudioOutputLoopback: snapshotAudioOutput(g.__aeroAudioOutputLoopback),
          },
          workerProducer: snapshotWorkerProducerForExport(),
          microphone: snapshotMicAttachment(),
          workers: snapshotWorkerCoordinator(),
          config: snapshotConfigForExport(),
        };

        const entries: Array<{ path: string; data: Uint8Array }> = [
          {
            path: `${dir}/audio-metrics.json`,
            data: encoder.encode(JSON.stringify(metricsReport, null, 2)),
          },
        ];

        entries.push({
          path: `${dir}/README.txt`,
          data: encoder.encode(
            [
              `Aero audio QA bundle (${timeIso})`,
              `Build: version=${metricsReport.build.version} gitSha=${metricsReport.build.gitSha} builtAt=${metricsReport.build.builtAt}`,
              ``,
              `This archive is best-effort: some files may be missing and corresponding *-error.txt or *.json { ok:false } files may be present.`,
              ``,
              `Key files:`,
              `- audio-metrics.json: consolidated host-side audio counters + worker snapshot + config snapshot`,
              `- manifest.json: list of all files in this tar (paths + byte sizes)`,
              `- aero-config.json: effective runtime config snapshot (sensitive fields redacted)`,
              `- aero.version.json / aero.version-meta.json: build/version endpoint snapshot (best-effort) + metadata`,
              `- workers.json: worker coordinator snapshot (state/health + wasm variants)`,
              `- host-media-devices.json: browser media device inventory + mic permission state (device/group IDs hashed)`,
              `- audio-output-*.wav: buffered output ring snapshots (PCM16 WAV)`,
              `- audio-output-*.json: metadata for each output WAV (sample rate, ring indices/counters, signal stats)`,
              `- microphone-buffered.wav / microphone-buffered.json: buffered mic ring snapshot + metadata (backend + track state/settings/constraints/capabilities; device/group IDs hashed)`,
              `- audio-samples.txt: one-line summary of captured WAVs (includes RMS/peak dBFS estimates)`,
              `- hda-codec-state.json: HDA codec gating debug state (requires IO worker + codec_debug_state export)`,
              `- hda-controller-state.bin / hda-controller-state.json: HDA controller snapshot bytes (deterministic, no guest RAM) + metadata`,
              `- hda-tick-stats.json: IO worker HDA tick clamp counters (stall observability)`,
              `- virtio-snd-state.bin / virtio-snd-state.json: virtio-snd PCI function snapshot bytes (when present) + metadata`,
              `- screenshot-*.png / screenshot.json: guest framebuffer screenshot (requires GPU worker) + metadata`,
              `- serial.txt: guest serial output tail (best-effort)`,
              `- trace.json / trace-meta.json: Chrome trace export (best-effort) + metadata`,
              `- perf-hud.json / perf-hud-meta.json: on-page perf HUD export (best-effort) + metadata`,
              ``,
              `Microphone backend notes:`,
              `- backend=worklet: AudioWorklet capture path (preferred, low latency).`,
              `- backend=script: ScriptProcessorNode fallback (higher latency); indicates worklet init failed or is unavailable.`,
              ``,
              `See docs/testing/audio-windows7.md for the Win7 audio smoke test + interpretation tips.`,
              ``,
            ].join("\n"),
          ),
        });

        // Include the currently effective runtime config (redacting sensitive fields). This is
        // useful when QA runs are executed with URL overrides / deployment configs and we need to
        // reproduce the exact worker/runtime setup.
        try {
          const cfg = snapshotConfigForExport();
          const cfgOk = !(cfg && typeof cfg === "object" && "error" in cfg);
          entries.push({
            path: `${dir}/aero-config.json`,
            data: encoder.encode(JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: cfgOk, ...cfg }, null, 2)),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/aero-config-error.txt`, data: encoder.encode(message) });
        }

        // Include the build info endpoint, when available (more complete than the subset embedded
        // in `__AERO_BUILD_INFO__`).
        try {
          const res = await fetch("/aero.version.json", { cache: "no-store" });
          if (!res.ok) throw new Error(`Failed to fetch /aero.version.json (HTTP ${res.status})`);
          const text = await res.text();
          const payload = encoder.encode(text);
          entries.push({ path: `${dir}/aero.version.json`, data: payload });
          entries.push({
            path: `${dir}/aero.version-meta.json`,
            data: encoder.encode(
              JSON.stringify(
                { timeIso, build: getBuildInfoForExport(), ok: true, file: "aero.version.json", bytes: payload.byteLength },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/aero.version-error.txt`, data: encoder.encode(message) });
          entries.push({
            path: `${dir}/aero.version-meta.json`,
            data: encoder.encode(
              JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, file: "aero.version.json", error: message }, null, 2),
            ),
          });
        }

        // Host media device inventory (best-effort). Useful for debugging microphone failures
        // (permissions, device selection, supported constraints).
        entries.push({
          path: `${dir}/host-media-devices.json`,
          data: encoder.encode(JSON.stringify({ timeIso, build: getBuildInfoForExport(), ...hostMediaDevices }, null, 2)),
        });

        // Worker coordinator snapshot (best-effort): captures worker state/health + wasm variants.
        try {
          const workersSnap = snapshotWorkerCoordinator();
          const workersOk = !(workersSnap && typeof workersSnap === "object" && "error" in workersSnap);
          entries.push({
            path: `${dir}/workers.json`,
            data: encoder.encode(
              JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: workersOk, workers: workersSnap }, null, 2),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/workers-error.txt`, data: encoder.encode(message) });
        }

        // Guest serial output tail (best-effort). Useful when driver bring-up emits logs via COM1.
        try {
          const serialText = workerCoordinator.getSerialOutputText();
          const serialBytes = workerCoordinator.getSerialOutputBytes();
          const MAX_CHARS = 200_000;
          const truncated = serialText.length > MAX_CHARS;
          const tail = truncated ? serialText.slice(-MAX_CHARS) : serialText;
          const build = getBuildInfoForExport();
          const baseHeader = truncated
            ? `# NOTE: serial output truncated (chars=${serialText.length.toLocaleString()} bytes=${serialBytes.toLocaleString()})\n`
            : `# serial output (bytes=${serialBytes.toLocaleString()})\n`;
          const header =
            baseHeader +
            `# timeIso: ${timeIso}\n` +
            `# build: version=${build.version} gitSha=${build.gitSha} builtAt=${build.builtAt}\n\n`;
          entries.push({ path: `${dir}/serial.txt`, data: encoder.encode(header + tail) });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/serial-error.txt`, data: encoder.encode(message) });
        }

        // Buffered ring audio snapshots (best-effort). This is *not* a long recording: it captures
        // the currently-buffered samples in the AudioWorklet ring(s) (usually a few hundred ms).
        try {
          const build = getBuildInfoForExport();
          const summaryLines: string[] = [
            `# timeIso: ${timeIso}`,
            `# build: version=${build.version} gitSha=${build.gitSha} builtAt=${build.builtAt}`,
            ``,
          ];
          const captureSeconds = 3;

          const outputsToCapture: Array<{ name: string; out: unknown }> = [
            { name: "main", out: g.__aeroAudioOutput },
            { name: "worker", out: g.__aeroAudioOutputWorker },
            { name: "hda-demo", out: g.__aeroAudioOutputHdaDemo },
            { name: "virtio-snd-demo", out: g.__aeroAudioOutputVirtioSndDemo },
            { name: "loopback", out: g.__aeroAudioOutputLoopback },
          ];

          let capturedAny = false;
          for (const item of outputsToCapture) {
            const res = snapshotAudioOutputWav(item.out, { maxSeconds: captureSeconds });
            const wavFile = `audio-output-${item.name}.wav`;
            if (!res.ok) {
              summaryLines.push(`audio-output-${item.name}: ${res.error}`);
              entries.push({
                path: `${dir}/audio-output-${item.name}.json`,
                data: encoder.encode(
                  JSON.stringify(
                    {
                      timeIso,
                      build: getBuildInfoForExport(),
                      ok: false,
                      file: wavFile,
                      error: res.error,
                      ...(res.context ? { context: res.context } : {}),
                    },
                    null,
                    2,
                  ),
                ),
              });
              entries.push({
                path: `${dir}/audio-output-${item.name}-error.txt`,
                data: encoder.encode(res.error + "\n"),
              });
              continue;
            }
            capturedAny = true;
            entries.push({ path: `${dir}/${wavFile}`, data: res.wav });
            entries.push({
              path: `${dir}/audio-output-${item.name}.json`,
              data: encoder.encode(
                JSON.stringify(
                  { timeIso, build: getBuildInfoForExport(), ok: true, file: wavFile, bytes: res.wav.byteLength, ...res.meta },
                  null,
                  2,
                ),
              ),
            });
            summaryLines.push(
              `audio-output-${item.name}.wav: frames=${res.meta.framesCaptured} avail=${res.meta.availableFrames} sr=${res.meta.sampleRate} cc=${res.meta.channelCount} ctx=${res.meta.audioContextState ?? "n/a"} baseLatMs=${formatMsOrNa(res.meta.baseLatencySeconds)} outputLatMs=${formatMsOrNa(res.meta.outputLatencySeconds)} underruns=${res.meta.underrunCount} overruns=${res.meta.overrunCount} rms=${formatDbfsFromLinear(res.meta.signal.rms)}dBFS peak=${formatDbfsFromLinear(res.meta.signal.peakAbs)}dBFS`,
            );
          }

          const micRes = snapshotMicAttachmentWav({ maxSeconds: captureSeconds });
          const micWavFile = "microphone-buffered.wav";
          if (micRes.ok) {
            entries.push({ path: `${dir}/${micWavFile}`, data: micRes.wav });
            entries.push({
              path: `${dir}/microphone-buffered.json`,
              data: encoder.encode(
                JSON.stringify(
                  { timeIso, build: getBuildInfoForExport(), ok: true, file: micWavFile, bytes: micRes.wav.byteLength, ...micRes.meta },
                  null,
                  2,
                ),
              ),
            });
            summaryLines.push(
              `microphone-buffered.wav: samples=${micRes.meta.samplesCaptured} avail=${micRes.meta.availableSamples} dropped=${micRes.meta.droppedSamples} sr=${micRes.meta.sampleRate} ctx=${micRes.meta.audioContextState ?? "n/a"} backend=${micRes.meta.backend ?? "n/a"} muted=${micRes.meta.muted ?? "n/a"} trackMuted=${micRes.meta.trackMuted ?? "n/a"} trackReadyState=${micRes.meta.trackReadyState ?? "n/a"} deviceIdHash=${micRes.meta.deviceIdHash ?? "n/a"} rms=${formatDbfsFromLinear(micRes.meta.signal.rms)}dBFS peak=${formatDbfsFromLinear(micRes.meta.signal.peakAbs)}dBFS`,
            );
          } else {
            summaryLines.push(`microphone-buffered: ${micRes.error}`);
            entries.push({
              path: `${dir}/microphone-buffered.json`,
              data: encoder.encode(
                JSON.stringify(
                  {
                    timeIso,
                    build: getBuildInfoForExport(),
                    ok: false,
                    file: micWavFile,
                    error: micRes.error,
                    ...(micRes.context ? { context: micRes.context } : {}),
                  },
                  null,
                  2,
                ),
              ),
            });
            entries.push({ path: `${dir}/microphone-buffered-error.txt`, data: encoder.encode(micRes.error + "\n") });
          }

          if (!capturedAny && !micRes.ok) {
            summaryLines.push("note: no WAV snapshots captured (see lines above).");
          }
          entries.push({ path: `${dir}/audio-samples.txt`, data: encoder.encode(summaryLines.join("\n") + "\n") });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/audio-samples-error.txt`, data: encoder.encode(message) });
        }

        // HDA codec debug state (best-effort).
        try {
          const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
          if (vmRuntime === "machine") {
            throw new Error("HDA codec state is unavailable in vmRuntime=machine.");
          }
          const ioWorker = workerCoordinator.getIoWorker();
          if (!ioWorker) throw new Error("I/O worker is not running.");
          const requestId = hdaCodecDebugRequestId++;
          const response = await new Promise<HdaCodecDebugStateResultMessage>((resolve, reject) => {
            const timeout = window.setTimeout(() => {
              cleanup();
              reject(new Error("Timed out waiting for IO worker HDA codec debug state response."));
            }, 3000);
            (timeout as unknown as { unref?: () => void }).unref?.();

            const onMessage = (ev: MessageEvent<unknown>) => {
              const msg = ev.data as Partial<HdaCodecDebugStateResultMessage> | null;
              if (!msg || msg.type !== "hda.codecDebugStateResult") return;
              if (msg.requestId !== requestId) return;
              cleanup();
              resolve(msg as HdaCodecDebugStateResultMessage);
            };

            const cleanup = () => {
              window.clearTimeout(timeout);
              ioWorker.removeEventListener("message", onMessage);
            };

            ioWorker.addEventListener("message", onMessage);
            ioWorker.postMessage({ type: "hda.codecDebugState", requestId });
          });

          entries.push({
            path: `${dir}/hda-codec-state.json`,
            data: encoder.encode(
              JSON.stringify(
                {
                  timeIso,
                  build: getBuildInfoForExport(),
                  ok: response.ok,
                  ...(response.ok ? { state: response.state ?? null } : { error: response.error || "unknown" }),
                },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({
            path: `${dir}/hda-codec-state.json`,
            data: encoder.encode(JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, error: message }, null, 2)),
          });
        }

        // HDA controller snapshot bytes (best-effort). Useful for debugging guest driver behavior;
        // this is a small deterministic blob (no guest RAM).
        try {
          const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
          if (vmRuntime === "machine") {
            throw new Error("HDA controller state is unavailable in vmRuntime=machine.");
          }
          const ioWorker = workerCoordinator.getIoWorker();
          if (!ioWorker) throw new Error("I/O worker is not running.");
          const requestId = hdaSnapshotStateRequestId++;
          const response = await new Promise<HdaSnapshotStateResultMessage>((resolve, reject) => {
            const timeout = window.setTimeout(() => {
              cleanup();
              reject(new Error("Timed out waiting for IO worker HDA snapshot state response."));
            }, 3000);
            (timeout as unknown as { unref?: () => void }).unref?.();

            const onMessage = (ev: MessageEvent<unknown>) => {
              const msg = ev.data as Partial<HdaSnapshotStateResultMessage> | null;
              if (!msg || msg.type !== "hda.snapshotStateResult") return;
              if (msg.requestId !== requestId) return;
              cleanup();
              resolve(msg as HdaSnapshotStateResultMessage);
            };

            const cleanup = () => {
              window.clearTimeout(timeout);
              ioWorker.removeEventListener("message", onMessage);
            };

            ioWorker.addEventListener("message", onMessage);
            ioWorker.postMessage({ type: "hda.snapshotState", requestId });
          });

          if (!response.ok) {
            throw new Error(response.error || "Failed to fetch HDA snapshot state.");
          }
          if (!(response.bytes instanceof Uint8Array)) {
            throw new Error("Invalid HDA snapshot state response.");
          }
          entries.push({ path: `${dir}/hda-controller-state.bin`, data: response.bytes });
          entries.push({
            path: `${dir}/hda-controller-state.json`,
            data: encoder.encode(
              JSON.stringify(
                {
                  timeIso,
                  build: getBuildInfoForExport(),
                  ok: true,
                  file: "hda-controller-state.bin",
                  bytes: response.bytes.byteLength,
                },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/hda-controller-state-error.txt`, data: encoder.encode(message) });
          entries.push({
            path: `${dir}/hda-controller-state.json`,
            data: encoder.encode(
              JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, file: "hda-controller-state.bin", error: message }, null, 2),
            ),
          });
        }

        // HDA tick clamp stats (best-effort). These are useful even when tracing is disabled,
        // because they surface whether the IO worker has been clamping large host time deltas.
        try {
          const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
          if (vmRuntime === "machine") {
            throw new Error("HDA tick stats are unavailable in vmRuntime=machine.");
          }
          const ioWorker = workerCoordinator.getIoWorker();
          if (!ioWorker) throw new Error("I/O worker is not running.");
          const requestId = hdaTickStatsRequestId++;
          const response = await new Promise<HdaTickStatsResultMessage>((resolve, reject) => {
            const timeout = window.setTimeout(() => {
              cleanup();
              reject(new Error("Timed out waiting for IO worker HDA tick stats response."));
            }, 3000);
            (timeout as unknown as { unref?: () => void }).unref?.();

            const onMessage = (ev: MessageEvent<unknown>) => {
              const msg = ev.data as Partial<HdaTickStatsResultMessage> | null;
              if (!msg || msg.type !== "hda.tickStatsResult") return;
              if (msg.requestId !== requestId) return;
              cleanup();
              resolve(msg as HdaTickStatsResultMessage);
            };

            const cleanup = () => {
              window.clearTimeout(timeout);
              ioWorker.removeEventListener("message", onMessage);
            };

            ioWorker.addEventListener("message", onMessage);
            ioWorker.postMessage({ type: "hda.tickStats", requestId });
          });

          entries.push({
            path: `${dir}/hda-tick-stats.json`,
            data: encoder.encode(
              JSON.stringify(
                {
                  timeIso,
                  build: getBuildInfoForExport(),
                  ok: response.ok,
                  ...(response.ok ? { stats: response.stats ?? null } : { error: response.error || "unknown" }),
                },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({
            path: `${dir}/hda-tick-stats.json`,
            data: encoder.encode(JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, error: message }, null, 2)),
          });
        }

        // virtio-snd PCI function snapshot bytes (best-effort). Useful when the runtime is using
        // virtio-snd (e.g. HDA omitted) or when debugging the virtio driver stack.
        try {
          const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
          if (vmRuntime === "machine") {
            throw new Error("virtio-snd state is unavailable in vmRuntime=machine.");
          }
          const ioWorker = workerCoordinator.getIoWorker();
          if (!ioWorker) throw new Error("I/O worker is not running.");
          const requestId = virtioSndSnapshotStateRequestId++;
          const response = await new Promise<VirtioSndSnapshotStateResultMessage>((resolve, reject) => {
            const timeout = window.setTimeout(() => {
              cleanup();
              reject(new Error("Timed out waiting for IO worker virtio-snd snapshot state response."));
            }, 3000);
            (timeout as unknown as { unref?: () => void }).unref?.();

            const onMessage = (ev: MessageEvent<unknown>) => {
              const msg = ev.data as Partial<VirtioSndSnapshotStateResultMessage> | null;
              if (!msg || msg.type !== "virtioSnd.snapshotStateResult") return;
              if (msg.requestId !== requestId) return;
              cleanup();
              resolve(msg as VirtioSndSnapshotStateResultMessage);
            };

            const cleanup = () => {
              window.clearTimeout(timeout);
              ioWorker.removeEventListener("message", onMessage);
            };

            ioWorker.addEventListener("message", onMessage);
            ioWorker.postMessage({ type: "virtioSnd.snapshotState", requestId });
          });

          if (!response.ok) {
            throw new Error(response.error || "Failed to fetch virtio-snd snapshot state.");
          }
          if (!(response.bytes instanceof Uint8Array)) {
            throw new Error("Invalid virtio-snd snapshot state response.");
          }
          entries.push({ path: `${dir}/virtio-snd-state.bin`, data: response.bytes });
          entries.push({
            path: `${dir}/virtio-snd-state.json`,
            data: encoder.encode(
              JSON.stringify(
                { timeIso, build: getBuildInfoForExport(), ok: true, file: "virtio-snd-state.bin", bytes: response.bytes.byteLength },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/virtio-snd-state-error.txt`, data: encoder.encode(message) });
          entries.push({
            path: `${dir}/virtio-snd-state.json`,
            data: encoder.encode(JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, file: "virtio-snd-state.bin", error: message }, null, 2)),
          });
        }

        // Guest screenshot (best-effort).
        try {
          const gpuWorker = workerCoordinator.getWorker("gpu");
          if (!gpuWorker) throw new Error("GPU worker is not running.");

          const requestId = qaBundleScreenshotRequestId++;
          const response = await new Promise<GpuRuntimeScreenshotResponseMessage>((resolve, reject) => {
            const timeout = window.setTimeout(() => {
              cleanup();
              reject(new Error("Timed out waiting for GPU screenshot response."));
            }, 8000);
            (timeout as unknown as { unref?: () => void }).unref?.();

            const onMessage = (ev: MessageEvent<unknown>) => {
              const msg = ev.data;
              if (!isGpuWorkerMessageBase(msg)) return;
              const typed = msg as GpuRuntimeScreenshotResponseMessage;
              if (typed.type !== "screenshot") return;
              if (typed.requestId !== requestId) return;
              if (!(typed.rgba8 instanceof ArrayBuffer)) return;
              cleanup();
              resolve(typed);
            };

            const cleanup = () => {
              window.clearTimeout(timeout);
              gpuWorker.removeEventListener("message", onMessage);
            };

            gpuWorker.addEventListener("message", onMessage);
            gpuWorker.postMessage({
              protocol: GPU_PROTOCOL_NAME,
              protocolVersion: GPU_PROTOCOL_VERSION,
              type: "screenshot",
              requestId,
              includeCursor: false,
            });
          });

          const rgba8 = new Uint8Array(response.rgba8);
          const pngBlob = await rgba8ToPngBlob(response.width, response.height, rgba8);
          const pngBytes = new Uint8Array(await pngBlob.arrayBuffer());
          const screenshotFile = `screenshot-${response.width}x${response.height}.png`;
          entries.push({ path: `${dir}/${screenshotFile}`, data: pngBytes });
          entries.push({
            path: `${dir}/screenshot.json`,
            data: encoder.encode(
              JSON.stringify(
                {
                  timeIso,
                  build: getBuildInfoForExport(),
                  ok: true,
                  file: screenshotFile,
                  width: response.width,
                  height: response.height,
                  includeCursor: false,
                  bytes: pngBytes.byteLength,
                },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/screenshot-error.txt`, data: encoder.encode(message) });
          entries.push({
            path: `${dir}/screenshot.json`,
            data: encoder.encode(JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, error: message }, null, 2)),
          });
        }

        // Chrome trace export (best-effort). This is only meaningful if trace was enabled at some
        // point (e.g. via `?trace` or `aero.perf.traceStart()`), but exporting an empty trace is
        // cheap and still useful for recording the thread/worker metadata.
        try {
          const data = await perf.exportTrace({ asString: true });
          const payload = typeof data === "string" ? data : JSON.stringify(data);
          const traceBytes = encoder.encode(payload);
          entries.push({ path: `${dir}/trace.json`, data: traceBytes });
          entries.push({
            path: `${dir}/trace-meta.json`,
            data: encoder.encode(
              JSON.stringify(
                { timeIso, build: getBuildInfoForExport(), ok: true, file: "trace.json", bytes: traceBytes.byteLength },
                null,
                2,
              ),
            ),
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/trace-error.txt`, data: encoder.encode(message) });
          entries.push({
            path: `${dir}/trace-meta.json`,
            data: encoder.encode(
              JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, file: "trace.json", error: message }, null, 2),
            ),
          });
        }

        // Perf HUD export (best-effort). This captures the low-rate perf telemetry visible in the
        // on-page HUD and can be useful for correlating audio underruns with CPU/GPU stalls.
        try {
          const aero = (globalThis as unknown as { aero?: unknown }).aero;
          const perfApi = aero && typeof aero === "object" ? (aero as { perf?: unknown }).perf : undefined;
          const exportFn = (perfApi as { export?: unknown } | undefined)?.export;
          if (typeof exportFn === "function") {
            const data = (exportFn as (this: unknown) => unknown).call(perfApi);
            const perfHudBytes = encoder.encode(JSON.stringify(data, null, 2));
            entries.push({ path: `${dir}/perf-hud.json`, data: perfHudBytes });
            entries.push({
              path: `${dir}/perf-hud-meta.json`,
              data: encoder.encode(
                JSON.stringify(
                  { timeIso, build: getBuildInfoForExport(), ok: true, file: "perf-hud.json", bytes: perfHudBytes.byteLength },
                  null,
                  2,
                ),
              ),
            });
          } else {
            entries.push({
              path: `${dir}/perf-hud-meta.json`,
              data: encoder.encode(
                JSON.stringify(
                  {
                    timeIso,
                    build: getBuildInfoForExport(),
                    ok: false,
                    file: "perf-hud.json",
                    error: "window.aero.perf.export is not available.",
                  },
                  null,
                  2,
                ),
              ),
            });
          }
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/perf-hud-error.txt`, data: encoder.encode(message) });
          entries.push({
            path: `${dir}/perf-hud-meta.json`,
            data: encoder.encode(
              JSON.stringify({ timeIso, build: getBuildInfoForExport(), ok: false, file: "perf-hud.json", error: message }, null, 2),
            ),
          });
        }

        // Bundle manifest (best-effort). This helps tooling quickly identify which files were
        // emitted by this export run without unpacking the tar manually.
        try {
          const build = getBuildInfoForExport();
          const manifestPath = `${dir}/manifest.json`;
          const baseFiles = entries.map((e) => ({ path: e.path, bytes: e.data.byteLength }));

          // Include the manifest itself in the file list. This is self-referential (the manifest
          // contains its own byte size), so compute a small fixed point by iterating until the
          // encoded size stabilizes.
          let manifestSizeGuess = 0;
          let manifestBytes = encoder.encode("");
          for (let i = 0; i < 8; i += 1) {
            const files = [...baseFiles, { path: manifestPath, bytes: manifestSizeGuess }];
            files.sort((a, b) => a.path.localeCompare(b.path));
            const totalBytes = files.reduce((sum, f) => sum + (Number.isFinite(f.bytes) ? f.bytes : 0), 0);
            manifestBytes = encoder.encode(
              JSON.stringify({ schemaVersion: 1, timeIso, build, fileCount: files.length, totalBytes, files }, null, 2),
            );
            const next = manifestBytes.byteLength;
            if (next === manifestSizeGuess) break;
            manifestSizeGuess = next;
          }

          entries.push({ path: manifestPath, data: manifestBytes });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          entries.push({ path: `${dir}/manifest-error.txt`, data: encoder.encode(message) });
        }

        const tarBytes = createTarArchive(entries, { mtimeSec });
        // `BlobPart` typings require ArrayBuffer-backed views; defensively normalize.
        const tarPayload = ensureArrayBufferBacked(tarBytes);
        downloadFile(new Blob([tarPayload], { type: "application/x-tar" }), `${dir}.tar`);
        status.textContent = `Saved QA bundle → ${dir}.tar`;
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Audio" }),
    el("div", { class: "row" }, button, workerButton, hdaDemoButton, virtioSndDemoButton, loopbackButton),
    el("div", { class: "row" }, exportMetricsButton, exportHdaCodecStateButton, exportHdaControllerStateButton, exportQaBundleButton),
    status,
  );
}

function renderMicrophonePanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const stateLine = el("div", { class: "mono", text: "state=inactive" });
  const statsLine = el("div", { class: "mono", text: "" });
  const deviceIdEncoder = new TextEncoder();

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
      // Avoid displaying raw `deviceId` values (they can be long stable identifiers).
      const deviceIdHash = dev.deviceId ? fnv1a32Hex(deviceIdEncoder.encode(dev.deviceId)) : "";
      const label = dev.label || (deviceIdHash ? `mic (${deviceIdHash})` : "mic");
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
    const deviceLabel = deviceSelect.value ? fnv1a32Hex(deviceIdEncoder.encode(deviceSelect.value)) : "default";
    statsLine.textContent =
      `bufferedSamples=${buffered} droppedSamples=${dropped} ` + `device=${deviceLabel}`;
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

        const sanitizeTrackInfo = (value: unknown): Record<string, unknown> | null => {
          if (!value || typeof value !== "object") return null;

          const seen = new WeakMap<object, unknown>();
          const collectSeen = new WeakSet<object>();
          const collectStrings = (v: unknown, out: string[]): void => {
            if (typeof v === "string") {
              out.push(v);
              return;
            }

            if (!v || typeof v !== "object") return;

            // Defensive: avoid recursion loops if a browser ever returns a cyclic structure here.
            if (collectSeen.has(v)) return;
            collectSeen.add(v);

            if (Array.isArray(v)) {
              for (const item of v) collectStrings(item, out);
              return;
            }

            for (const item of Object.values(v as Record<string, unknown>)) {
              collectStrings(item, out);
            }
          };

          const sanitize = (v: unknown): unknown => {
            if (!v || typeof v !== "object") return v;
            if (Array.isArray(v)) return v.map(sanitize);

            const obj = v as Record<string, unknown>;
            const cached = seen.get(obj);
            if (cached !== undefined) return cached;

            const out: Record<string, unknown> = {};
            seen.set(obj, out);
            for (const [key, val] of Object.entries(obj)) {
              if (key === "deviceId" || key === "groupId") {
                const strings: string[] = [];
                collectStrings(val, strings);
                const hashes = Array.from(
                  new Set(strings.filter((s) => typeof s === "string" && s.length).map((s) => fnv1a32Hex(deviceIdEncoder.encode(s)))),
                );
                if (hashes.length === 1) {
                  out[`${key}Hash`] = hashes[0];
                } else if (hashes.length > 1) {
                  out[`${key}Hashes`] = hashes;
                }
                continue;
              }
              out[key] = sanitize(val);
            }
            return out;
          };

          const out = sanitize(value);
          return out && typeof out === "object" && !Array.isArray(out) ? (out as Record<string, unknown>) : null;
        };
        const dbg = mic.getDebugInfo();
        // Prefer the user-selected deviceId for stable repro. If "default" was selected, fall back
        // to the deviceId reported by the active track settings (if any).
        const selectedDeviceIdHash = deviceSelect.value ? fnv1a32Hex(deviceIdEncoder.encode(deviceSelect.value)) : null;
        const trackDeviceId = (dbg.trackSettings as Record<string, unknown> | null)?.["deviceId"];
        const trackDeviceIdHash = typeof trackDeviceId === "string" && trackDeviceId.length ? fnv1a32Hex(deviceIdEncoder.encode(trackDeviceId)) : null;
        const deviceIdHash = selectedDeviceIdHash ?? trackDeviceIdHash;
        micAttachment = {
          ringBuffer: mic.ringBuffer.sab,
          sampleRate: mic.actualSampleRate,
          deviceIdHash,
          backend: dbg.backend,
          audioContextState: typeof dbg.audioContextState === "string" ? dbg.audioContextState : null,
          workletInitError: typeof dbg.workletInitError === "string" && dbg.workletInitError.length ? dbg.workletInitError : null,
          trackLabel: dbg.trackLabel,
          trackEnabled: typeof dbg.trackEnabled === "boolean" ? dbg.trackEnabled : null,
          trackMuted: typeof dbg.trackMuted === "boolean" ? dbg.trackMuted : null,
          trackReadyState: typeof dbg.trackReadyState === "string" ? dbg.trackReadyState : null,
          trackSettings: sanitizeTrackInfo(dbg.trackSettings),
          trackConstraints: sanitizeTrackInfo(dbg.trackConstraints),
          trackCapabilities: sanitizeTrackInfo(dbg.trackCapabilities),
          bufferMs: Math.max(10, Number(bufferMsInput.value || 0) | 0),
          echoCancellation: echoCancellation.checked,
          noiseSuppression: noiseSuppression.checked,
          autoGainControl: autoGainControl.checked,
          muted: mutedInput.checked,
        };
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
    if (micAttachment) micAttachment.muted = mutedInput.checked;
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

function renderInputDiagnosticsPanel(): HTMLElement {
  const host = el("div", { class: "panel" });
  const panel = mountInputDiagnosticsPanel(host);

  const tick = (): void => {
    const status = workerCoordinator.getStatusView();
    if (!status) {
      panel.setSnapshot(null);
      return;
    }
    panel.setSnapshot(readInputDiagnosticsSnapshotFromStatus(status));
  };

  tick();
  const timer = globalThis.setInterval(tick, 250);
  (timer as unknown as { unref?: () => void }).unref?.();

  return host;
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

  const resolveInputWorker = (): Worker | null => {
    // In machine runtime, the I/O worker may be host-only and cannot inject input
    // into guest devices. Route input batches to the CPU worker instead.
    const runtime = configManager.getState().effective.vmRuntime;
    if (runtime === "machine") {
      return workerCoordinator.getWorker("cpu") ?? null;
    }
    return workerCoordinator.getIoWorker();
  };

  const inputTarget: InputBatchTarget & {
    addEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
    removeEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
  } = (() => {
    const messageListeners: ((ev: MessageEvent<unknown>) => void)[] = [];
    let attachedCpu: Worker | null = null;
    let attachedIo: Worker | null = null;

    const onWorkerMessage = (ev: MessageEvent<unknown>): void => {
      const data = ev.data as { type?: unknown } | null;
      if (!data || typeof data !== "object") return;
      // Forward only input-recycle messages to avoid spamming the capture with unrelated worker traffic.
      if (data.type !== "in:input-batch-recycle") return;
      for (const listener of messageListeners.slice()) listener(ev);
    };

    const syncWorkerAttachments = (): void => {
      if (messageListeners.length === 0) {
        if (attachedCpu) attachedCpu.removeEventListener("message", onWorkerMessage as EventListener);
        if (attachedIo) attachedIo.removeEventListener("message", onWorkerMessage as EventListener);
        attachedCpu = null;
        attachedIo = null;
        return;
      }

      const cpu = workerCoordinator.getWorker("cpu") ?? null;
      const io = workerCoordinator.getIoWorker();
      if (cpu !== attachedCpu) {
        if (attachedCpu) attachedCpu.removeEventListener("message", onWorkerMessage as EventListener);
        if (cpu) cpu.addEventListener("message", onWorkerMessage as EventListener);
        attachedCpu = cpu;
      }
      if (io !== attachedIo) {
        if (attachedIo) attachedIo.removeEventListener("message", onWorkerMessage as EventListener);
        if (io) io.addEventListener("message", onWorkerMessage as EventListener);
        attachedIo = io;
      }
    };

    return {
      postMessage: (msg, transfer) => {
        syncWorkerAttachments();
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
          append(`mouse: wheel=${words[off + 2] | 0} pan=${words[off + 3] | 0}`);
        }
      }

      const worker = resolveInputWorker();
      if (worker) {
        worker.postMessage(msg, transfer);
      }
      },
      addEventListener: (type, listener) => {
        if (type !== "message") return;
        messageListeners.push(listener);
        syncWorkerAttachments();
      },
      removeEventListener: (type, listener) => {
        if (type !== "message") return;
        const idx = messageListeners.indexOf(listener);
        if (idx >= 0) messageListeners.splice(idx, 1);
        syncWorkerAttachments();
      },
    };
  })();

  const capture = new InputCapture(canvas, inputTarget, { onBeforeSendBatch: inputRecordReplay.captureHook });
  capture.start();

  const hint = el("div", {
    class: "mono",
    text:
      "Click the canvas to focus + request pointer lock. Keyboard/mouse/gamepad events are batched and forwarded to the VM input worker " +
      "(legacy runtime: I/O worker; machine runtime: CPU worker).",
  });

  const clear = el("button", {
    text: "Clear log",
    onclick: () => {
      log.textContent = "";
    },
  });

  const updateStatus = (): void => {
    const runtime = configManager.getState().effective.vmRuntime;
    const targetRole = runtime === "machine" ? "cpu" : "io";
    const targetWorker = resolveInputWorker();
    status.textContent =
      `vmRuntime=${runtime}  ` +
      `pointerLock=${capture.pointerLocked ? "yes" : "no"}  ` +
      `targetWorker=${targetRole}:${targetWorker ? "ready" : "stopped"}  ` +
      `ioWorker=${workerCoordinator.getIoWorker() ? "ready" : "stopped"}  ` +
      `cpuWorker=${workerCoordinator.getWorker("cpu") ? "ready" : "stopped"}  ` +
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
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "WebUSB passthrough demo is unavailable in vmRuntime=machine.";
        pending = false;
        refreshUi();
        return;
      }
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
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "WebUSB passthrough demo is unavailable in vmRuntime=machine.";
        pending = false;
        refreshUi();
        return;
      }
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
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "WebUSB passthrough demo is unavailable in vmRuntime=machine.";
        pending = false;
        refreshUi();
        return;
      }
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
    const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
    const supported = vmRuntime !== "machine";
    if (!supported && pending) pending = false;

    const workerReady = !!attachedIoWorker && supported;
    const selected = !!selectedInfo;
    const controlsDisabled = !workerReady || !selected || pending;
    runDeviceButton.disabled = controlsDisabled;
    runConfigButton.disabled = controlsDisabled;
    configTotalLenHint = null;
    runConfigFullButton.hidden = true;

    if (!supported) {
      status.textContent =
        `vmRuntime=${vmRuntime}\n` +
        `ioWorker=unsupported\n` +
        `selected=${selectedInfo ? `${hex16(selectedInfo.vendorId)}:${hex16(selectedInfo.productId)}` : "(none)"}\n` +
        `lastRequest=${lastRequest ?? "(none)"}\n` +
        `lastResult=unavailable`;
      resultLine.textContent = "Result: unavailable (vmRuntime=machine)";
      bytesLine.textContent = "(no bytes)";
      if (!errorLine.textContent) errorLine.textContent = "WebUSB passthrough demo is currently only supported in the legacy runtime.";
      clearButton.disabled = false;
      return;
    }

    const selectedLine = selectedInfo
      ? `selected=${selectedInfo.productName ?? "(unnamed)"} vid=${hex16(selectedInfo.vendorId)} pid=${hex16(selectedInfo.productId)}`
      : selectedError
        ? `selected=(none) error=${selectedError}`
        : "selected=(none)";
    const requestLine = `lastRequest=${lastRequest ?? "(none)"}`;
    const resultStatus = lastResult?.status ?? (pending ? "pending" : "(none)");
    status.textContent =
      `vmRuntime=${vmRuntime}\n` +
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
    const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
    const supported = vmRuntime !== "machine";
    const workerReady = !!attachedIoWorker && supported;
    const enabled = snapshot?.enabled ?? false;
    const blocked = snapshot?.blocked ?? true;
    const available = snapshot?.available ?? false;

    if (!supported) {
      status.textContent =
        `vmRuntime=${vmRuntime}\n` +
        `ioWorker=unsupported\n` +
        `harnessAvailable=false\n` +
        `status=unavailable`;
      deviceDesc.textContent = "(unavailable)";
      configDesc.textContent = "(unavailable)";
      lastActionLine.textContent = "Last action: (unavailable)";
      lastCompletionLine.textContent = "Last completion: (unavailable)";
      errorLine.textContent = "UHCI passthrough harness is unavailable in vmRuntime=machine.";
      startButton.disabled = true;
      stopButton.disabled = true;
      return;
    }

    status.textContent =
      `vmRuntime=${vmRuntime}\n` +
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
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "UHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.harness.start" });
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop/Reset",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "UHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
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
    const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
    if (vmRuntime === "machine") {
      if (attachedIoWorker) {
        attachedIoWorker.removeEventListener("message", onMessage);
      }
      attachedIoWorker = null;
      snapshot = null;
      refreshUi();
      return;
    }

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

function renderWebUsbEhciHarnessWorkerPanel(): HTMLElement {
  const status = el("pre", { class: "mono", text: "" });
  const deviceDesc = el("pre", { class: "mono", text: "(none yet)" });
  const configDesc = el("pre", { class: "mono", text: "(none yet)" });

  const lastActionLine = el("pre", { class: "mono", text: "Last action: (none)" });
  const lastCompletionLine = el("pre", { class: "mono", text: "Last completion: (none)" });
  const irqLine = el("div", { class: "mono", text: "" });
  const usbStsLine = el("div", { class: "mono", text: "" });
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
  let snapshot: WebUsbEhciHarnessRuntimeSnapshot | null = null;

  const refreshUi = (): void => {
    const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
    const supported = vmRuntime !== "machine";
    const workerReady = !!attachedIoWorker && supported;
    const available = snapshot?.available ?? false;
    const blocked = snapshot?.blocked ?? true;
    const controllerAttached = snapshot?.controllerAttached ?? false;
    const deviceAttached = snapshot?.deviceAttached ?? false;

    if (!supported) {
      status.textContent =
        `vmRuntime=${vmRuntime}\n` +
        `ioWorker=unsupported\n` +
        `harnessAvailable=false\n` +
        `status=unavailable`;
      irqLine.textContent = "";
      usbStsLine.textContent = "";
      lastActionLine.textContent = "Last action: (unavailable)";
      lastCompletionLine.textContent = "Last completion: (unavailable)";
      deviceDesc.textContent = "(unavailable)";
      configDesc.textContent = "(unavailable)";
      errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
      attachControllerButton.disabled = true;
      detachControllerButton.disabled = true;
      attachDeviceButton.disabled = true;
      detachDeviceButton.disabled = true;
      getDeviceDescButton.disabled = true;
      getConfigDescButton.disabled = true;
      clearUsbStsButton.disabled = true;
      return;
    }

    status.textContent =
      `vmRuntime=${vmRuntime}\n` +
      `ioWorker=${workerReady ? "ready" : "stopped"}\n` +
      `harnessAvailable=${available}\n` +
      `blocked=${blocked}\n` +
      `controllerAttached=${controllerAttached}\n` +
      `deviceAttached=${deviceAttached}\n` +
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

    const usbSts = snapshot?.usbSts ?? 0;
    irqLine.textContent = `IRQ: ${snapshot?.irqLevel ? "asserted" : "deasserted"}`;
    usbStsLine.textContent =
      `USBSTS: 0x${usbSts.toString(16).padStart(8, "0")} ` +
      `USBINT=${snapshot?.usbStsUsbInt ? 1 : 0} ` +
      `USBERRINT=${snapshot?.usbStsUsbErrInt ? 1 : 0} ` +
      `PCD=${snapshot?.usbStsPcd ? 1 : 0}`;

    errorLine.textContent = snapshot?.lastError ? snapshot.lastError : "";

    attachControllerButton.disabled = !workerReady;
    detachControllerButton.disabled = !workerReady;
    attachDeviceButton.disabled = !workerReady;
    detachDeviceButton.disabled = !workerReady;
    getDeviceDescButton.disabled = !workerReady;
    getConfigDescButton.disabled = !workerReady;
    clearUsbStsButton.disabled = !workerReady;
  };

  const attachControllerButton = el("button", {
    text: "Attach EHCI controller",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.ehciHarness.attachController" });
    },
  }) as HTMLButtonElement;

  const detachControllerButton = el("button", {
    text: "Detach controller",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.ehciHarness.detachController" });
    },
  }) as HTMLButtonElement;

  const attachDeviceButton = el("button", {
    text: "Attach passthrough device",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.ehciHarness.attachDevice" });
    },
  }) as HTMLButtonElement;

  const detachDeviceButton = el("button", {
    text: "Detach device",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.ehciHarness.detachDevice" });
    },
  }) as HTMLButtonElement;

  const getDeviceDescButton = el("button", {
    text: "GET_DESCRIPTOR(Device)",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.ehciHarness.getDeviceDescriptor" });
    },
  }) as HTMLButtonElement;

  const getConfigDescButton = el("button", {
    text: "GET_DESCRIPTOR(Config)",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      worker?.postMessage({ type: "usb.ehciHarness.getConfigDescriptor" });
    },
  }) as HTMLButtonElement;

  const clearUsbStsButton = el("button", {
    text: "Clear USBSTS",
    onclick: () => {
      if ((configManager.getState().effective.vmRuntime ?? "legacy") === "machine") {
        errorLine.textContent = "EHCI passthrough harness is unavailable in vmRuntime=machine.";
        refreshUi();
        return;
      }
      const worker = workerCoordinator.getIoWorker();
      // Clear USBINT | USBERRINT | PCD (bits 0..2).
      worker?.postMessage({ type: "usb.ehciHarness.clearUsbSts", bits: 0x7 });
    },
  }) as HTMLButtonElement;

  const onMessage = (ev: MessageEvent<unknown>): void => {
    if (!isUsbEhciHarnessStatusMessage(ev.data)) return;
    snapshot = ev.data.snapshot;
    refreshUi();
  };

  const ensureAttached = (): void => {
    const vmRuntime = configManager.getState().effective.vmRuntime ?? "legacy";
    if (vmRuntime === "machine") {
      if (attachedIoWorker) attachedIoWorker.removeEventListener("message", onMessage);
      attachedIoWorker = null;
      snapshot = null;
      refreshUi();
      return;
    }

    const ioWorker = workerCoordinator.getIoWorker();
    if (ioWorker === attachedIoWorker) return;
    if (attachedIoWorker) attachedIoWorker.removeEventListener("message", onMessage);
    attachedIoWorker = ioWorker;
    snapshot = null;
    if (attachedIoWorker) attachedIoWorker.addEventListener("message", onMessage);
    refreshUi();
  };

  ensureAttached();
  const attachTimer = globalThis.setInterval(ensureAttached, 250);
  (attachTimer as unknown as { unref?: () => void }).unref?.();
  refreshUi();

  const hint = el("div", {
    class: "hint",
    text:
      "Dev-only smoke test: start workers, select a WebUSB device via the broker panel, then use the EHCI harness controls. " +
      "The harness runs in the I/O worker, emits usb.action messages, and receives usb.completion replies from the main thread UsbBroker.",
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "EHCI passthrough harness (IO worker + UsbBroker)" }),
    hint,
    el(
      "div",
      { class: "row" },
      attachControllerButton,
      detachControllerButton,
      attachDeviceButton,
      detachDeviceButton,
      getDeviceDescButton,
      getConfigDescButton,
      clearUsbStsButton,
    ),
    status,
    irqLine,
    usbStsLine,
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
  const inputStatusLine = el("div", { id: "workers-input-status", class: "hint mono", text: "" });
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

  const screenshotLine = el("div", { class: "mono", text: "" });
  let screenshotInFlight = false;
  let screenshotRequestId = 1;
  const screenshotButton = el("button", {
    text: "Save screenshot (png)",
    onclick: async () => {
      error.textContent = "";
      screenshotInFlight = true;
      update();
      screenshotLine.textContent = "screenshot: capturing…";

      try {
        const gpuWorker = workerCoordinator.getWorker("gpu");
        if (!gpuWorker) {
          throw new Error("GPU worker is not running; start workers before taking a screenshot.");
        }

        const requestId = screenshotRequestId++;
        const response = await new Promise<GpuRuntimeScreenshotResponseMessage>((resolve, reject) => {
          const timeout = window.setTimeout(() => {
            cleanup();
            reject(new Error("Timed out waiting for GPU screenshot response."));
          }, 8000);
          (timeout as unknown as { unref?: () => void }).unref?.();

          const onMessage = (ev: MessageEvent<unknown>) => {
            const msg = ev.data;
            if (!isGpuWorkerMessageBase(msg)) return;
            const typed = msg as GpuRuntimeScreenshotResponseMessage;
            if (typed.type !== "screenshot") return;
            if (typed.requestId !== requestId) return;
            if (!(typed.rgba8 instanceof ArrayBuffer)) return;
            cleanup();
            resolve(typed);
          };

          const cleanup = () => {
            window.clearTimeout(timeout);
            gpuWorker.removeEventListener("message", onMessage);
          };

          gpuWorker.addEventListener("message", onMessage);
          gpuWorker.postMessage({
            protocol: GPU_PROTOCOL_NAME,
            protocolVersion: GPU_PROTOCOL_VERSION,
            type: "screenshot",
            requestId,
            // Exclude the cursor by default; the guest typically uses a hardware cursor,
            // which is not part of the deterministic framebuffer bytes.
            includeCursor: false,
          });
        });

        const ts = new Date().toISOString().replaceAll(":", "-").replaceAll(".", "-");
        const filename = `aero-screenshot-${response.width}x${response.height}-${ts}.png`;
        const rgba8 = new Uint8Array(response.rgba8);
        const blob = await rgba8ToPngBlob(response.width, response.height, rgba8);
        downloadFile(blob, filename);

        screenshotLine.textContent = `screenshot: saved → ${filename}`;
      } catch (err) {
        screenshotLine.textContent = "screenshot: failed";
        error.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        screenshotInFlight = false;
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
    (globalThis as unknown as { __aeroJitDemo?: unknown }).__aeroJitDemo = response;

    if (response.type === "jit:error") {
      jitDemoLine.textContent = `jit: error (${response.code ?? "unknown"}) in ${response.durationMs ?? 0}ms`;
      jitDemoError.textContent = response.message;
      return;
    }

    if (response.type !== "jit:compiled") {
      // This should never happen for `compile()`, but keep the demo resilient to
      // protocol changes.
      jitDemoLine.textContent = `jit: unexpected response (${response.type})`;
      jitDemoError.textContent = JSON.stringify(response);
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
    id: "workers-start",
    text: "Start workers",
    onclick: async () => {
      error.textContent = "";
      booting = true;
      update();
      try {
        // Ensure the static config (if any) has been loaded before starting workers. Otherwise,
        // `AeroConfigManager.init()` may emit an update after we start and trigger an avoidable
        // worker restart.
        await configInitPromise;
        const config = configManager.getState().effective;
        const platformFeatures = forceJitCspBlock.checked ? { ...report, jit_dynamic_wasm: false } : report;
        const diskManager = await diskManagerPromise;
        const selection = await getBootDiskSelection(diskManager);
        // Inform the worker coordinator before starting so it can route audio/mic ring ownership
        // correctly (legacy demo vs legacy VM mode).
        workerCoordinator.setBootDisks(selection.mounts, selection.hdd ?? null, selection.cd ?? null);

        // When no boot disks are mounted, the workers panel runs in a lightweight demo mode
        // (shared framebuffer + input capture). Avoid reserving a full VM-sized guest RAM/VRAM
        // allocation so Playwright smoke tests don't OOM (and so the legacy demo does not
        // accidentally embed large shared buffers into the shared WebAssembly.Memory).
        const isDemoMode = !selection.hdd && !selection.cd;
        const startConfig = isDemoMode
          ? { ...config, guestMemoryMiB: 1, vramMiB: 0 }
          : config;

        workerCoordinator.start(startConfig, { platformFeatures });
        const ioWorker = workerCoordinator.getIoWorker();
        if (ioWorker) {
          // WebUSB passthrough currently targets the legacy IO worker USB stack. In vmRuntime=machine,
          // the IO worker runs in host-only mode (no guest USB controller/device models), so avoid
          // attaching the broker to it (which would otherwise allocate SharedArrayBuffer rings and
          // polling timers that can never be used).
          if ((config.vmRuntime ?? "legacy") !== "machine") {
            usbBroker.attachWorkerPort(ioWorker);
          }
          // WebHID passthrough is currently only supported in the legacy runtime.
          // In machine runtime the IO worker is a host-only stub and will ignore passthrough messages.
          if ((config.vmRuntime ?? "legacy") !== "machine") {
            wireIoWorkerForWebHid(ioWorker, webHidManager);
            void webHidManager.resyncAttachedDevices();
          }
          syncWebHidInputReportRing(ioWorker);
          attachedIoWorker = ioWorker;
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
      } finally {
        booting = false;
      }
      update();
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    id: "workers-stop",
    text: "Stop workers",
    onclick: async () => {
      stopWorkersInputCapture();
      jitClient?.destroy();
      jitClient = null;
      jitClientWorker = null;
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      workerCoordinator.stop();
      useWorkerPresentation = false;
      teardownVgaPresenter();
      if (canvasTransferred) resetVgaCanvas();
      update();
    },
  }) as HTMLButtonElement;

  const restartButton = el("button", {
    id: "workers-restart",
    text: "Restart VM",
    onclick: async () => {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      try {
        await configInitPromise;
        // Keep restart behavior consistent with the initial start button: use the
        // latest disk mounts from DiskManager, even if the user changed them since
        // the last boot.
        const diskManager = await diskManagerPromise;
        const selection = await getBootDiskSelection(diskManager);
        workerCoordinator.setBootDisks(selection.mounts, selection.hdd ?? null, selection.cd ?? null);
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
      }
      try {
        stopWorkersInputCapture();
        workerCoordinator.restart();
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const resetButton = el("button", {
    id: "workers-reset",
    text: "Reset VM",
    onclick: () => {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      stopWorkersInputCapture();
      workerCoordinator.reset("ui");
      update();
    },
  }) as HTMLButtonElement;

  const powerOffButton = el("button", {
    id: "workers-poweroff",
    text: "Power off",
    onclick: () => {
      frameScheduler?.stop();
      frameScheduler = null;
      schedulerWorker = null;
      schedulerFrameStateSab = null;
      schedulerSharedFramebuffer = null;
      stopWorkersInputCapture();
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
    const canvas = el("canvas", { id: "workers-vga-canvas" }) as HTMLCanvasElement;
    canvas.style.width = "640px";
    canvas.style.height = "480px";
    canvas.style.border = "1px solid #333";
    canvas.style.background = "#000";
    canvas.style.imageRendering = "pixelated";
    // Ensure the canvas can receive focus even before InputCapture is attached (important for
    // focus-based keyboard capture when pointer lock is unavailable).
    if (canvas.tabIndex < 0) canvas.tabIndex = 0;
    return canvas;
  };

  let vgaCanvas = createVgaCanvas();
  const vgaCanvasRow = el("div", { class: "row" }, vgaCanvas);
  let canvasTransferred = false;
  let useWorkerPresentation = false;

  function resetVgaCanvas(): void {
    stopWorkersInputCapture();
    // `transferControlToOffscreen()` is one-shot per HTMLCanvasElement. When the
    // worker presentation path is used, recreate the canvas so stop/start cycles
    // continue to work.
    vgaCanvas = createVgaCanvas();
    vgaCanvasRow.replaceChildren(vgaCanvas);
    canvasTransferred = false;
  }

  const vgaInfoLine = el("div", { class: "mono", text: "" });

  let vgaPresenter: SharedLayoutPresenter | null = null;
  let legacyFramebufferInfo: { sab: SharedArrayBuffer; offsetBytes: number } | null = null;
  let schedulerWorker: Worker | null = null;
  let schedulerFrameStateSab: SharedArrayBuffer | null = null;
  let schedulerSharedFramebuffer: { sab: SharedArrayBuffer; offsetBytes: number } | null = null;
  let attachedIoWorker: Worker | null = null;
  let inputCapture: InputCapture | null = null;
  let inputCaptureCanvas: HTMLCanvasElement | null = null;
  let inputCaptureWorker: Worker | null = null;

  function stopWorkersInputCapture(): void {
    const current = inputCapture;
    if (!current) return;
    try {
      current.stop();
    } catch {
      // ignore (worker may already be terminated)
    }
    inputCapture = null;
    inputCaptureCanvas = null;
    inputCaptureWorker = null;
  }

  workerCoordinator.addEventListener("fatal", (ev) => {
    const detail = ev.detail;
    stopWorkersInputCapture();
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

  workerCoordinator.addEventListener("statechange", (ev) => {
    const next = ev.detail.next;
    if (next === "stopped" || next === "restarting" || next === "resetting" || next === "poweredOff" || next === "failed") {
      stopWorkersInputCapture();
    }
  });

  function ensureVgaPresenter(): void {
    const fb = workerCoordinator.getSharedFramebuffer();
    if (!fb) return;

    if (useWorkerPresentation) {
      // Worker owns the canvas; main-thread presenter must be disabled.
      if (vgaPresenter) {
        vgaPresenter.destroy();
        vgaPresenter = null;
      }
      legacyFramebufferInfo = null;
      return;
    }

    const changed =
      !legacyFramebufferInfo ||
      legacyFramebufferInfo.sab !== fb.sab ||
      legacyFramebufferInfo.offsetBytes !== fb.offsetBytes;
    if (changed) {
      legacyFramebufferInfo = fb;
      vgaPresenter?.setSharedFramebuffer(fb);
    }

    if (!vgaPresenter) {
      vgaPresenter = new SharedLayoutPresenter(vgaCanvas, { maxPresentHz: 60 });
      vgaPresenter.setSharedFramebuffer(fb);
      vgaPresenter.start();
    }
  }

  function teardownVgaPresenter(): void {
    if (vgaPresenter) {
      vgaPresenter.destroy();
      vgaPresenter = null;
    }
    legacyFramebufferInfo = null;
    vgaInfoLine.textContent = "";
  }

  function update(): void {
    const statuses = workerCoordinator.getWorkerStatuses();
    const anyActive = Object.values(statuses).some((s) => s.state !== "stopped");
    const config = configManager.getState().effective;
    const vmRuntime = config.vmRuntime ?? "legacy";
    const cpuWorker = workerCoordinator.getCpuWorker();

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

    const screenshotReady = statuses.gpu.state === "ready";
    screenshotButton.disabled = !screenshotReady || screenshotInFlight;
    if (!screenshotReady) {
      // Avoid clobbering a recent "saved" message while the GPU worker is still
      // running; only overwrite the line when the worker is unavailable.
      screenshotLine.textContent = "screenshot: unavailable (GPU worker not ready)";
    } else if (!screenshotInFlight && !screenshotLine.textContent) {
      screenshotLine.textContent = "screenshot: ready";
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
        if (vmRuntime !== "machine") {
          usbBroker.attachWorkerPort(ioWorker);
        }
        if (vmRuntime !== "machine") {
          wireIoWorkerForWebHid(ioWorker, webHidManager);
          void webHidManager.resyncAttachedDevices();
        }
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

    const bootDiskSelection = workerCoordinator.getBootDisks();
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
      if (bootDiskSelection.mounts.hddId || bootDiskSelection.mounts.cdId) {
        const boot = bootDiskSelection.bootDevice ?? (bootDiskSelection.mounts.cdId ? "cdrom" : "hdd");
        parts.push(`boot=${boot}`);
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
      const fb = legacyFramebufferInfo;
      if (fb) {
        try {
          const header = new Int32Array(fb.sab, fb.offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
          const w = Atomics.load(header, SharedFramebufferHeaderIndex.WIDTH);
          const h = Atomics.load(header, SharedFramebufferHeaderIndex.HEIGHT);
          const seq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
          vgaInfoLine.textContent = `legacy ${w}x${h} seq=${seq}`;
        } catch {
          // ignore if the framebuffer isn't initialized yet.
          vgaInfoLine.textContent = "";
        }
      }
    } else {
      teardownVgaPresenter();
    }

    // Workers panel input capture. In machine runtime, input must be routed to the CPU worker
    // (the I/O worker may be host-only and cannot inject into guest devices).
    const inputWorker = vmRuntime === "machine" ? cpuWorker : ioWorker;
    const shouldCapture = anyActive && inputWorker !== null;
    if (!shouldCapture) {
      stopWorkersInputCapture();
    } else if (inputWorker !== inputCaptureWorker || vgaCanvas !== inputCaptureCanvas) {
      // Recreate capture when the target worker is restarted or when the canvas is replaced.
      stopWorkersInputCapture();
    }
    if (shouldCapture && !inputCapture && inputWorker) {
      const capture = new InputCapture(vgaCanvas, inputWorker);
      try {
        capture.start();
        inputCapture = capture;
        inputCaptureCanvas = vgaCanvas;
        inputCaptureWorker = inputWorker;
      } catch {
        // ignore
      }
    }

    const inputTargetRole = vmRuntime === "machine" ? "cpu" : "io";
    const inputTargetState = inputTargetRole === "cpu" ? statuses.cpu.state : statuses.io.state;
    inputStatusLine.textContent =
      `input: click canvas to focus + request pointer lock. ` +
      `pointerLock=${inputCapture?.pointerLocked ? "yes" : "no"}  ` +
      `targetWorker=${inputTargetRole}:${inputTargetState}  ` +
      `ioWorker=${statuses.io.state}  ` +
      `ioBatches=${workerCoordinator.getIoInputBatchCounter()}  ` +
      `ioEvents=${workerCoordinator.getIoInputEventCounter()}`;

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
    el("div", { class: "row" }, snapshotSaveButton, snapshotLoadButton, screenshotButton),
    snapshotLine,
    screenshotLine,
    vgaCanvasRow,
    inputStatusLine,
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

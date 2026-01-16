// NOTE: Repo-root Vite app entrypoint.
//
// This is the canonical browser host used by CI/Playwright. The `web/` directory
// still houses shared runtime modules and WASM build tooling; its own Vite
// entrypoint (`web/index.html`) is legacy/experimental.
import './style.css';

import { formatOneLineError, formatOneLineUtf8 } from './text.js';

import { installPerfHud } from '../web/src/perf/hud_entry';
import { perf } from '../web/src/perf/perf';
import { installAeroGlobal } from '../web/src/runtime/aero_global';
import { installNetTraceBackendOnAeroGlobal } from '../web/src/net/trace_backend';
import { installNetTraceUI, type NetTraceBackend } from '../web/src/net/trace_ui';
import { installBootDeviceBackendOnAeroGlobal } from '../web/src/runtime/boot_device_backend';
import { RuntimeDiskClient } from '../web/src/storage/runtime_disk_client';

import { createHotspotsPanel } from './ui/hud_hotspots.js';
import type { HotspotEntry, PerfExport as HotspotPerfExport } from './perf/aero_perf.js';

import { createAudioOutput } from './platform/audio';
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from './platform/features';
import { importFileToOpfs } from './platform/opfs';
import { requestWebGpuDevice } from './platform/webgpu';
import { VmCoordinator } from './emulator/vmCoordinator.js';
import { MicCapture, micRingBufferReadInto, type MicRingBuffer } from '../web/src/audio/mic_capture';
import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX as MIC_DROPPED_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  READ_POS_INDEX as MIC_READ_POS_INDEX,
  WRITE_POS_INDEX as MIC_WRITE_POS_INDEX,
} from '../web/src/audio/mic_ring.js';
import { startSyntheticMic, type SyntheticMicSource } from '../web/src/audio/synthetic_mic';
import type { AeroConfig } from '../web/src/config/aero_config';
import { WorkerCoordinator } from '../web/src/runtime/coordinator';
import { StatusIndex } from '../web/src/runtime/shared_layout';
import { decodeInputBackendStatus } from '../web/src/input/input_backend_status';
import { explainWebUsbError, formatWebUsbError } from '../web/src/platform/webusb_troubleshooting';

const MAX_UI_ERROR_NAME_BYTES = 128;
const MAX_UI_ERROR_MESSAGE_BYTES = 512;

const formatUiErrorMessage = (err: unknown): string => formatOneLineError(err, MAX_UI_ERROR_MESSAGE_BYTES);

declare global {
  interface Window {
    __aeroUiTicks?: number;
    __aeroVm?: VmCoordinator;
  }
}

// Install the Perf HUD overlay + global perf API for automation.
// CI relies on `window.aero.perf.captureStart/captureStop/export()` to produce
// `perf_export.json` via `tools/perf/run.mjs` against `npm run preview`.
installPerfHud();
perf.installGlobalApi();
const harnessSearchParams = new URLSearchParams(location.search);
if (harnessSearchParams.has('trace')) perf.traceStart();

// Nice-to-have: allow forcing the IO worker's input backend selection from the repo-root Vite harness:
// - `?kbd=ps2|usb|virtio|auto`
// - `?mouse=ps2|usb|virtio|auto`
const harnessInputBackendOverrides: Partial<Pick<AeroConfig, "forceKeyboardBackend" | "forceMouseBackend">> = {};
const parseHarnessInputBackendOverride = (value: string | null): AeroConfig["forceKeyboardBackend"] | undefined => {
  if (value === null) return undefined;
  const v = value.trim().toLowerCase();
  if (v === "" || v === "default") return "auto";
  if (v === "auto" || v === "ps2" || v === "usb" || v === "virtio") {
    return v as AeroConfig["forceKeyboardBackend"];
  }
  return undefined;
};
const forcedKbd = parseHarnessInputBackendOverride(harnessSearchParams.get("kbd"));
if (forcedKbd !== undefined) harnessInputBackendOverrides.forceKeyboardBackend = forcedKbd;
const forcedMouse = parseHarnessInputBackendOverride(harnessSearchParams.get("mouse"));
if (forcedMouse !== undefined) harnessInputBackendOverrides.forceMouseBackend = forcedMouse;

// Install optional `window.aero.bench` helpers so automation can invoke
// microbenchmarks without requiring the emulator/guest OS.
installAeroGlobal();

// Updated by the microphone UI and read by the VM UI so that new VM instances
// automatically inherit the current mic attachment (if any).
// `sampleRate` is the actual capture sample rate (AudioContext.sampleRate).
let micAttachment: { ringBuffer: SharedArrayBuffer; sampleRate: number } | null = null;

type CpuWorkerToMainMessage =
  | { type: 'CpuWorkerReady' }
  // Keep the result payload flexible: the JIT smoke pipeline evolves quickly
  // (Tier-0/Tier-1 counters, compile/install metadata, etc.). Playwright asserts
  // on the concrete fields at runtime; the UI only needs the message as JSON.
  | ({ type: 'CpuWorkerResult' } & Record<string, unknown>)
  | { type: 'CpuWorkerError'; reason: string };

type CpuWorkerStartMessage = {
  type: 'CpuWorkerStart';
  iterations?: number;
  threshold?: number;
};

declare global {
  interface Window {
    __jit_smoke_result?: CpuWorkerToMainMessage;
  }
}

function formatByteSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let n = bytes;
  let unit = 0;
  while (n >= 1024 && unit < units.length - 1) {
    n /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : n >= 100 ? 0 : n >= 10 ? 1 : 2;
  return `${n.toFixed(digits)} ${units[unit]}`;
}

async function runJitSmokeTest(output: HTMLPreElement): Promise<void> {
  output.textContent = '';
  window.__jit_smoke_result = undefined;

  let cpuWorker: Worker;
  try {
    cpuWorker = new Worker(new URL('./workers/cpu-worker.ts', import.meta.url), {
      type: 'module',
    });
  } catch (err) {
    const reason = formatUiErrorMessage(err);
    window.__jit_smoke_result = { type: 'CpuWorkerError', reason };
    output.textContent = reason;
    return;
  }

  const result = await new Promise<CpuWorkerToMainMessage>((resolve) => {
    const settle = (msg: CpuWorkerToMainMessage) => {
      resolve(msg);
      cpuWorker.terminate();
    };

    let started = false;
    const start: CpuWorkerStartMessage = { type: 'CpuWorkerStart' };
    const maybeStart = () => {
      if (started) return;
      started = true;
      cpuWorker.postMessage(start);
    };

    cpuWorker.addEventListener('message', (ev: MessageEvent<CpuWorkerToMainMessage>) => {
      const msg = ev.data;
      if (msg.type === 'CpuWorkerReady') {
        maybeStart();
        return;
      }
      if (msg.type === 'CpuWorkerResult' || msg.type === 'CpuWorkerError') {
        settle(msg);
      }
    });

    cpuWorker.addEventListener('error', (ev) => {
      settle({ type: 'CpuWorkerError', reason: formatUiErrorMessage(ev) });
    });

    cpuWorker.addEventListener('messageerror', () => {
      settle({ type: 'CpuWorkerError', reason: 'worker message deserialization failed' });
    });

    // Some worker implementations no longer send an explicit `CpuWorkerReady`
    // handshake (or send it only for debug); post the start message eagerly so
    // the smoke test still runs.
    maybeStart();
  });

  window.__jit_smoke_result = result;

  output.textContent = JSON.stringify(result, null, 2);
}

type WebUsbProbePending = {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
  timeoutHandle: number;
};

let webUsbProbeWorker: Worker | null = null;
let webUsbProbeNextId = 1;
const webUsbProbePending = new Map<number, WebUsbProbePending>();

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

function renderCapabilityTable(report: PlatformFeatureReport): HTMLTableElement {
  const orderedKeys: Array<keyof PlatformFeatureReport> = [
    'crossOriginIsolated',
    'sharedArrayBuffer',
    'wasmSimd',
    'wasmThreads',
    'jit_dynamic_wasm',
    'webgpu',
    'webusb',
    'opfs',
    'opfsSyncAccessHandle',
    'audioWorklet',
    'offscreenCanvas',
  ];

  const tbody = el("tbody");
  for (const key of orderedKeys) {
    const val = report[key];
    tbody.append(
      el("tr", {}, el("th", { text: key }), el("td", { class: val ? "ok" : "bad", text: val ? "supported" : "missing" })),
    );
  }

  return el("table", {}, el("thead", {}, el("tr", {}, el("th", { text: "feature" }), el("th", { text: "status" }))), tbody);
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
        output.textContent = formatUiErrorMessage(err);
      }
    },
  });

  return el("div", { class: "panel" }, el("h2", { text: "WebGPU" }), el("div", { class: "row" }, button), output);
}

function parseUsbId(text: string): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const normalized = trimmed.toLowerCase().startsWith('0x') ? trimmed.slice(2) : trimmed;
  const value = Number.parseInt(normalized, 16);
  if (!Number.isFinite(value)) return null;
  if (value < 0 || value > 0xffff) return null;
  return value;
}

function summarizeUsbDevice(
  device: unknown,
): Record<string, unknown> | null {
  if (!device || typeof device !== 'object') return null;

  const out: Record<string, unknown> = {};
  const fieldErrors: Record<string, string> = {};

  const read = (field: string) => {
    try {
      out[field] = (device as Record<string, unknown>)[field];
    } catch (err) {
      fieldErrors[field] = formatWebUsbError(err);
    }
  };

  read('productName');
  read('manufacturerName');
  read('serialNumber');
  read('vendorId');
  read('productId');
  read('opened');

  if (Object.keys(fieldErrors).length) out.fieldErrors = fieldErrors;
  return out;
}

function rejectAllWebUsbProbePending(err: unknown): void {
  for (const [id, entry] of webUsbProbePending.entries()) {
    webUsbProbePending.delete(id);
    window.clearTimeout(entry.timeoutHandle);
    entry.reject(err);
  }
}

function resetWebUsbProbeWorker(): void {
  if (webUsbProbeWorker) {
    webUsbProbeWorker.terminate();
    webUsbProbeWorker = null;
  }
}

function ensureWebUsbProbeWorker(): Worker {
  if (webUsbProbeWorker) return webUsbProbeWorker;

  const worker = new Worker(new URL('./workers/webusb-probe-worker.ts', import.meta.url), { type: 'module' });
  webUsbProbeWorker = worker;

  worker.addEventListener('message', (ev: MessageEvent) => {
    const data = ev.data as { id?: unknown } | null;
    const id = typeof data?.id === 'number' ? data.id : null;
    if (id === null) return;
    const entry = webUsbProbePending.get(id);
    if (!entry) return;
    webUsbProbePending.delete(id);
    window.clearTimeout(entry.timeoutHandle);
    entry.resolve(ev.data);
  });

  worker.addEventListener('messageerror', () => {
    rejectAllWebUsbProbePending(new Error('WebUSB probe worker message deserialization failed'));
    resetWebUsbProbeWorker();
  });

  worker.addEventListener('error', (ev) => {
    rejectAllWebUsbProbePending(new Error(formatUiErrorMessage(ev)));
    resetWebUsbProbeWorker();
  });

  return worker;
}

async function runWebUsbProbeWorker(
  msg: unknown,
  { timeoutMs = 10_000, transfer = [] }: { timeoutMs?: number; transfer?: Transferable[] } = {},
): Promise<unknown> {
  const worker = ensureWebUsbProbeWorker();
  const id = webUsbProbeNextId++;

  const payload: Record<string, unknown> =
    msg && typeof msg === 'object' && !Array.isArray(msg) ? { ...(msg as Record<string, unknown>) } : { value: msg };
  payload.id = id;

  return await new Promise((resolve, reject) => {
    const timeoutHandle = window.setTimeout(() => {
      webUsbProbePending.delete(id);
      reject(new Error(`WebUSB probe worker timed out after ${timeoutMs}ms`));
    }, timeoutMs);
    (timeoutHandle as unknown as { unref?: () => void }).unref?.();

    webUsbProbePending.set(id, { resolve, reject, timeoutHandle });

    try {
      worker.postMessage(payload, transfer);
    } catch (err) {
      webUsbProbePending.delete(id);
      window.clearTimeout(timeoutHandle);
      reject(err);
    }
  });
}

function renderWebUsbPanel(report: PlatformFeatureReport): HTMLElement {
  const info = el('pre', { text: '' });
  const output = el('pre', { text: '' });
  const workerGetDevicesBefore = el('pre', { text: '' });
  const workerGetDevicesAfter = el('pre', { text: '' });
  const workerMatchAndOpenOutput = el('pre', { text: '' });
  const errorTitle = el('div', { class: 'bad', text: '' });
  const errorDetails = el('div', { class: 'hint', text: '' });
  const errorRaw = el('pre', { class: 'mono', text: '' });
  const errorHints = el('ul');
  const acceptAllDevicesInput = el('input', { type: 'checkbox' }) as HTMLInputElement;
  const vendorIdInput = el('input', {
    type: 'text',
    placeholder: '0x1234 (optional)',
    style: 'min-width: 0; width: 12ch;',
  }) as HTMLInputElement;
  const productIdInput = el('input', {
    type: 'text',
    placeholder: '0x5678 (optional)',
    style: 'min-width: 0; width: 12ch;',
  }) as HTMLInputElement;
  const interfaceSelect = el('select') as HTMLSelectElement;

  let selectedDevice: USBDevice | null = null;
  let selectedSummary: Record<string, unknown> | null = null;
  let workerProbe: Record<string, unknown> | null = null;
  let workerProbeError: { name: string; message: string } | null = null;

  function serializeError(err: unknown): { name: string; message: string } {
    const message = formatUiErrorMessage(err);
    let nameRaw: string | null = null;
    if (err && typeof err === "object") {
      try {
        const maybeName = (err as { name?: unknown }).name;
        if (typeof maybeName === "string") nameRaw = maybeName;
      } catch {
        // ignore getters throwing
      }
    }
    const name = formatOneLineUtf8(nameRaw ?? "Error", MAX_UI_ERROR_NAME_BYTES) || "Error";
    return { name, message };
  }

  function clearError(): void {
    errorTitle.textContent = '';
    errorDetails.textContent = '';
    errorRaw.textContent = '';
    errorHints.replaceChildren();
  }

  function showError(err: unknown): void {
    const explained = explainWebUsbError(err);
    errorTitle.textContent = explained.title;
    errorDetails.textContent = explained.details ?? '';
    errorRaw.textContent = formatWebUsbError(err);
    errorHints.replaceChildren(...explained.hints.map((h) => el('li', { text: h })));
  }

  function renderJson(pre: HTMLPreElement, value: unknown): void {
    try {
      pre.textContent = JSON.stringify(value, null, 2);
    } catch (err) {
      pre.textContent = formatWebUsbError(err);
    }
  }

  function runWorkerGetDevicesSnapshot(target: HTMLPreElement, label: string): void {
    target.textContent = `Probing worker getDevices() (${label})…`;
    void runWebUsbProbeWorker({ type: 'probe' })
      .then((resp) => renderJson(target, resp))
      .catch((err) => {
        renderJson(target, { ok: false, error: serializeError(err) });
      });
  }

  function refreshInterfaceSelect(): void {
    interfaceSelect.replaceChildren();
    if (!selectedDevice) {
      interfaceSelect.append(el('option', { value: '', text: '(no device selected)' }));
      return;
    }

    // Prefer the active configuration if selected; otherwise fall back to the first
    // descriptor configuration so we can still show interface info pre-open.
    const cfg = selectedDevice.configuration ?? selectedDevice.configurations?.[0] ?? null;
    const ifaces = cfg?.interfaces ?? [];
    if (!Array.isArray(ifaces) || ifaces.length === 0) {
      interfaceSelect.append(el('option', { value: '', text: '(no interfaces found)' }));
      return;
    }

    for (const iface of ifaces) {
      const num = iface?.interfaceNumber;
      const alt = iface?.alternates?.[0];
      const cls = typeof alt?.interfaceClass === 'number' ? alt.interfaceClass : null;
      const sub = typeof alt?.interfaceSubclass === 'number' ? alt.interfaceSubclass : null;
      const proto = typeof alt?.interfaceProtocol === 'number' ? alt.interfaceProtocol : null;
      const label =
        cls === null
          ? `#${num}`
          : `#${num} (class=0x${cls.toString(16).padStart(2, '0')} sub=0x${(sub ?? 0).toString(16).padStart(2, '0')} proto=0x${(proto ?? 0).toString(16).padStart(2, '0')})`;

      interfaceSelect.append(el('option', { value: String(num), text: label }));
    }
  }

  function updateInfo(): void {
    const userActivation = (navigator as unknown as { userActivation?: { isActive?: boolean; hasBeenActive?: boolean } })
      .userActivation;
    const mainUsb = (navigator as Navigator & { usb?: USB }).usb;
    const mainHasRequestDevice = typeof mainUsb?.requestDevice === 'function';
    const liveSummary = selectedDevice ? summarizeUsbDevice(selectedDevice) : selectedSummary;
    if (selectedDevice) selectedSummary = liveSummary;

    const workerHasUsb = typeof workerProbe?.hasUsb === 'boolean' ? (workerProbe.hasUsb as boolean) : null;
    const workerHasRequestDevice =
      typeof workerProbe?.hasRequestDevice === 'boolean' ? (workerProbe.hasRequestDevice as boolean) : null;
    const workerRequestDevice =
      workerProbe && typeof workerProbe.requestDevice === 'object' && workerProbe.requestDevice !== null
        ? (workerProbe.requestDevice as { ok?: unknown; error?: unknown })
        : null;
    const workerRequestDeviceError =
      workerRequestDevice &&
      workerRequestDevice.ok === false &&
      typeof workerRequestDevice.error === 'object' &&
      workerRequestDevice.error !== null
        ? (workerRequestDevice.error as { name?: unknown; message?: unknown })
        : null;
    const workerRequestDeviceText = (() => {
      if (workerRequestDevice && workerRequestDevice.ok === true) return 'resolved';
      if (workerRequestDevice && workerRequestDevice.ok === false) {
        const name = typeof workerRequestDeviceError?.name === 'string' ? workerRequestDeviceError.name : null;
        const message = typeof workerRequestDeviceError?.message === 'string' ? workerRequestDeviceError.message : null;
        if (name && message) return `rejected: ${name}: ${message}`;
        if (name) return `rejected: ${name}`;
        return 'rejected';
      }
      return 'not run';
    })();

    info.textContent =
      `isSecureContext=${(globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true}\n` +
      `navigator.usb=${report.webusb ? 'present' : 'missing'}\n` +
      `main requestDevice=${mainHasRequestDevice ? 'function' : 'missing'}\n` +
      `worker navigator.usb=${workerHasUsb === null ? '(probe pending)' : workerHasUsb ? 'present' : 'missing'}\n` +
      `worker requestDevice=${workerHasRequestDevice === null ? '(probe pending)' : workerHasRequestDevice ? 'function' : 'missing'} (${workerRequestDeviceText})\n` +
      `workerProbeError=${workerProbeError ? `${workerProbeError.name}: ${workerProbeError.message}` : 'none'}\n` +
      `userActivation.isActive=${userActivation?.isActive ?? 'n/a'}\n` +
      `userActivation.hasBeenActive=${userActivation?.hasBeenActive ?? 'n/a'}\n` +
      `selectedDevice=${liveSummary ? JSON.stringify(liveSummary) : 'none'}\n`;

    const hasSelected = !!selectedDevice;
    const enabled = report.webusb && hasSelected;
    openButton.disabled = !enabled;
    closeButton.disabled = !enabled;
    claimButton.disabled = !enabled;
    interfaceSelect.disabled = !enabled;
    refreshInterfaceSelect();
  }

  function updateFilterInputs(): void {
    const disabled = acceptAllDevicesInput.checked;
    vendorIdInput.disabled = disabled;
    productIdInput.disabled = disabled;
  }
  acceptAllDevicesInput.addEventListener('change', updateFilterInputs);
  updateFilterInputs();

  async function runWorkerProbe(): Promise<void> {
    output.textContent = '';
    clearError();
    workerProbe = null;
    workerProbeError = null;
    updateInfo();
    try {
      const resp = await runWebUsbProbeWorker({ type: 'probe' });
      if (resp && typeof resp === 'object' && !Array.isArray(resp)) {
        const msg = resp as { type?: unknown; report?: unknown; error?: unknown };
        if (msg.type === 'probe-result' && msg.report && typeof msg.report === 'object') {
          workerProbe = msg.report as Record<string, unknown>;
        } else if (msg.type === 'error' && msg.error && typeof msg.error === 'object') {
          const errObj = msg.error as { name?: unknown; message?: unknown };
          workerProbeError = {
            name: typeof errObj.name === 'string' ? errObj.name : 'Error',
            message: typeof errObj.message === 'string' ? errObj.message : String(errObj),
          };
        }
      }
      output.textContent = JSON.stringify(resp, null, 2);
      updateInfo();
    } catch (err) {
      workerProbeError = serializeError(err);
      showError(err);
      output.textContent = JSON.stringify({ ok: false, error: serializeError(err) }, null, 2);
      updateInfo();
    }
  }

  const requestButton = el('button', {
    text: 'Request USB device (chooser)',
    onclick: async () => {
      output.textContent = '';
      clearError();
      workerGetDevicesAfter.textContent = '';
      workerMatchAndOpenOutput.textContent = '';
      selectedDevice = null;
      selectedSummary = null;
      updateInfo();

      if (!report.webusb) {
        output.textContent = 'WebUSB is unavailable (navigator.usb is undefined).';
        return;
      }

      const acceptAll = acceptAllDevicesInput.checked;
      const vendorId = acceptAll ? null : parseUsbId(vendorIdInput.value);
      const productId = acceptAll ? null : parseUsbId(productIdInput.value);
      if (!acceptAll && productId !== null && vendorId === null) {
        output.textContent = 'productId filter requires vendorId.';
        return;
      }

      const usb = (navigator as unknown as { usb?: USB }).usb;
      if (!usb || typeof usb.requestDevice !== 'function') {
        output.textContent = 'navigator.usb.requestDevice is unavailable in this context.';
        return;
      }

      try {
        // Must be called directly from the user gesture handler (transient user activation).
        type UsbRequestDeviceOptions = USBDeviceRequestOptions & { acceptAllDevices?: boolean };
        const options: UsbRequestDeviceOptions = (() => {
          if (acceptAllDevicesInput.checked) {
            return { filters: [], acceptAllDevices: true };
          }

          // Note: some Chromium versions require at least one filter; `{}` is a best-effort "match all"
          // filter for probing. If this fails, specify vendorId/productId explicitly.
          const filters: USBDeviceFilter[] = [];
          if (vendorId !== null) {
            const filter: USBDeviceFilter = { vendorId };
            if (productId !== null) filter.productId = productId;
            filters.push(filter);
          } else {
            filters.push({});
          }
          return { filters };
        })();

        // Snapshot the worker's pre-grant getDevices() view (do not await; preserve user gesture).
        runWorkerGetDevicesSnapshot(workerGetDevicesBefore, 'before_requestDevice');

        selectedDevice = await usb.requestDevice(options);
        selectedSummary = summarizeUsbDevice(selectedDevice);
        updateInfo();

        const criteria: { vendorId: number; productId: number; serialNumber?: string } | null = (() => {
          const vendorIdValue = selectedDevice?.vendorId;
          const productIdValue = selectedDevice?.productId;
          if (typeof vendorIdValue !== 'number' || typeof productIdValue !== 'number') return null;
          const out: { vendorId: number; productId: number; serialNumber?: string } = {
            vendorId: vendorIdValue,
            productId: productIdValue,
          };
          try {
            const serial = selectedDevice?.serialNumber;
            if (typeof serial === 'string' && serial.length) out.serialNumber = serial;
          } catch {
            // ignore
          }
          return out;
        })();

        const results: Record<string, unknown> = { selected: selectedSummary };

        // 1) Structured clone attempt.
        try {
          const resp = await runWebUsbProbeWorker({ type: 'device', device: selectedDevice });
          results.clone = { ok: true, response: resp };
        } catch (err) {
          results.clone = { ok: false, error: serializeError(err) };
        }

        // 2) Transfer attempt (via transfer list).
        try {
          const resp = await runWebUsbProbeWorker(
            { type: 'device', device: selectedDevice },
            { transfer: [selectedDevice as unknown as Transferable] },
          );
          results.transfer = { ok: true, response: resp };
          // If transfer succeeds, the device may no longer be usable on the main thread.
          selectedDevice = null;
        } catch (err) {
          results.transfer = { ok: false, error: serializeError(err) };
        }

        // 3) Worker getDevices() view after requestDevice() grant.
        runWorkerGetDevicesSnapshot(workerGetDevicesAfter, 'after_requestDevice');

        // 4) Worker fallback: match by stable criteria, then open+close without receiving USBDevice.
        if (!criteria) {
          workerMatchAndOpenOutput.textContent = 'Cannot run match_and_open: missing vendorId/productId on selected device.';
          results.match_and_open = { ok: false, error: { name: 'Error', message: 'Missing vendorId/productId on selected device.' } };
        } else {
          results.matchCriteria = criteria;
          try {
            const resp = await runWebUsbProbeWorker({ type: 'match_and_open', criteria });
            results.match_and_open = { ok: true, response: resp };
            renderJson(workerMatchAndOpenOutput, resp);
          } catch (err) {
            results.match_and_open = { ok: false, error: serializeError(err) };
            renderJson(workerMatchAndOpenOutput, { ok: false, error: serializeError(err) });
          }
        }

        updateInfo();
        output.textContent = JSON.stringify(results, null, 2);
      } catch (err) {
        showError(err);
      }
    },
  });

  const listButton = el('button', {
    text: 'List permitted devices (getDevices)',
    onclick: async () => {
      output.textContent = '';
      clearError();
      if (!report.webusb) {
        output.textContent = 'WebUSB is unavailable (navigator.usb is undefined).';
        return;
      }
      const usb = (navigator as unknown as { usb?: USB }).usb;
      if (!usb || typeof usb.getDevices !== 'function') {
        output.textContent = 'navigator.usb.getDevices is unavailable in this context.';
        return;
      }

      try {
        const devices = await usb.getDevices();
        output.textContent = JSON.stringify(
          {
            count: Array.isArray(devices) ? devices.length : null,
            devices: Array.isArray(devices) ? devices.map(summarizeUsbDevice) : null,
          },
          null,
          2,
        );
      } catch (err) {
        showError(err);
      }
    },
  });

  const openButton = el('button', {
    text: 'Open selected device',
    onclick: async () => {
      output.textContent = '';
      clearError();
      if (!selectedDevice) {
        output.textContent = 'Select a device first (Request USB device).';
        return;
      }
      try {
        if (!selectedDevice.opened) {
          await selectedDevice.open();
        }
        updateInfo();
        output.textContent = 'Device opened.';
      } catch (err) {
        showError(err);
      }
    },
  }) as HTMLButtonElement;

  const closeButton = el('button', {
    text: 'Close selected device',
    onclick: async () => {
      output.textContent = '';
      clearError();
      if (!selectedDevice) {
        output.textContent = 'Select a device first (Request USB device).';
        return;
      }
      try {
        await selectedDevice.close();
        updateInfo();
        output.textContent = 'Device closed.';
      } catch (err) {
        showError(err);
      }
    },
  }) as HTMLButtonElement;

  const claimButton = el('button', {
    text: 'Claim interface',
    onclick: async () => {
      output.textContent = '';
      clearError();
      if (!selectedDevice) {
        output.textContent = 'Select a device first (Request USB device).';
        return;
      }

      try {
        if (!selectedDevice.opened) {
          await selectedDevice.open();
        }
      } catch (err) {
        showError(err);
        updateInfo();
        return;
      }

      try {
        if (!selectedDevice.configuration) {
          const cfg = selectedDevice.configurations?.[0]?.configurationValue ?? 1;
          await selectedDevice.selectConfiguration(cfg);
        }
      } catch (err) {
        showError(err);
        updateInfo();
        return;
      }

      updateInfo();
      const ifaceNum = Number.parseInt(interfaceSelect.value, 10);
      if (!Number.isFinite(ifaceNum)) {
        output.textContent = 'Select an interface first.';
        return;
      }

      try {
        await selectedDevice.claimInterface(ifaceNum);
        output.textContent = `Claimed interface ${ifaceNum}.`;
      } catch (err) {
        showError(err);
      }
    },
  }) as HTMLButtonElement;

  const workerProbeButton = el('button', {
    text: 'Probe worker WebUSB (WorkerNavigator.usb)',
    onclick: async () => {
      await runWorkerProbe();
    },
  });

  // Initialize info + control state.
  updateInfo();
  // Probe worker-side WebUSB semantics on load so the panel reports both main + worker support.
  void runWorkerProbe();
  runWorkerGetDevicesSnapshot(workerGetDevicesBefore, 'startup');

  return el(
    'div',
    { class: 'panel', id: 'webusb' },
    el('h2', { text: 'WebUSB (probe)' }),
    el(
      'div',
      { class: 'mono' },
      'Note: requestDevice() requires a user gesture on the main thread; user activation does not propagate to workers.',
    ),
    info,
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'acceptAllDevices:' }),
      acceptAllDevicesInput,
      el('label', { text: 'vendorId:' }),
      vendorIdInput,
      el('label', { text: 'productId:' }),
      productIdInput,
      requestButton,
      listButton,
    ),
    el('div', { class: 'row' }, openButton, closeButton, interfaceSelect, claimButton),
    el('div', { class: 'row' }, workerProbeButton),
    output,
    errorTitle,
    errorDetails,
    errorRaw,
    errorHints,
    el('h3', { text: 'Worker permission sharing (getDevices + match_and_open)' }),
    el('h4', { text: 'Worker getDevices() BEFORE requestDevice()' }),
    workerGetDevicesBefore,
    el('h4', { text: 'Worker getDevices() AFTER requestDevice()' }),
    workerGetDevicesAfter,
    el('h4', { text: 'Worker match_and_open result' }),
    workerMatchAndOpenOutput,
  );
}

function renderOpfsPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;
  const destPathInput = el("input", { type: "text", value: "images/disk.img" }) as HTMLInputElement;
  const fileInput = el("input", { type: "file" }) as HTMLInputElement;

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
        await importFileToOpfs(file, destPath, ({ writtenBytes, totalBytes }) => {
          progress.value = totalBytes ? writtenBytes / totalBytes : 0;
          status.textContent = `Writing ${writtenBytes.toLocaleString()} / ${totalBytes.toLocaleString()} bytes…`;
        });
        status.textContent = `Imported to OPFS: ${destPath}`;
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
      }
    },
  });

  fileInput.addEventListener("change", () => {
    const file = fileInput.files?.[0];
    if (file) destPathInput.value = `images/${file.name}`;
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "OPFS (disk image import)" }),
    el(
      "div",
      { class: "row" },
      el("label", { text: "File:" }),
      fileInput,
      el("label", { text: "Dest path:" }),
      destPathInput,
      importButton,
      progress,
    ),
    status,
  );
}

function renderAudioPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  let toneTimer: number | null = null;
  let tonePhase = 0;
  const workerCoordinator = new WorkerCoordinator();
  // Expose for Playwright E2E tests that need to drive low-level worker/device plumbing.
  try {
    const g = globalThis as unknown;
    if (g && (typeof g === "object" || typeof g === "function")) {
      (g as { __aeroWorkerCoordinator?: unknown }).__aeroWorkerCoordinator = workerCoordinator;
    }
  } catch {
    // Best-effort only.
  }
  installNetTraceBackendOnAeroGlobal(workerCoordinator);
  installBootDeviceBackendOnAeroGlobal(workerCoordinator);
  let hdaDemoWorker: Worker | null = null;
  let hdaDemoStats: { [k: string]: unknown } | null = null;
  let virtioSndDemoWorker: Worker | null = null;
  let virtioSndDemoStats: { [k: string]: unknown } | null = null;
  let hdaPciDeviceTimer: number | null = null;
  let loopbackTimer: number | null = null;
  let syntheticMic: SyntheticMicSource | null = null;
  let hdaCaptureRequestId = 1;

  function sleepMs(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = window.setTimeout(resolve, ms);
      (timer as unknown as { unref?: () => void }).unref?.();
    });
  }

  function stopTone() {
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  function stopHdaPciDevice(): void {
    if (hdaPciDeviceTimer !== null) {
      window.clearInterval(hdaPciDeviceTimer);
      hdaPciDeviceTimer = null;
    }
    // Best-effort: ask the CPU worker to stop the HDA playback stream so it doesn't keep running
    // in the background while other harness demos reuse the same worker runtime.
    try {
      workerCoordinator.getWorker("cpu")?.postMessage({ type: "audioOutputHdaPciDevice.stop" });
    } catch {
      // ignore
    }
    // Detach the AudioWorklet ring from the worker runtime and restore the default
    // routing policy (demo mode: CPU worker; VM mode: IO worker).
    try {
      workerCoordinator.setAudioRingBuffer(null, 0, 0, 0);
      workerCoordinator.setAudioRingBufferOwner(null);
    } catch {
      // ignore best-effort detach/reset
    }
    (globalThis as typeof globalThis & { __aeroAudioOutputHdaPciDevice?: unknown }).__aeroAudioOutputHdaPciDevice = undefined;
  }

  function stopLoopback(): void {
    if (loopbackTimer !== null) {
      window.clearInterval(loopbackTimer);
      loopbackTimer = null;
    }
    syntheticMic?.stop();
    syntheticMic = null;
    workerCoordinator.setMicrophoneRingBuffer(null, 0);
    // Restore default routing after the loopback demo (demo mode: CPU worker; VM mode: IO worker).
    workerCoordinator.setMicrophoneRingBufferOwner(null);
    workerCoordinator.setAudioOutputRingBuffer(null, 0, 0, 0);
    workerCoordinator.setAudioRingBufferOwner(null);
  }

  function stopHdaDemo(): void {
    if (!hdaDemoWorker) return;
    hdaDemoWorker.postMessage({ type: "audioOutputHdaDemo.stop" });
    hdaDemoWorker.terminate();
    hdaDemoWorker = null;
    hdaDemoStats = null;
    (globalThis as typeof globalThis & { __aeroAudioHdaDemoStats?: unknown }).__aeroAudioHdaDemoStats = undefined;
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
    (globalThis as typeof globalThis & { __aeroAudioVirtioSndDemoStats?: unknown }).__aeroAudioVirtioSndDemoStats = undefined;
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  function startTone(output: Exclude<Awaited<ReturnType<typeof createAudioOutput>>, { enabled: false }>) {
    stopTone();
    stopLoopback();
    stopHdaDemo();
    stopVirtioSndDemo();
    stopHdaPciDevice();

    const freqHz = 440;
    const gain = 0.1;
    const channelCount = output.ringBuffer.channelCount;
    const sr = output.context.sampleRate;

    // Generate the initial prefill in smaller chunks so the main thread can
    // start feeding the ring buffer before the AudioWorklet drains the small
    // `createAudioOutput()` startup padding. This keeps the Playwright smoke
    // test underrun-free even on slower CI runners.
    const writeChunkFrames = 512;
    const writeChunk = new Float32Array(writeChunkFrames * channelCount);

    function writeTone(frames: number) {
      let remaining = frames;
      while (remaining > 0) {
        const chunkFrames = Math.min(writeChunkFrames, remaining);
        const buf =
          chunkFrames === writeChunkFrames ? writeChunk : writeChunk.subarray(0, chunkFrames * channelCount);

        for (let i = 0; i < chunkFrames; i++) {
          const s = Math.sin(tonePhase * 2 * Math.PI) * gain;
          for (let c = 0; c < channelCount; c++) buf[i * channelCount + c] = s;
          tonePhase += freqHz / sr;
          if (tonePhase >= 1) tonePhase -= 1;
        }
        output.writeInterleaved(buf, sr);
        remaining -= chunkFrames;
      }
    }

    // Prefill ~100ms to avoid startup underruns.
    writeTone(Math.floor(sr / 10));

    const timer = window.setInterval(() => {
      const target = Math.floor(sr / 5); // ~200ms buffered
      const level = output.getBufferLevelFrames();
      const need = Math.max(0, target - level);
      if (need > 0) writeTone(need);

      const metrics = output.getMetrics();
      status.textContent =
        `AudioContext: ${metrics.state}\n` +
        `sampleRate: ${metrics.sampleRate}\n` +
        `baseLatencySeconds: ${metrics.baseLatencySeconds ?? "n/a"}\n` +
        `outputLatencySeconds: ${metrics.outputLatencySeconds ?? "n/a"}\n` +
        `bufferLevelFrames: ${metrics.bufferLevelFrames}\n` +
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
      stopHdaPciDevice();
      // Allow Playwright specs to override the ring-buffer capacity without adding extra UI knobs.
      // (CI uses this to validate resume-discard behaviour with large backlogs.)
      let ringBufferFrames: number | undefined;
      try {
        const params = new URLSearchParams(location.search);
        const raw = params.get("ringBufferFrames") ?? params.get("audioRingFrames");
        if (typeof raw === "string" && raw.trim()) {
          const parsed = Number.parseInt(raw, 10);
          if (Number.isFinite(parsed) && parsed > 0) ringBufferFrames = parsed;
        }
      } catch {
        // ignore
      }
      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ...(ringBufferFrames ? { ringBufferFrames } : {}),
      });
      // Expose for Playwright smoke tests.
      (globalThis as typeof globalThis & { __aeroAudioOutput?: unknown }).__aeroAudioOutput = output;

      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      try {
        startTone(output);
        await output.resume();
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
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
      stopHdaPciDevice();

      const workerConfig: AeroConfig = {
        // Audio-only worker demos do not require large guest RAM or a VRAM aperture; keep allocations
        // small so CI/Playwright runs don't reserve hundreds of MiB of shared memory.
        guestMemoryMiB: 1,
        vramMiB: 0,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
        ...harnessInputBackendOverrides,
      };

      try {
        workerCoordinator.start(workerConfig);
        // io.worker waits for the first `setBootDisks` message before reporting READY.
        // This harness does not mount any disks, but sending an explicit empty selection
        // keeps the worker lifecycle consistent and avoids the CPU demo busy-waiting on
        // `StatusIndex.IoReady`.
        workerCoordinator.setBootDisks({}, null, null);
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: Math.floor(48_000 / 5),
      });

      // Expose for Playwright smoke tests.
      (globalThis as typeof globalThis & { __aeroAudioOutputWorker?: unknown }).__aeroAudioOutputWorker = output;
      (globalThis as typeof globalThis & { __aeroAudioToneBackendWorker?: unknown }).__aeroAudioToneBackendWorker =
        "cpu-worker-wasm";

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
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

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
      stopHdaPciDevice();

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: 131_072, // ~2.7s @ 48k; gives the worker/WASM init ample slack in CI/headless.
      });

      // Expose for Playwright smoke tests / e2e assertions.
      (globalThis as typeof globalThis & { __aeroAudioOutputHdaDemo?: unknown }).__aeroAudioOutputHdaDemo = output;
      // Back-compat: other tests/debug helpers look for `__aeroAudioOutput`.
      (globalThis as typeof globalThis & { __aeroAudioOutput?: unknown }).__aeroAudioOutput = output;
      (globalThis as typeof globalThis & { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "wasm-hda";
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

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
      hdaDemoWorker = new Worker(new URL("../web/src/workers/cpu.worker.ts", import.meta.url), { type: "module" });
      hdaDemoWorker.addEventListener("message", (ev: MessageEvent<unknown>) => {
        const msg = ev.data as { type?: unknown } | null;
        if (!msg || msg.type !== "audioOutputHdaDemo.stats") return;
        hdaDemoStats = msg as { [k: string]: unknown };
        (globalThis as typeof globalThis & { __aeroAudioHdaDemoStats?: unknown }).__aeroAudioHdaDemoStats = hdaDemoStats;
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
        status.textContent = formatUiErrorMessage(err);
        stopHdaDemo();
        return;
      }

      try {
        await output.resume();
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        stopHdaDemo();
        return;
      }
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
      stopHdaPciDevice();

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: 131_072, // ~2.7s @ 48k; matches the HDA demo for CI slack.
      });

      // Expose for Playwright smoke tests / e2e assertions.
      (globalThis as typeof globalThis & { __aeroAudioOutputVirtioSndDemo?: unknown }).__aeroAudioOutputVirtioSndDemo = output;
      (globalThis as typeof globalThis & { __aeroAudioOutput?: unknown }).__aeroAudioOutput = output;
      (globalThis as typeof globalThis & { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "wasm-virtio-snd";
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      // Prefill the ring with silence so the worker has time to attach and start producing audio
      // without incurring startup underruns.
      const level = output.getBufferLevelFrames();
      const prefillFrames = Math.max(0, output.ringBuffer.capacityFrames - level);
      if (prefillFrames > 0) {
        Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
      }

      // Start the CPU worker in a standalone "audio demo" mode.
      virtioSndDemoWorker = new Worker(new URL("../web/src/workers/cpu.worker.ts", import.meta.url), { type: "module" });
      virtioSndDemoWorker.addEventListener("message", (ev: MessageEvent<unknown>) => {
        const msg = ev.data as { type?: unknown } | null;
        if (!msg || msg.type !== "audioOutputVirtioSndDemo.stats") return;
        virtioSndDemoStats = msg as { [k: string]: unknown };
        (globalThis as typeof globalThis & { __aeroAudioVirtioSndDemoStats?: unknown }).__aeroAudioVirtioSndDemoStats =
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
        status.textContent = formatUiErrorMessage(err);
        stopVirtioSndDemo();
        return;
      }

      try {
        await output.resume();
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        stopVirtioSndDemo();
        return;
      }

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

  const hdaPciDeviceButton = el("button", {
    id: "init-audio-hda-pci-device",
    text: "Init audio output (HDA PCI device)",
    onclick: async () => {
      status.textContent = "";
      stopTone();
      stopLoopback();
      stopHdaDemo();
      stopVirtioSndDemo();
      stopHdaPciDevice();

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        // Match the HDA demo ring size so the AudioWorklet has ample slack while the
        // worker runtime + WASM bridge spin up in CI/headless environments.
        ringBufferFrames: 131_072, // ~2.7s @ 48k
      });

      (globalThis as typeof globalThis & { __aeroAudioOutputHdaPciDevice?: unknown }).__aeroAudioOutputHdaPciDevice = output;
      (globalThis as typeof globalThis & { __aeroAudioOutput?: unknown }).__aeroAudioOutput = output;
      (globalThis as typeof globalThis & { __aeroAudioToneBackend?: unknown }).__aeroAudioToneBackend = "io-worker-hda-pci";

      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      // Prefill a large chunk of silence so AudioWorklet startup doesn't underrun
      // while we bring up the workers and program the HDA controller.
      //
      // Keep some headroom so the IO-worker HDA device can start writing without
      // immediately hitting a full buffer (which would count as producer overruns).
      const sr = output.context.sampleRate;
      const headroomFrames = Math.floor(sr / 2); // ~500ms headroom
      const maxPrefill = Math.max(0, output.ringBuffer.capacityFrames - headroomFrames);
      const targetPrefillFrames = Math.min(maxPrefill, Math.floor(sr * 2)); // ~2s silence
      const existingLevel = output.getBufferLevelFrames();
      const prefillFrames = Math.max(0, targetPrefillFrames - existingLevel);
      if (prefillFrames > 0) {
        Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
      }

      const workerConfig: AeroConfig = {
        // The CPU-worker HDA PCI playback harness allocates its CORB/RIRB/BDL/PCM scratch buffers
        // from the end of guest RAM (see `web/src/workers/cpu.worker.ts`), so 1MiB is sufficient
        // and reduces shared WebAssembly.Memory pressure (especially in CI / parallel tabs).
        guestMemoryMiB: 1,
        vramMiB: 0,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
        ...harnessInputBackendOverrides,
      };

      try {
        workerCoordinator.start(workerConfig);
        workerCoordinator.setBootDisks({}, null, null);
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

      // Attach the AudioWorklet ring buffer to the IO worker. Use the coordinator's
      // explicit ring-owner policy so we don't accidentally detach the ring when
      // workers report READY (the coordinator re-sends attachments on READY).
      workerCoordinator.setAudioRingBufferOwner("io");
      workerCoordinator.setAudioRingBuffer(
        output.ringBuffer.buffer,
        output.ringBuffer.capacityFrames,
        output.ringBuffer.channelCount,
        output.context.sampleRate,
      );

      // Ask the CPU worker to program the HDA PCI device over the real port/mmio path.
      const cpu = workerCoordinator.getWorker("cpu");
      if (!cpu) {
        status.textContent = "Missing CPU worker (workerCoordinator.getWorker(\"cpu\") returned null).";
        return;
      }

      const ready = new Promise<{ pci: { bus: number; device: number; function: number }; bar0: number }>((resolve, reject) => {
        // HDA device registration depends on WASM init in the IO worker (can be slow in CI without a wasm cache).
        const timeoutMs = 45_000;
        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data as { type?: unknown; message?: unknown; pci?: unknown; bar0?: unknown } | null;
          if (!data || typeof data !== "object") return;
          if (data.type === "audioOutputHdaPciDevice.ready") {
            cleanup();
            const pci = (data as { pci?: unknown }).pci as { bus: number; device: number; function: number };
            const bar0 = (data as { bar0?: unknown }).bar0 as number;
            resolve({ pci, bar0 });
          } else if (data.type === "audioOutputHdaPciDevice.error") {
            cleanup();
            reject(new Error(typeof data.message === "string" ? data.message : "HDA PCI device init failed"));
          }
        };
        const onError = (ev: ErrorEvent) => {
          cleanup();
          reject(new Error(ev.message || "CPU worker error while starting HDA PCI device"));
        };
        const timer = window.setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for HDA PCI device init (${timeoutMs}ms).`));
        }, timeoutMs);
        (timer as unknown as { unref?: () => void }).unref?.();
        const cleanup = () => {
          window.clearTimeout(timer);
          cpu.removeEventListener("message", onMessage as EventListener);
          cpu.removeEventListener("error", onError as EventListener);
        };
        cpu.addEventListener("message", onMessage as EventListener);
        cpu.addEventListener("error", onError as EventListener);
      });

      cpu.postMessage({ type: "audioOutputHdaPciDevice.start", freqHz: 440, gain: 0.1 });

      let initInfo: { pci: { bus: number; device: number; function: number }; bar0: number };
      try {
        initInfo = await ready;
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

      try {
        await output.resume();
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

      status.textContent = `Audio initialized and HDA PCI device started (bus=${initInfo.pci.bus} dev=${initInfo.pci.device}).`;
      const timer = window.setInterval(() => {
        const metrics = output.getMetrics();
        const read = Atomics.load(output.ringBuffer.readIndex, 0) >>> 0;
        const write = Atomics.load(output.ringBuffer.writeIndex, 0) >>> 0;
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
          `ring.writeFrameIndex: ${write}\n` +
          `pci: ${initInfo.pci.bus}:${initInfo.pci.device}.${initInfo.pci.function}\n` +
          `bar0: 0x${initInfo.bar0.toString(16)}`;
      }, 50);
      (timer as unknown as { unref?: () => void }).unref?.();
      hdaPciDeviceTimer = timer as unknown as number;
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
      stopHdaPciDevice();

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: 16_384, // ~340ms @ 48k; gives the worker/WASM init some slack.
      });

      const globals =
        globalThis as typeof globalThis & {
          __aeroAudioOutputLoopback?: unknown;
          __aeroAudioLoopbackBackend?: unknown;
          __aeroSyntheticMic?: unknown;
        };

      // Expose for Playwright.
      globals.__aeroAudioOutputLoopback = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      const sr = output.context.sampleRate;

      try {
        syntheticMic = startSyntheticMic({
          sampleRate: sr,
          bufferMs: 250,
          freqHz: 440,
          // Use a louder tone than the CPU worker's fallback sine (0.1) so automation can
          // distinguish true mic loopback from the fallback path.
          gain: 0.2,
        });
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

      globals.__aeroSyntheticMic = syntheticMic;

      // Prefill ~200ms of silence so the AudioWorklet doesn't count underruns while the
      // workers spin up and attach the loopback plumbing.
      const targetPrefillFrames = Math.min(output.ringBuffer.capacityFrames, Math.floor(sr / 5));
      const existingLevel = output.getBufferLevelFrames();
      const prefillFrames = Math.max(0, targetPrefillFrames - existingLevel);
      if (prefillFrames > 0) {
        // `SharedArrayBuffer` is guaranteed to be zero-initialized, and this demo always uses a
        // freshly allocated ring buffer. Avoid allocating/copying a large Float32Array of zeros
        // by simply advancing the write index to "claim" silent frames.
        Atomics.add(output.ringBuffer.writeIndex, 0, prefillFrames);
      }

      const workerConfig: AeroConfig = {
        // Audio loopback only needs the worker runtime + ring buffers; avoid allocating VRAM.
        guestMemoryMiB: 1,
        vramMiB: 0,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
        ...harnessInputBackendOverrides,
      };

      let backend: "worker" | "main" = "worker";
      let workerError: string | null = null;
      try {
        workerCoordinator.start(workerConfig);
        workerCoordinator.setBootDisks({}, null, null);
        // This demo performs a CPU-worker mic loopback (consume synthetic mic samples and write
        // them into the AudioWorklet output ring). Ensure the microphone ring is routed to the
        // CPU worker so it becomes the sole SPSC consumer (and so we don't accidentally keep the
        // mic attached to the IO worker from other harnesses like HDA capture).
        workerCoordinator.setMicrophoneRingBufferOwner("cpu");
        workerCoordinator.setMicrophoneRingBuffer(syntheticMic.ringBuffer, syntheticMic.sampleRate);
        workerCoordinator.setAudioOutputRingBuffer(
          output.ringBuffer.buffer,
          sr,
          output.ringBuffer.channelCount,
          output.ringBuffer.capacityFrames,
        );
      } catch (err) {
        backend = "main";
        workerError = formatUiErrorMessage(err);

        // Ensure we don't have both the worker and main thread consuming the mic/output rings.
        workerCoordinator.setMicrophoneRingBuffer(null, 0);
        workerCoordinator.setAudioOutputRingBuffer(null, 0, 0, 0);

        const header = new Uint32Array(syntheticMic.ringBuffer, 0, MIC_HEADER_U32_LEN);
        const capacity = Atomics.load(header, MIC_CAPACITY_SAMPLES_INDEX) >>> 0;
        const data = new Float32Array(syntheticMic.ringBuffer, MIC_HEADER_BYTES, capacity);
        const micRb: MicRingBuffer = { sab: syntheticMic.ringBuffer, header, data, capacity };

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

            const written = output.writeInterleaved(tmpInterleaved.subarray(0, outSamples), sr);
            if (written === 0) break;
            need -= written;
          }
        }, 25);
        (timer as unknown as { unref?: () => void }).unref?.();
        loopbackTimer = timer as unknown as number;
      }

      globals.__aeroAudioLoopbackBackend = backend;

      try {
        await output.resume();
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        return;
      }

      status.textContent = workerError
        ? `Audio loopback initialized (backend=${backend}). Worker init failed: ${workerError}`
        : `Audio loopback initialized (backend=${backend}).`;
    },
  });

  const hdaCaptureButton = el("button", {
    id: "init-audio-hda-capture-synthetic",
    text: "Init HDA capture (synthetic mic)",
    onclick: async () => {
      status.textContent = "";
      stopTone();
      stopLoopback();
      stopHdaDemo();
      stopVirtioSndDemo();
      stopHdaPciDevice();

      const globals =
        globalThis as typeof globalThis & {
          __aeroSyntheticMic?: unknown;
          __aeroAudioHdaCaptureSyntheticResult?: unknown;
        };

      const result: {
        done: boolean;
        ok: boolean;
        error?: string;
        pcmNonZero?: boolean;
        pcmNonZeroBytes?: number;
        pcmFirst16?: number[];
        micReadDelta?: number;
        micWriteDelta?: number;
        micDroppedDelta?: number;
      } = { done: false, ok: false };
      globals.__aeroAudioHdaCaptureSyntheticResult = result;

      try {
        syntheticMic = startSyntheticMic({
          sampleRate: 48_000,
          bufferMs: 250,
          freqHz: 440,
          gain: 0.1,
        });
        globals.__aeroSyntheticMic = syntheticMic;

        const header = new Uint32Array(syntheticMic.ringBuffer, 0, MIC_HEADER_U32_LEN);
        const startMicReadPos = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
        const startMicWritePos = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;
        const startMicDropped = Atomics.load(header, MIC_DROPPED_SAMPLES_INDEX) >>> 0;

        const workerConfig: AeroConfig = {
          // Synthetic HDA capture allocates scratch buffers at the end of guest RAM; keep the
          // allocation small and disable VRAM to reduce shared-memory pressure in tests.
          guestMemoryMiB: 1,
          vramMiB: 0,
          enableWorkers: true,
          enableWebGPU: false,
          proxyUrl: null,
          activeDiskImage: null,
          logLevel: "info",
          ...harnessInputBackendOverrides,
        };

        workerCoordinator.start(workerConfig);
        // The synthetic HDA capture harness uses the IO worker's HDA controller to consume mic
        // samples and DMA-write PCM into guest RAM. The default microphone ring-buffer policy
        // routes the mic to the CPU worker when no disk is attached (demo mode), so override it
        // here to ensure the IO worker receives the capture ring.
        workerCoordinator.setBootDisks({}, null, null);
        // Demo mode defaults the mic ring consumer to the CPU worker, but HDA capture lives in the IO worker.
        // Route the microphone ring explicitly so the IO worker is the sole SPSC consumer.
        workerCoordinator.setMicrophoneRingBufferOwner("io");
        workerCoordinator.setMicrophoneRingBuffer(syntheticMic.ringBuffer, syntheticMic.sampleRate);

        const statusView = workerCoordinator.getStatusView();
        if (!statusView) throw new Error("Missing worker status view.");

        const readyDeadlineMs = performance.now() + 20_000;
        while (performance.now() < readyDeadlineMs) {
          const cpuReady = Atomics.load(statusView, StatusIndex.CpuReady);
          const ioReady = Atomics.load(statusView, StatusIndex.IoReady);
          if (cpuReady === 1 && ioReady === 1) break;
          await sleepMs(50);
        }
        if (Atomics.load(statusView, StatusIndex.CpuReady) !== 1 || Atomics.load(statusView, StatusIndex.IoReady) !== 1) {
          throw new Error("Timed out waiting for CPU/I/O workers to become ready.");
        }

        // Wait for IO worker WASM init so the HDA PCI function is registered before we attempt to
        // program capture registers from the CPU side.
        const wasmDeadlineMs = performance.now() + 30_000;
        while (performance.now() < wasmDeadlineMs) {
          const ioWasm = workerCoordinator.getWorkerWasmStatus("io");
          if (ioWasm?.variant) break;
          await sleepMs(50);
        }
        const ioWasm = workerCoordinator.getWorkerWasmStatus("io");
        if (!ioWasm) {
          throw new Error("Timed out waiting for IO worker WASM initialization.");
        }
        if (ioWasm.variant !== "threaded") {
          throw new Error(`Unexpected IO worker WASM variant '${ioWasm.variant}'; expected 'threaded'.`);
        }

        const cpuWorker = workerCoordinator.getWorker("cpu");
        if (!cpuWorker) throw new Error("CPU worker is not available.");

        const requestId = hdaCaptureRequestId++;

        type ReadyMsg = {
          type: "audioHdaCaptureSynthetic.ready";
          requestId: number;
          pcmBase: number;
          pcmBytes: number;
        };
        type ErrorMsg = { type: "audioHdaCaptureSynthetic.error"; requestId: number; message: string };

        const readyMsg = await new Promise<ReadyMsg>((resolve, reject) => {
          // HDA device discovery depends on I/O worker WASM/device bring-up which can be slower
          // in CI/headless Chromium. Allow more time before treating it as a hard failure.
          const timeoutMs = 35_000;
          const timer = window.setTimeout(() => {
            cleanup();
            reject(new Error(`Timed out waiting for HDA capture setup (${timeoutMs}ms).`));
          }, timeoutMs);
          (timer as unknown as { unref?: () => void }).unref?.();

          const onMessage = (ev: MessageEvent) => {
            const data = ev.data as Partial<ReadyMsg | ErrorMsg> | null;
            if (!data || typeof data !== "object") return;
            if (data.requestId !== requestId) return;
            if (data.type === "audioHdaCaptureSynthetic.ready") {
              cleanup();
              resolve(data as ReadyMsg);
            } else if (data.type === "audioHdaCaptureSynthetic.error") {
              cleanup();
              reject(new Error(typeof data.message === "string" ? data.message : "HDA capture setup failed."));
            }
          };

          const cleanup = () => {
            window.clearTimeout(timer);
            cpuWorker.removeEventListener("message", onMessage);
          };

          cpuWorker.addEventListener("message", onMessage);
          cpuWorker.postMessage({ type: "audioHdaCaptureSynthetic.start", requestId });
        });

        const memory = workerCoordinator.getGuestMemory();
        if (!memory) throw new Error("Missing guest memory.");

        const guestBase = Atomics.load(statusView, StatusIndex.GuestBase) >>> 0;
        const guestSize = Atomics.load(statusView, StatusIndex.GuestSize) >>> 0;
        if (guestBase === 0 || guestSize === 0) throw new Error("Guest RAM layout is not initialized.");

        const guestU8 = new Uint8Array(memory.buffer, guestBase, guestSize);
        const pcmBase = readyMsg.pcmBase >>> 0;
        const pcmBytes = readyMsg.pcmBytes >>> 0;
        if (pcmBase + pcmBytes > guestU8.byteLength) {
          throw new Error(`PCM buffer out of bounds (pcmBase=0x${pcmBase.toString(16)} pcmBytes=0x${pcmBytes.toString(16)}).`);
        }

        const pcm = guestU8.subarray(pcmBase, pcmBase + pcmBytes);

        status.textContent = "Waiting for HDA capture DMA…";

        const captureDeadlineMs = performance.now() + 5_000;
        let pcmNonZero = false;
        let pcmNonZeroBytes = 0;
        while (performance.now() < captureDeadlineMs) {
          pcmNonZeroBytes = 0;
          for (let i = 0; i < pcm.length; i++) {
            if (pcm[i] !== 0) {
              pcmNonZero = true;
              pcmNonZeroBytes += 1;
              // Scan a small prefix for metrics; we only need to know it's not all zeros.
              if (pcmNonZeroBytes >= 8) break;
            }
          }
          if (pcmNonZero) break;
          await sleepMs(50);
        }

        const endMicReadPos = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
        const endMicWritePos = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;
        const endMicDropped = Atomics.load(header, MIC_DROPPED_SAMPLES_INDEX) >>> 0;

        const micReadDelta = ((endMicReadPos - startMicReadPos) >>> 0) >>> 0;
        const micWriteDelta = ((endMicWritePos - startMicWritePos) >>> 0) >>> 0;
        const micDroppedDelta = ((endMicDropped - startMicDropped) >>> 0) >>> 0;

        result.pcmNonZero = pcmNonZero;
        result.pcmNonZeroBytes = pcmNonZeroBytes;
        result.pcmFirst16 = Array.from(pcm.slice(0, 16));
        result.micReadDelta = micReadDelta;
        result.micWriteDelta = micWriteDelta;
        result.micDroppedDelta = micDroppedDelta;

        if (!pcmNonZero) {
          throw new Error("Capture DMA did not write any non-zero PCM bytes into guest RAM.");
        }
        if (micReadDelta === 0) {
          throw new Error("Mic ring buffer read_pos did not advance (consumer inactive).");
        }

        result.ok = true;
        status.textContent =
          `HDA capture OK.\n` +
          `micReadDelta=${micReadDelta} micWriteDelta=${micWriteDelta} micDroppedDelta=${micDroppedDelta}\n` +
          `pcmFirst16=${result.pcmFirst16.join(",")}`;
      } catch (err) {
        const message = formatUiErrorMessage(err);
        result.error = message;
        result.ok = false;
        status.textContent = message;
      } finally {
        // Best-effort cleanup: stop the synthetic mic timer and detach the ring buffer so
        // other demos/tests don't inherit a background capture producer/consumer.
        try {
          // Stop the capture stream/CORB/RIRB engines (best-effort). Without this, the capture
          // DMA engine can remain active in the long-lived worker runtime and interfere with
          // subsequent demos.
          workerCoordinator.getWorker("cpu")?.postMessage({ type: "audioOutputHdaPciDevice.stop" });
        } catch {
          // ignore
        }
        try {
          syntheticMic?.stop();
        } catch {
          // ignore
        }
        syntheticMic = null;
        try {
          workerCoordinator.setMicrophoneRingBuffer(null, 0);
          workerCoordinator.setMicrophoneRingBufferOwner(null);
        } catch {
          // ignore
        }
        result.done = true;
      }
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Audio" }),
    el(
      "div",
      { class: "row" },
      button,
      workerButton,
      hdaDemoButton,
      virtioSndDemoButton,
      hdaPciDeviceButton,
      loopbackButton,
      hdaCaptureButton,
    ),
    status,
  );
}

function renderInputBackendHudPanel(): HTMLElement {
  const output = el("pre", { class: "mono", text: "Waiting for VM status…" });

  function resolveCoordinator(): WorkerCoordinator | null {
    const g = globalThis as unknown as { __aeroWorkerCoordinator?: unknown };
    const candidate = g.__aeroWorkerCoordinator;
    if (!candidate || typeof candidate !== "object") return null;
    const maybe = candidate as { getStatusView?: unknown };
    return typeof maybe.getStatusView === "function" ? (candidate as WorkerCoordinator) : null;
  }

  function bitsSet(v: number): number {
    let n = v >>> 0;
    let count = 0;
    while (n) {
      n &= n - 1;
      count += 1;
    }
    return count;
  }

  const update = () => {
    const coordinator = resolveCoordinator();
    const statusView = coordinator?.getStatusView() ?? null;
    if (!statusView) {
      output.textContent = "Status unavailable (workers not started or SharedArrayBuffer unsupported).";
      return;
    }

    try {
      const kbBackendCode = Atomics.load(statusView, StatusIndex.IoInputKeyboardBackend);
      const mouseBackendCode = Atomics.load(statusView, StatusIndex.IoInputMouseBackend);
      const kbBackend = decodeInputBackendStatus(kbBackendCode) ?? `unknown(${kbBackendCode})`;
      const mouseBackend = decodeInputBackendStatus(mouseBackendCode) ?? `unknown(${mouseBackendCode})`;

      const virtioKbOk = Atomics.load(statusView, StatusIndex.IoInputVirtioKeyboardDriverOk) !== 0;
      const virtioMouseOk = Atomics.load(statusView, StatusIndex.IoInputVirtioMouseDriverOk) !== 0;
      const usbKbOk = Atomics.load(statusView, StatusIndex.IoInputUsbKeyboardOk) !== 0;
      const usbMouseOk = Atomics.load(statusView, StatusIndex.IoInputUsbMouseOk) !== 0;

      const keysHeld = Atomics.load(statusView, StatusIndex.IoInputKeyboardHeldCount) >>> 0;
      const buttonsMask = Atomics.load(statusView, StatusIndex.IoInputMouseButtonsHeldMask) >>> 0;
      const buttonsHeld = bitsSet(buttonsMask & 0x1f);

      output.textContent =
        `keyboard backend: ${kbBackend}\n` +
        `mouse backend: ${mouseBackend}\n` +
        `virtio driver_ok: keyboard=${virtioKbOk} mouse=${virtioMouseOk}\n` +
        `usb configured: keyboard=${usbKbOk} mouse=${usbMouseOk}\n` +
        `held: keys=${keysHeld} mouseButtons=${buttonsHeld} mask=0x${(buttonsMask & 0x1f).toString(16)}`;
    } catch (err) {
      output.textContent = formatUiErrorMessage(err);
    }
  };

  const timer = window.setInterval(update, 200);
  (timer as unknown as { unref?: () => void }).unref?.();
  update();

  return el("div", { class: "panel" }, el("h2", { text: "Input backend HUD" }), output);
}

function renderHotspotsPanel(report: PlatformFeatureReport): HTMLElement {
  if (!report.wasmThreads) {
    return el(
      'div',
      { class: 'panel' },
      el('h2', { text: 'Hotspots' }),
      el('pre', { text: 'Hotspots unavailable: requires cross-origin isolation + SharedArrayBuffer + Atomics.' }),
    );
  }

  const isHotspotEntry = (value: unknown): value is HotspotEntry => {
    if (!value || typeof value !== 'object') return false;
    const maybe = value as Record<string, unknown>;
    return (
      typeof maybe.pc === 'string' &&
      typeof maybe.hits === 'number' &&
      typeof maybe.instructions === 'number' &&
      typeof maybe.percent_of_total === 'number'
    );
  };

  const perfFacade = {
    export: (): HotspotPerfExport => {
      const exported = globalThis.aero?.perf?.export?.();
      if (exported && typeof exported === 'object' && !Array.isArray(exported)) {
        const hotspots = (exported as { hotspots?: unknown }).hotspots;
        if (Array.isArray(hotspots)) {
          return { hotspots: hotspots.filter(isHotspotEntry) };
        }
      }
      return { hotspots: [] };
    },
  };

  const panel = createHotspotsPanel({ perf: perfFacade, topN: 10, refreshMs: 500 });
  panel.classList.add('panel');
  return panel;
}

function renderRemoteDiskPanel(): HTMLElement {
  const SETTINGS_KEY = 'aero.remoteDiskPanel.settings.v1';

  function stableUrlForStorage(url: string): string {
    // Avoid persisting signed URLs / auth query params into localStorage.
    try {
      const u = new URL(url, location.href);
      u.search = '';
      u.hash = '';
      return u.toString();
    } catch {
      const noHash = url.split('#', 1)[0] ?? url;
      return (noHash.split('?', 1)[0] ?? noHash).trim();
    }
  }

  const warning = el(
    'div',
    { class: 'mono' },
    'Remote disk images are experimental. Only use images you own/have rights to. ',
    'The server must support either HTTP Range requests (single-file images) or the chunked manifest format (see docs/disk-images.md).',
  );

  const enabledInput = el('input', { type: 'checkbox' }) as HTMLInputElement;
  const modeSelect = el(
    'select',
    {},
    el('option', { value: 'range', text: 'HTTP Range' }),
    el('option', { value: 'chunked', text: 'Chunked manifest.json' }),
  ) as HTMLSelectElement;
  const cacheBackendSelect = el(
    'select',
    {},
    el('option', { value: 'auto', text: 'cache: auto' }),
    el('option', { value: 'opfs', text: 'cache: OPFS' }),
    el('option', { value: 'idb', text: 'cache: IndexedDB' }),
  ) as HTMLSelectElement;
  const credentialsSelect = el(
    'select',
    {},
    el('option', { value: 'same-origin', text: 'credentials: same-origin' }),
    el('option', { value: 'include', text: 'credentials: include' }),
    el('option', { value: 'omit', text: 'credentials: omit' }),
  ) as HTMLSelectElement;
  const cacheImageIdInput = el('input', {
    type: 'text',
    placeholder: 'cache image id (optional)',
  }) as HTMLInputElement;
  const cacheVersionInput = el('input', {
    type: 'text',
    placeholder: 'cache version (optional)',
  }) as HTMLInputElement;
  const urlInput = el('input', { type: 'url', placeholder: 'https://example.com/disk.raw' }) as HTMLInputElement;
  const blockSizeInput = el('input', { type: 'number', value: String(1024), min: '4' }) as HTMLInputElement;
  const cacheLimitInput = el('input', { type: 'number', value: String(512), min: '0' }) as HTMLInputElement;
  const cacheUnboundedInput = el('input', { type: 'checkbox' }) as HTMLInputElement;
  const prefetchInput = el('input', { type: 'number', value: String(2), min: '0' }) as HTMLInputElement;
  const maxConcurrentFetchesInput = el('input', { type: 'number', value: String(4), min: '1' }) as HTMLInputElement;
  const stats = el('pre', { text: '' });
  const output = el('pre', { text: '' });

  const probeButton = el('button', { text: 'Probe Range support' }) as HTMLButtonElement;
  const readButton = el('button', { text: 'Read sample bytes' }) as HTMLButtonElement;
  const flushButton = el('button', { text: 'Flush cache' }) as HTMLButtonElement;
  const clearButton = el('button', { text: 'Clear cache' }) as HTMLButtonElement;
  const resetStatsButton = el('button', { text: 'Reset stats' }) as HTMLButtonElement;
  const closeButton = el('button', { text: 'Close' }) as HTMLButtonElement;
  const progress = el('progress', { value: '0', max: '1', style: 'width: 320px' }) as HTMLProgressElement;

  const client = new RuntimeDiskClient();
  let handle: number | null = null;
  let statsPollPending = false;
  let statsBaseline: Awaited<ReturnType<RuntimeDiskClient['stats']>> | null = null;
  let statsBaselineAtMs: number | null = null;

  const saveSettings = () => {
    const payload = {
      enabled: enabledInput.checked,
      mode: modeSelect.value,
      cacheBackend: cacheBackendSelect.value,
      credentials: credentialsSelect.value,
      url: stableUrlForStorage(urlInput.value),
      blockKiB: blockSizeInput.value,
      cacheLimitMiB: cacheLimitInput.value,
      cacheUnbounded: cacheUnboundedInput.checked,
      prefetch: prefetchInput.value,
      maxConcurrentFetches: maxConcurrentFetchesInput.value,
      cacheImageId: cacheImageIdInput.value,
      cacheVersion: cacheVersionInput.value,
    };
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(payload));
  };

  const restoreSettings = () => {
    try {
      const raw = localStorage.getItem(SETTINGS_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw) as Partial<Record<string, unknown>>;
      if (typeof parsed.enabled === 'boolean') enabledInput.checked = parsed.enabled;
      if (typeof parsed.mode === 'string' && ['range', 'chunked'].includes(parsed.mode)) modeSelect.value = parsed.mode;
      if (typeof parsed.cacheBackend === 'string' && ['auto', 'opfs', 'idb'].includes(parsed.cacheBackend)) {
        cacheBackendSelect.value = parsed.cacheBackend;
      }
      if (typeof parsed.credentials === 'string' && ['same-origin', 'include', 'omit'].includes(parsed.credentials)) {
        credentialsSelect.value = parsed.credentials;
      }
      if (typeof parsed.url === 'string') urlInput.value = parsed.url;
      if (typeof parsed.blockKiB === 'string') blockSizeInput.value = parsed.blockKiB;
      if (typeof parsed.cacheLimitMiB === 'string') cacheLimitInput.value = parsed.cacheLimitMiB;
      if (typeof parsed.cacheUnbounded === 'boolean') {
        cacheUnboundedInput.checked = parsed.cacheUnbounded;
      } else if (typeof parsed.cacheLimitMiB === 'string') {
        // Backward compatibility: old panel versions treated cacheLimitMiB <= 0 as "unbounded".
        // Preserve that intent by mapping it onto the explicit unbounded checkbox.
        const n = Number(parsed.cacheLimitMiB);
        if (Number.isFinite(n) && n <= 0) {
          cacheUnboundedInput.checked = true;
          // Ensure disabling unbounded doesn't silently turn caching off.
          cacheLimitInput.value = String(512);
        }
      }
      if (typeof parsed.prefetch === 'string') prefetchInput.value = parsed.prefetch;
      if (typeof parsed.maxConcurrentFetches === 'string') maxConcurrentFetchesInput.value = parsed.maxConcurrentFetches;
      if (typeof parsed.cacheImageId === 'string') cacheImageIdInput.value = parsed.cacheImageId;
      if (typeof parsed.cacheVersion === 'string') cacheVersionInput.value = parsed.cacheVersion;
    } catch {
      // ignore
    }
  };

  function formatMaybeBytes(bytes: number | null): string {
    if (bytes === 0) return 'off';
    if (bytes === null) return 'unlimited';
    return formatByteSize(bytes);
  }

  function updateButtons(): void {
    const enabled = enabledInput.checked;
    probeButton.disabled = !enabled;
    readButton.disabled = !enabled;
    flushButton.disabled = !enabled || handle === null;
    clearButton.disabled = !enabled || handle === null;
    resetStatsButton.disabled = !enabled || handle === null;
    closeButton.disabled = !enabled || handle === null;
  }

  function updateModeUi(): void {
    const chunked = modeSelect.value === 'chunked';
    blockSizeInput.disabled = chunked;
    maxConcurrentFetchesInput.disabled = !chunked;
    urlInput.placeholder = chunked ? 'https://example.com/manifest.json' : 'https://example.com/disk.raw';
    probeButton.textContent = chunked ? 'Fetch manifest' : 'Probe Range support';
  }

  function updateCacheUi(): void {
    cacheLimitInput.disabled = cacheUnboundedInput.checked;
  }

  enabledInput.addEventListener('change', () => {
    if (!enabledInput.checked) {
      void closeHandle();
    }
    saveSettings();
    updateButtons();
  });
  modeSelect.addEventListener('change', () => {
    void closeHandle();
    updateModeUi();
    saveSettings();
    updateButtons();
  });
  cacheUnboundedInput.addEventListener('change', () => {
    void closeHandle();
    updateCacheUi();
    saveSettings();
    updateButtons();
  });
  for (const input of [
    urlInput,
    cacheBackendSelect,
    credentialsSelect,
    cacheImageIdInput,
    cacheVersionInput,
    blockSizeInput,
    cacheLimitInput,
    prefetchInput,
    maxConcurrentFetchesInput,
  ]) {
    input.addEventListener('change', () => {
      void closeHandle();
      saveSettings();
      updateButtons();
    });
  }
  restoreSettings();
  updateModeUi();
  updateCacheUi();
  updateButtons();

  async function closeHandle(): Promise<void> {
    if (handle === null) return;
    const cur = handle;
    handle = null;
    statsBaseline = null;
    statsBaselineAtMs = null;
    try {
      await client.closeDisk(cur);
    } catch (err) {
      // Best-effort; if the worker is gone, nothing else to do.
      output.textContent = formatUiErrorMessage(err);
    }
  }

  async function ensureOpen(): Promise<number> {
    if (handle !== null) return handle;

    const url = urlInput.value.trim();
    if (!url) throw new Error('Enter a URL first.');
    statsBaseline = null;
    statsBaselineAtMs = null;

    let cacheLimitBytes: number | null | undefined;
    if (cacheUnboundedInput.checked) {
      cacheLimitBytes = null;
    } else {
      const rawCacheMiB = cacheLimitInput.value.trim();
      if (rawCacheMiB) {
        const cacheLimitMiB = Number(rawCacheMiB);
        if (!Number.isFinite(cacheLimitMiB) || !Number.isInteger(cacheLimitMiB) || cacheLimitMiB < 0) {
          throw new Error('Invalid cache size.');
        }
        const bytes = cacheLimitMiB * 1024 * 1024;
        if (!Number.isSafeInteger(bytes) || bytes < 0) {
          throw new Error('Invalid cache size.');
        }
        cacheLimitBytes = bytes;
      } else {
        cacheLimitBytes = undefined;
      }
    }

    const prefetchSequential = Math.max(0, Number(prefetchInput.value) | 0);
    const cacheImageId = cacheImageIdInput.value.trim();
    const cacheVersion = cacheVersionInput.value.trim();
    const opened =
      modeSelect.value === 'chunked'
        ? await client.openChunked(url, {
            cacheLimitBytes,
            credentials: credentialsSelect.value as RequestCredentials,
            prefetchSequentialChunks: prefetchSequential,
            maxConcurrentFetches: Math.max(1, Number(maxConcurrentFetchesInput.value) | 0),
            cacheBackend: cacheBackendSelect.value === 'auto' ? undefined : (cacheBackendSelect.value as 'opfs' | 'idb'),
            ...(cacheImageId ? { cacheImageId } : {}),
            ...(cacheVersion ? { cacheVersion } : {}),
          })
        : await client.openRemote(url, {
            blockSize: Number(blockSizeInput.value) * 1024,
            cacheLimitBytes,
            credentials: credentialsSelect.value as RequestCredentials,
            prefetchSequentialBlocks: prefetchSequential,
            cacheBackend: cacheBackendSelect.value === 'auto' ? undefined : (cacheBackendSelect.value as 'opfs' | 'idb'),
            ...(cacheImageId ? { cacheImageId } : {}),
            ...(cacheVersion ? { cacheVersion } : {}),
          });
    handle = opened.handle;
    updateButtons();
    try {
      statsBaseline = await client.stats(opened.handle);
      statsBaselineAtMs = Date.now();
    } catch {
      statsBaseline = null;
      statsBaselineAtMs = null;
    }
    return opened.handle;
  }

  async function refreshStats(): Promise<void> {
    if (!enabledInput.checked || handle === null) {
      stats.textContent = '';
      return;
    }
    if (statsPollPending) return;
    statsPollPending = true;
    const cur = handle;
    try {
      const res = await client.stats(cur);
      if (handle !== cur) return;
      const remote = res.remote;
      if (!remote) {
        stats.textContent = `disk: ${formatByteSize(res.capacityBytes)}\nreads=${res.io.reads} writes=${res.io.writes}`;
        return;
      }

      const baselineRemote = statsBaseline?.remote;
      const baselineIo = statsBaseline?.io;
      const baseBlockRequests = baselineRemote?.blockRequests ?? 0;
      const baseCacheHits = baselineRemote?.cacheHits ?? 0;
      const baseCacheMisses = baselineRemote?.cacheMisses ?? 0;
      const baseInflightJoins = baselineRemote?.inflightJoins ?? 0;
      const baseRequests = baselineRemote?.requests ?? 0;
      const baseBytesDownloaded = baselineRemote?.bytesDownloaded ?? 0;
      const baseIoReads = baselineIo?.reads ?? 0;
      const baseIoBytesRead = baselineIo?.bytesRead ?? 0;

      const deltaCacheHits = remote.cacheHits - baseCacheHits;
      const deltaCacheMisses = remote.cacheMisses - baseCacheMisses;
      const hitRateDenom = deltaCacheHits + deltaCacheMisses;
      const hitRate = hitRateDenom > 0 ? deltaCacheHits / hitRateDenom : 0;
      const cacheCoverage = remote.totalSize > 0 ? remote.cachedBytes / remote.totalSize : 0;
      const deltaIoBytesRead = res.io.bytesRead - baseIoBytesRead;
      const deltaBytesDownloaded = remote.bytesDownloaded - baseBytesDownloaded;
      const downloadAmplification = deltaIoBytesRead > 0 ? deltaBytesDownloaded / deltaIoBytesRead : 0;
      const lastFetchRangeText = remote.lastFetchRange
        ? `${formatByteSize(remote.lastFetchRange.start)}-${formatByteSize(remote.lastFetchRange.end - 1)}`
        : '—';
      const lastFetchAtText = remote.lastFetchAtMs === null ? '—' : new Date(remote.lastFetchAtMs).toLocaleTimeString();
      const sinceText = statsBaselineAtMs === null ? '—' : new Date(statsBaselineAtMs).toLocaleTimeString();

      stats.textContent =
        `imageSize=${formatByteSize(remote.totalSize)}\n` +
        `cache=${formatByteSize(remote.cachedBytes)} (${(cacheCoverage * 100).toFixed(2)}%) limit=${formatMaybeBytes(remote.cacheLimitBytes)}\n` +
        `blockSize=${formatByteSize(remote.blockSize)}\n` +
        `since=${sinceText}\n` +
        `ioReads=${res.io.reads - baseIoReads} inflightReads=${res.io.inflightReads} lastReadMs=${res.io.lastReadMs === null ? '—' : res.io.lastReadMs.toFixed(1)}\n` +
        `ioBytesRead=${formatByteSize(deltaIoBytesRead)} downloadAmp=${downloadAmplification.toFixed(2)}x\n` +
        `requests=${remote.requests - baseRequests} bytesDownloaded=${formatByteSize(deltaBytesDownloaded)}\n` +
        `blockRequests=${remote.blockRequests - baseBlockRequests} hits=${deltaCacheHits} misses=${deltaCacheMisses} inflightJoins=${remote.inflightJoins - baseInflightJoins} hitRate=${(hitRate * 100).toFixed(1)}%\n` +
        `inflightFetches=${remote.inflightFetches} lastFetch=${lastFetchAtText} ${lastFetchRangeText} (${remote.lastFetchMs === null ? '—' : remote.lastFetchMs.toFixed(1)}ms)\n`;
    } catch (err) {
      stats.textContent = formatUiErrorMessage(err);
    } finally {
      statsPollPending = false;
    }
  }

  const statsTimer = window.setInterval(() => void refreshStats(), 250);
  (statsTimer as unknown as { unref?: () => void }).unref?.();

  probeButton.onclick = async () => {
    output.textContent = '';
    progress.value = 0;

    try {
      await closeHandle();

      output.textContent = 'Probing… (this will make HTTP requests)\n';
      const openedHandle = await ensureOpen();
      const res = await client.stats(openedHandle);
      output.textContent = JSON.stringify(res.remote, null, 2);
      updateButtons();
    } catch (err) {
      output.textContent = formatUiErrorMessage(err);
    }
  };

  readButton.onclick = async () => {
    output.textContent = '';
    progress.value = 0;

    try {
      const openedHandle = await ensureOpen();

      // Read one sector at LBA=2 (byte offset 1024). This is aligned for the block device API.
      const bytes = await client.read(openedHandle, 2, 512);

      const res = await client.stats(openedHandle);
      output.textContent = JSON.stringify(
        { read: { lba: 2, byteLength: 512, first16: Array.from(bytes.slice(0, 16)) }, stats: res.remote },
        null,
        2,
      );
      progress.value = 1;
    } catch (err) {
      output.textContent = formatUiErrorMessage(err);
    }
  };

  flushButton.onclick = async () => {
    output.textContent = '';
    progress.value = 0;
    try {
      if (handle === null) {
        output.textContent = 'Nothing to flush (probe/open first).';
        return;
      }
      await client.flush(handle);
      progress.value = 1;
      void refreshStats();
    } catch (err) {
      output.textContent = formatUiErrorMessage(err);
    }
  };

  clearButton.onclick = async () => {
    output.textContent = '';
    progress.value = 0;
    try {
      if (handle === null) {
        output.textContent = 'Nothing to clear (probe/open first).';
        return;
      }
      await client.clearCache(handle);
      try {
        statsBaseline = await client.stats(handle);
        statsBaselineAtMs = Date.now();
      } catch {
        statsBaseline = null;
        statsBaselineAtMs = null;
      }
      progress.value = 1;
      void refreshStats();
      output.textContent = 'Cache cleared.';
      updateButtons();
    } catch (err) {
      output.textContent = formatUiErrorMessage(err);
    }
  };

  resetStatsButton.onclick = async () => {
    output.textContent = '';
    progress.value = 0;
    try {
      if (handle === null) {
        output.textContent = 'Nothing to reset (probe/open first).';
        return;
      }
      statsBaseline = await client.stats(handle);
      statsBaselineAtMs = Date.now();
      progress.value = 1;
      output.textContent = 'Stats reset.';
      void refreshStats();
    } catch (err) {
      output.textContent = formatUiErrorMessage(err);
    }
  };

  closeButton.onclick = async () => {
    output.textContent = '';
    progress.value = 0;
    await closeHandle();
    updateButtons();
  };

  return el(
    'div',
    { class: 'panel' },
    el('h2', { text: 'Remote disk image (streaming)' }),
    warning,
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'Enable:' }),
      enabledInput,
      el('label', { text: 'Mode:' }),
      modeSelect,
      cacheBackendSelect,
      credentialsSelect,
      el('label', { text: 'URL:' }),
      urlInput,
    ),
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'Cache key override:' }),
      cacheImageIdInput,
      cacheVersionInput,
    ),
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'Block KiB (range):' }),
      blockSizeInput,
      el('label', { text: 'Cache MiB (0=disabled):' }),
      cacheLimitInput,
      el('label', { text: 'Unbounded:' }),
      cacheUnboundedInput,
      el('label', { text: 'Prefetch:' }),
      prefetchInput,
      el('label', { text: 'Max inflight (chunked):' }),
      maxConcurrentFetchesInput,
      probeButton,
      readButton,
      flushButton,
      clearButton,
      resetStatsButton,
      closeButton,
      progress,
    ),
    stats,
    output,
  );
}

function renderJitSmokePanel(report: PlatformFeatureReport): HTMLElement {
  const output = el('pre', { text: '' });
  const button = el('button', { text: 'Run JIT smoke test' }) as HTMLButtonElement;

  const enabled = report.wasmThreads && report.jit_dynamic_wasm;

  const hint = el('div', {
    class: 'mono',
    text: enabled
      ? 'Spawns CPU+JIT workers; CPU requests compilation, JIT emits a WASM block, CPU installs it into a table and executes it.'
      : !report.wasmThreads
        ? 'Requires cross-origin isolation + SharedArrayBuffer + Atomics (wasmThreads=true).'
        : "Dynamic WASM compilation is blocked (jit_dynamic_wasm=false). Check CSP for `script-src 'wasm-unsafe-eval'`.",
  });

  const run = () => {
    void runJitSmokeTest(output).catch((err) => {
      output.textContent = formatUiErrorMessage(err);
    });
  };
  button.onclick = run;

  if (!enabled) {
    button.disabled = true;
    output.textContent = `Skipped (${!report.wasmThreads ? 'wasmThreads=false' : 'jit_dynamic_wasm=false'}).`;
  } else {
    // Avoid running the JIT smoke test automatically: it spawns CPU+JIT workers and allocates a
    // large shared WebAssembly.Memory (128MiB runtime-reserved + guest RAM). This can significantly
    // increase baseline memory usage in Playwright where many pages load in parallel.
    //
    // Opt into auto-run via `?jitSmoke=1` (used by the dedicated jit-pipeline E2E test).
    if (harnessSearchParams.has('jitSmoke')) {
      run();
    } else {
      output.textContent = 'Idle (click "Run JIT smoke test" to start).';
    }
  }

  return el(
    'div',
    { class: 'panel' },
    el('h2', { text: 'JIT (Tier-1) smoke test' }),
    hint,
    el('div', { class: 'row' }, button),
    output,
  );
}

function renderMicrophonePanel(): HTMLElement {
  const status = el('pre', { text: '' });
  const stateLine = el('div', { class: 'mono', text: 'state=inactive' });
  const statsLine = el('div', { class: 'mono', text: '' });

  const deviceSelect = el('select') as HTMLSelectElement;
  const echoCancellation = el('input', { type: 'checkbox', checked: '' }) as HTMLInputElement;
  const noiseSuppression = el('input', { type: 'checkbox', checked: '' }) as HTMLInputElement;
  const autoGainControl = el('input', { type: 'checkbox', checked: '' }) as HTMLInputElement;
  const bufferMsInput = el('input', { type: 'number', value: '80', min: '10', max: '500', step: '10' }) as HTMLInputElement;
  const mutedInput = el('input', { type: 'checkbox' }) as HTMLInputElement;

  let mic: MicCapture | null = null;
  let lastWorkletStats: { buffered?: number; dropped?: number } = {};

  async function refreshDevices(): Promise<void> {
    deviceSelect.replaceChildren(el('option', { value: '', text: 'default' }));
    if (!navigator.mediaDevices?.enumerateDevices) return;
    const devices = await navigator.mediaDevices.enumerateDevices();
    for (const dev of devices) {
      if (dev.kind !== 'audioinput') continue;
      const label = dev.label || `mic (${dev.deviceId.slice(0, 8)}…)`;
      deviceSelect.append(el('option', { value: dev.deviceId, text: label }));
    }
  }

  function attachToVm(): void {
    const vm = window.__aeroVm;
    if (!vm) return;
    if (micAttachment) {
      vm.setMicrophoneRingBuffer(micAttachment.ringBuffer, { sampleRate: micAttachment.sampleRate });
    } else {
      vm.setMicrophoneRingBuffer(null);
    }
  }

  function update(): void {
    const state = mic?.state ?? 'inactive';
    stateLine.textContent = `state=${state}`;

    const buffered = lastWorkletStats.buffered ?? 0;
    const dropped = lastWorkletStats.dropped ?? 0;

    statsLine.textContent =
      `bufferedSamples=${buffered} droppedSamples=${dropped} ` +
      `device=${deviceSelect.value ? deviceSelect.value.slice(0, 8) + '…' : 'default'}`;
  }

  const startButton = el('button', {
    text: 'Start microphone',
    onclick: async () => {
      status.textContent = '';
      lastWorkletStats = {};
      try {
        if (!navigator.mediaDevices?.getUserMedia) {
          throw new Error('getUserMedia is unavailable in this browser.');
        }
        if (typeof SharedArrayBuffer === 'undefined') {
          throw new Error('SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.');
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

        mic.addEventListener('statechange', update);
        mic.addEventListener('devicechange', () => {
          void refreshDevices();
        });
        mic.addEventListener('message', (event) => {
          const data = (event as MessageEvent).data as unknown;
          if (!data || typeof data !== 'object') return;
          const msg = data as { type?: unknown; buffered?: unknown; dropped?: unknown };
          if (msg.type === 'stats') {
            lastWorkletStats = {
              buffered: typeof msg.buffered === 'number' ? msg.buffered : undefined,
              dropped: typeof msg.dropped === 'number' ? msg.dropped : undefined,
            };
            update();
          }
        });

        await mic.start();
        mic.setMuted(mutedInput.checked);

        micAttachment = { ringBuffer: mic.ringBuffer.sab, sampleRate: mic.actualSampleRate };
        attachToVm();

        update();
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
        micAttachment = null;
        attachToVm();
        update();
      }
    },
  }) as HTMLButtonElement;

  const stopButton = el('button', {
    text: 'Stop microphone',
    onclick: async () => {
      status.textContent = '';
      try {
        await mic?.stop();
        mic = null;
      } catch (err) {
        status.textContent = formatUiErrorMessage(err);
      } finally {
        micAttachment = null;
        attachToVm();
        update();
      }
    },
  }) as HTMLButtonElement;

  mutedInput.addEventListener('change', () => {
    mic?.setMuted(mutedInput.checked);
    update();
  });

  void refreshDevices().then(update);
  navigator.mediaDevices?.addEventListener?.('devicechange', () => void refreshDevices());

  return el(
    'div',
    { class: 'panel' },
    el('h2', { text: 'Microphone (capture)' }),
    el('div', { class: 'row' }, startButton, stopButton, el('label', { text: 'device:' }), deviceSelect),
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'echoCancellation:' }),
      echoCancellation,
      el('label', { text: 'noiseSuppression:' }),
      noiseSuppression,
      el('label', { text: 'autoGainControl:' }),
      autoGainControl,
      el('label', { text: 'bufferMs:' }),
      bufferMsInput,
      el('label', { text: 'mute:' }),
      mutedInput,
    ),
    stateLine,
    statsLine,
    status,
  );
}

function renderPerfPanel(report: PlatformFeatureReport): HTMLElement {
  const supported = report.sharedArrayBuffer && typeof Atomics !== 'undefined';

  const perfApi = (globalThis as typeof globalThis & { aero?: { perf?: unknown } }).aero?.perf;
  const perfObj = perfApi && typeof perfApi === 'object' ? (perfApi as Record<string, unknown>) : null;

  const hasCaptureApi =
    !!perfObj &&
    typeof (perfObj as typeof perfObj & { captureStart?: unknown }).captureStart === 'function' &&
    typeof (perfObj as typeof perfObj & { captureStop?: unknown }).captureStop === 'function' &&
    typeof (perfObj as typeof perfObj & { export?: unknown }).export === 'function';

  const hasTraceApi =
    !!perfObj &&
    typeof (perfObj as typeof perfObj & { traceStart?: unknown }).traceStart === 'function' &&
    typeof (perfObj as typeof perfObj & { traceStop?: unknown }).traceStop === 'function' &&
    typeof (perfObj as typeof perfObj & { exportTrace?: unknown }).exportTrace === 'function';

  const hud = el('pre', {
    text:
      'The Perf HUD overlay is installed.\n' +
      'Toggle via the "Perf HUD" button (top-left) or press F2 / Ctrl+Shift+P.\n' +
      '\n' +
      `SharedArrayBuffer/Atomics: ${supported ? 'supported' : 'missing'}\n` +
      `capture API: ${hasCaptureApi ? 'available' : 'missing'}\n` +
      `trace API: ${hasTraceApi ? 'available' : 'missing'}\n` +
      '\n' +
      'Console:\n' +
      '  window.aero.perf.captureStart();\n' +
      '  window.aero.perf.captureStop();\n' +
      '  window.aero.perf.export();\n' +
      '  window.aero.perf.traceStart();\n' +
      '  window.aero.perf.traceStop();\n' +
      '  await window.aero.perf.exportTrace();\n',
  });

  return el(
    'div',
    { class: 'panel' },
    el('h2', { text: 'Perf HUD + exports' }),
    el(
      'div',
      {
        text:
          'The perf API is installed at startup and used by CI tooling (tools/perf/run.mjs) to capture perf exports.',
      },
    ),
    hud,
  );
}

function renderEmulatorSafetyPanel(): HTMLElement {
  window.__aeroUiTicks ??= 0;
  const uiTickTimer = globalThis.setInterval(() => {
    window.__aeroUiTicks = (window.__aeroUiTicks ?? 0) + 1;
  }, 25);
  (uiTickTimer as unknown as { unref?: () => void }).unref?.();

  const stateLine = el('div', { class: 'mono', id: 'vm-state', text: 'state=stopped' });
  const heartbeatLine = el('div', { class: 'mono', id: 'vm-heartbeat', text: 'heartbeat=0' });
  const tickLine = el('div', { class: 'mono', id: 'vm-ticks', text: 'uiTicks=0' });
  const snapshotSavedLine = el('div', { class: 'mono', id: 'vm-snapshot-saved', text: 'snapshotSavedTo=none' });
  const resourcesLine = el('div', { class: 'mono', id: 'vm-resources', text: 'resources=unknown' });

  const errorOut = el('pre', { id: 'vm-error', text: '' });
  const snapshotOut = el('pre', { id: 'vm-snapshot', text: '' });

  // Keep the default guest RAM small: the VmCoordinator safety panel uses a fake CPU worker and
  // most tests don't need large guest buffers. This reduces baseline allocations in Playwright.
  const guestRamMiB = el('input', { id: 'vm-guest-mib', type: 'number', value: '16', min: '1', step: '1' }) as HTMLInputElement;
  const maxGuestRamMiB = el('input', { id: 'vm-max-guest-mib', type: 'number', value: '512', min: '1', step: '1' }) as HTMLInputElement;
  const maxDiskCacheMiB = el('input', { id: 'vm-max-disk-cache-mib', type: 'number', value: '128', min: '1', step: '1' }) as HTMLInputElement;
  const maxShaderCacheMiB = el('input', { id: 'vm-max-shader-cache-mib', type: 'number', value: '64', min: '1', step: '1' }) as HTMLInputElement;
  const autoSaveSnapshot = el('input', { id: 'vm-auto-snapshot', type: 'checkbox' }) as HTMLInputElement;
  const cacheWriteMiB = el('input', { id: 'vm-cache-write-mib', type: 'number', value: '1', min: '0', step: '1' }) as HTMLInputElement;

  let vm: VmCoordinator | null = null;
  let visibilityListenerInstalled = false;

  function update(): void {
    const state = vm?.state ?? 'stopped';
    stateLine.textContent = `state=${state}`;
    const lastHeartbeat = vm?.lastHeartbeat as { totalInstructions?: number; mic?: unknown } | null | undefined;
    const totalInstructions = lastHeartbeat?.totalInstructions ?? 0;
    const mic =
      lastHeartbeat && typeof lastHeartbeat.mic === 'object'
        ? (lastHeartbeat.mic as { rms?: number; dropped?: number })
        : null;
    const micText = mic ? ` micRms=${(mic.rms ?? 0).toFixed(3)} micDropped=${mic.dropped ?? 0}` : '';
    heartbeatLine.textContent = `lastHeartbeatAt=${vm?.lastHeartbeatAt ?? 0} totalInstructions=${totalInstructions}${micText}`;
    tickLine.textContent = `uiTicks=${window.__aeroUiTicks ?? 0}`;
    let persistedSavedTo = 'none';
    try {
      if (typeof localStorage !== 'undefined' && localStorage.getItem('aero:lastCrashSnapshot')) {
        persistedSavedTo = 'localStorage:aero:lastCrashSnapshot';
      }
    } catch {
      // ignore
    }
    snapshotSavedLine.textContent = `snapshotSavedTo=${vm?.lastSnapshotSavedTo ?? persistedSavedTo}`;

    const resources = (vm?.lastHeartbeat as { resources?: { guestRamBytes?: number; diskCacheBytes?: number; shaderCacheBytes?: number } } | null)
      ?.resources;
    const guestRamBytes = resources?.guestRamBytes ?? 0;
    const diskCacheBytes = resources?.diskCacheBytes ?? 0;
    const shaderCacheBytes = resources?.shaderCacheBytes ?? 0;
    resourcesLine.textContent =
      `guestRamMiB=${(guestRamBytes / (1024 * 1024)).toFixed(1)} ` +
      `diskCacheMiB=${(diskCacheBytes / (1024 * 1024)).toFixed(1)} ` +
      `shaderCacheMiB=${(shaderCacheBytes / (1024 * 1024)).toFixed(1)}`;

    if (vm?.lastSnapshot) {
      snapshotOut.textContent = JSON.stringify(vm.lastSnapshot, null, 2);
    }
  }

  async function ensureVm(): Promise<VmCoordinator> {
    if (vm) return vm;

    const guestBytes = Math.max(1, Number(guestRamMiB.value || 0)) * 1024 * 1024;
    const maxGuestBytes = Math.max(1, Number(maxGuestRamMiB.value || 0)) * 1024 * 1024;
    const maxDiskCacheBytes = Math.max(1, Number(maxDiskCacheMiB.value || 0)) * 1024 * 1024;
    const maxShaderCacheBytes = Math.max(1, Number(maxShaderCacheMiB.value || 0)) * 1024 * 1024;

    vm = new VmCoordinator({
      config: {
        guestRamBytes: guestBytes,
        limits: { maxGuestRamBytes: maxGuestBytes, maxDiskCacheBytes, maxShaderCacheBytes },
        cpu: {
          watchdogTimeoutMs: 250,
          maxSliceMs: 8,
          maxInstructionsPerSlice: 250_000,
          backgroundThrottleMs: 50,
        },
        autoSaveSnapshotOnCrash: autoSaveSnapshot.checked,
      },
    });
    window.__aeroVm = vm;

    if (micAttachment) {
      vm.setMicrophoneRingBuffer(micAttachment.ringBuffer, { sampleRate: micAttachment.sampleRate });
    }

    vm.addEventListener('statechange', update);
    vm.addEventListener('heartbeat', update);
    vm.addEventListener('snapshotSaved', update);
    vm.addEventListener('error', (event) => {
      const detail = (event as CustomEvent).detail as unknown;
      errorOut.textContent = JSON.stringify(detail, null, 2);
      update();
    });

    if (!visibilityListenerInstalled) {
      visibilityListenerInstalled = true;
      document.addEventListener('visibilitychange', () => {
        vm?.setBackgrounded(document.visibilityState !== 'visible');
      });
    }

    update();
    return vm;
  }

  const startCoopButton = el('button', {
    id: 'vm-start-coop',
    text: 'Start (cooperative loop)',
    onclick: async () => {
      errorOut.textContent = '';
      snapshotOut.textContent = '';
      try {
        const inst = await ensureVm();
        await inst.start({ mode: 'cooperativeInfiniteLoop' });
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const startHangButton = el('button', {
    id: 'vm-start-hang',
    text: 'Start (non-yielding loop)',
    onclick: async () => {
      errorOut.textContent = '';
      snapshotOut.textContent = '';
      try {
        const inst = await ensureVm();
        await inst.start({ mode: 'nonYieldingLoop' });
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const startCrashButton = el('button', {
    id: 'vm-start-crash',
    text: 'Start (crash)',
    onclick: async () => {
      errorOut.textContent = '';
      snapshotOut.textContent = '';
      try {
        const inst = await ensureVm();
        await inst.start({ mode: 'crash' });
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const pauseButton = el('button', {
    id: 'vm-pause',
    text: 'Pause',
    onclick: async () => {
      try {
        await vm?.pause();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const resumeButton = el('button', {
    id: 'vm-resume',
    text: 'Resume',
    onclick: async () => {
      try {
        await vm?.resume();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const stepButton = el('button', {
    id: 'vm-step',
    text: 'Step',
    onclick: async () => {
      try {
        await vm?.step();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const resetButton = el('button', {
    id: 'vm-reset',
    text: 'Reset',
    onclick: () => {
      vm?.reset();
      vm = null;
      window.__aeroVm = undefined;
      errorOut.textContent = '';
      snapshotOut.textContent = '';
      update();
    },
  }) as HTMLButtonElement;

  const writeDiskCacheButton = el('button', {
    id: 'vm-write-disk-cache',
    text: 'Write disk cache entry',
    onclick: async () => {
      if (!vm || (vm.state !== 'running' && vm.state !== 'paused')) {
        errorOut.textContent = 'Start the VM first.';
        return;
      }
      try {
        const sizeBytes = Math.max(0, Number(cacheWriteMiB.value || 0)) * 1024 * 1024;
        const result = await vm.writeCacheEntry({ cache: 'disk', sizeBytes });
        if (!result.ok) {
          errorOut.textContent = JSON.stringify(result.error, null, 2);
        }
        update();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
    },
  }) as HTMLButtonElement;

  const writeShaderCacheButton = el('button', {
    id: 'vm-write-shader-cache',
    text: 'Write shader cache entry',
    onclick: async () => {
      if (!vm || (vm.state !== 'running' && vm.state !== 'paused')) {
        errorOut.textContent = 'Start the VM first.';
        return;
      }
      try {
        const sizeBytes = Math.max(0, Number(cacheWriteMiB.value || 0)) * 1024 * 1024;
        const result = await vm.writeCacheEntry({ cache: 'shader', sizeBytes });
        if (!result.ok) {
          errorOut.textContent = JSON.stringify(result.error, null, 2);
        }
        update();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
    },
  }) as HTMLButtonElement;

  const loadSavedSnapshotButton = el('button', {
    id: 'vm-load-saved-snapshot',
    text: 'Load saved crash snapshot',
    onclick: async () => {
      try {
        const saved = await VmCoordinator.loadSavedCrashSnapshot();
        if (!saved) {
          errorOut.textContent = 'No saved crash snapshot found.';
          return;
        }
        errorOut.textContent = `Loaded snapshot from ${saved.savedTo}`;
        snapshotOut.textContent = JSON.stringify(saved.snapshot, null, 2);
        update();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
    },
  }) as HTMLButtonElement;

  const clearSavedSnapshotButton = el('button', {
    id: 'vm-clear-saved-snapshot',
    text: 'Clear saved snapshot',
    onclick: async () => {
      try {
        await VmCoordinator.clearSavedCrashSnapshot();
        if (vm) vm.lastSnapshotSavedTo = null;
        update();
      } catch (err) {
        errorOut.textContent = formatUiErrorMessage(err);
      }
    },
  }) as HTMLButtonElement;

  const updateTimer = globalThis.setInterval(update, 250);
  (updateTimer as unknown as { unref?: () => void }).unref?.();

  return el(
    'div',
    { class: 'panel', id: 'vm-safety-panel' },
    el('h2', { text: 'Emulator safety controls (watchdog + pause/step)' }),
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'guestMiB:' }),
      guestRamMiB,
      el('label', { text: 'maxMiB:' }),
      maxGuestRamMiB,
      el('label', { text: 'diskCacheMiB:' }),
      maxDiskCacheMiB,
      el('label', { text: 'shaderCacheMiB:' }),
      maxShaderCacheMiB,
      el('label', { text: 'auto-save snapshot on crash:' }),
      autoSaveSnapshot,
    ),
    el('div', { class: 'row' }, el('label', { text: 'cacheWriteMiB:' }), cacheWriteMiB, writeDiskCacheButton, writeShaderCacheButton),
    el(
      'div',
      { class: 'row' },
      startCoopButton,
      startHangButton,
      startCrashButton,
      pauseButton,
      resumeButton,
      stepButton,
      resetButton,
      loadSavedSnapshotButton,
      clearSavedSnapshotButton,
    ),
    stateLine,
    heartbeatLine,
    tickLine,
    snapshotSavedLine,
    resourcesLine,
    el('h3', { text: 'Last error' }),
    errorOut,
    el('h3', { text: 'Last snapshot' }),
    snapshotOut,
  );
}

function isNetTraceBackend(value: unknown): value is NetTraceBackend {
  if (!value || typeof value !== 'object') return false;
  const maybe = value as Record<string, unknown>;
  return (
    typeof maybe.isEnabled === 'function' &&
    typeof maybe.enable === 'function' &&
    typeof maybe.disable === 'function' &&
    typeof maybe.downloadPcapng === 'function'
  );
}

function resolveNetTraceBackend(): NetTraceBackend {
  // Lazily resolve the global backend at call time so the UI keeps working even
  // if the `WorkerCoordinator` (which installs `window.aero.netTrace`) is created
  // after the panel is rendered.
  const resolveInstalled = (): NetTraceBackend | null => {
    const aero = (window as unknown as { aero?: unknown }).aero;
    if (!aero || typeof aero !== 'object') return null;
    const candidate = (aero as { netTrace?: unknown }).netTrace;
    return isNetTraceBackend(candidate) ? candidate : null;
  };

  const missingBackendError = () => new Error('Network tracing backend not installed (window.aero.netTrace missing).');

  return {
    isEnabled: () => resolveInstalled()?.isEnabled() ?? false,
    enable: () => {
      const backend = resolveInstalled();
      if (!backend) throw missingBackendError();
      backend.enable();
    },
    disable: () => {
      resolveInstalled()?.disable();
    },
    downloadPcapng: async () => {
      const backend = resolveInstalled();
      if (!backend) throw missingBackendError();
      return await backend.downloadPcapng();
    },
    exportPcapng: async () => {
      const backend = resolveInstalled();
      if (!backend) throw missingBackendError();
      // Fall back to the draining export when the backend does not support
      // non-draining snapshot exports.
      if (backend.exportPcapng) {
        return await backend.exportPcapng();
      }
      return await backend.downloadPcapng();
    },
    clear: async () => {
      const backend = resolveInstalled();
      if (!backend) return;
      if (backend.clear) {
        await backend.clear();
      } else {
        backend.clearCapture?.();
      }
    },
    getStats: async () => {
      const backend = resolveInstalled();
      if (!backend?.getStats) {
        return { enabled: false, records: 0, bytes: 0 };
      }
      return await backend.getStats();
    },
    // Legacy clear implementation used by earlier backends / UIs.
    clearCapture: () => {
      const backend = resolveInstalled();
      if (!backend) return;
      if (backend.clearCapture) {
        backend.clearCapture();
      } else {
        void backend.clear?.();
      }
    },
  };
}

function renderNetTracePanel(): HTMLElement {
  const panel = el('div', { class: 'panel' }, el('h2', { text: 'Network trace (PCAPNG)' }));
  installNetTraceUI(panel, resolveNetTraceBackend());
  return panel;
}

function render(): void {
  const app = document.querySelector<HTMLDivElement>('#app');
  if (!app) throw new Error('Missing #app element');

  const report = detectPlatformFeatures();
  const missing = explainMissingRequirements(report);

  app.replaceChildren(
    el('h1', { text: 'Aero Platform Capabilities' }),
    el(
      'div',
      { class: `panel ${missing.length ? 'missing' : ''}` },
      el('h2', { text: 'Required features' }),
      missing.length ? el('ul', {}, ...missing.map((m) => el('li', { text: m }))) : el('div', { text: 'All required features appear to be available.' }),
    ),
    el('div', { class: 'panel' }, el('h2', { text: 'Capability report' }), renderCapabilityTable(report)),
    renderWebGpuPanel(),
    renderWebUsbPanel(report),
    renderOpfsPanel(),
    renderRemoteDiskPanel(),
    renderAudioPanel(),
    renderInputBackendHudPanel(),
    renderJitSmokePanel(report),
    renderMicrophonePanel(),
    renderPerfPanel(report),
    renderHotspotsPanel(report),
    renderEmulatorSafetyPanel(),
    renderNetTracePanel(),
  );
}

render();

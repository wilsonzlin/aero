// NOTE: Repo-root Vite harness entrypoint.
//
// This file exists for debugging and Playwright smoke tests. The production
// browser host lives under `web/` (ADR 0001).
import './style.css';

import { installPerfHud } from '../web/src/perf/hud_entry';
import { perf } from '../web/src/perf/perf';
import { installAeroGlobal } from '../web/src/runtime/aero_global';
import { installNetTraceUI, type NetTraceBackend } from '../web/src/net/trace_ui';
import { RuntimeDiskClient } from '../web/src/storage/runtime_disk_client';
import { formatByteSize } from '../web/src/storage/disk_image_store';

import { createHotspotsPanel } from './ui/hud_hotspots.js';
import type { HotspotEntry, PerfExport as HotspotPerfExport } from './perf/aero_perf.js';

import { createAudioOutput } from './platform/audio';
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from './platform/features';
import { importFileToOpfs } from './platform/opfs';
import { createWebUsbBroker } from './platform/webusb_broker';
import { requestWebGpuDevice } from './platform/webgpu';
import { VmCoordinator } from './emulator/vmCoordinator.js';
import { MicCapture } from '../web/src/audio/mic_capture';
import type { AeroConfig } from '../web/src/config/aero_config';
import { WorkerCoordinator } from '../web/src/runtime/coordinator';
import { explainWebUsbError, formatWebUsbError } from '../web/src/platform/webusb_troubleshooting';

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
if (new URLSearchParams(location.search).has('trace')) perf.traceStart();

// Install optional `window.aero.bench` helpers so automation can invoke
// microbenchmarks without requiring the emulator/guest OS.
installAeroGlobal();

// Updated by the microphone UI and read by the VM UI so that new VM instances
// automatically inherit the current mic attachment (if any).
// `sampleRate` is the actual capture sample rate (AudioContext.sampleRate).
let micAttachment: { ringBuffer: SharedArrayBuffer; sampleRate: number } | null = null;

type CpuWorkerToMainMessage =
  | { type: 'CpuWorkerReady' }
  | {
      type: 'CpuWorkerResult';
      jit_executions: number;
      helper_executions: number;
      interp_executions: number;
      installed_table_index: number | null;
      runtime_installed_entry_rip: number | null;
      runtime_installed_table_index: number | null;
    }
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

async function runJitSmokeTest(output: HTMLPreElement): Promise<void> {
  output.textContent = '';
  window.__jit_smoke_result = undefined;

  let cpuWorker: Worker;
  try {
    cpuWorker = new Worker(new URL('./workers/cpu-worker.ts', import.meta.url), {
      type: 'module',
    });
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    window.__jit_smoke_result = { type: 'CpuWorkerError', reason };
    output.textContent = reason;
    return;
  }

  const result = await new Promise<CpuWorkerToMainMessage>((resolve) => {
    const settle = (msg: CpuWorkerToMainMessage) => {
      resolve(msg);
      cpuWorker.terminate();
    };

    cpuWorker.addEventListener('message', (ev: MessageEvent<CpuWorkerToMainMessage>) => {
      const msg = ev.data;
      if (msg.type === 'CpuWorkerReady') {
        const start: CpuWorkerStartMessage = { type: 'CpuWorkerStart' };
        cpuWorker.postMessage(start);
        return;
      }
      if (msg.type === 'CpuWorkerResult' || msg.type === 'CpuWorkerError') {
        settle(msg);
      }
    });

    cpuWorker.addEventListener('error', (ev) => {
      settle({ type: 'CpuWorkerError', reason: ev instanceof ErrorEvent ? ev.message : String(ev) });
    });

    cpuWorker.addEventListener('messageerror', () => {
      settle({ type: 'CpuWorkerError', reason: 'worker message deserialization failed' });
    });
  });

  window.__jit_smoke_result = result;

  output.textContent = JSON.stringify(result, null, 2);
}

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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (node as any)[key.toLowerCase()] = value;
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
        output.textContent = err instanceof Error ? err.message : String(err);
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
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  device: any,
): Record<string, unknown> | null {
  if (!device || typeof device !== 'object') return null;
  return {
    productName: device.productName,
    manufacturerName: device.manufacturerName,
    serialNumber: device.serialNumber,
    vendorId: device.vendorId,
    productId: device.productId,
    opened: device.opened,
  };
}

async function runWebUsbProbeWorker(
  msg: unknown,
  { timeoutMs = 10_000, transfer = [] }: { timeoutMs?: number; transfer?: Transferable[] } = {},
): Promise<unknown> {
  type Pending = { resolve: (value: unknown) => void; reject: (reason: unknown) => void; timeoutHandle: number };

  let worker = (runWebUsbProbeWorker as unknown as { worker?: Worker }).worker;
  let nextId = (runWebUsbProbeWorker as unknown as { nextId?: number }).nextId ?? 1;
  let pending = (runWebUsbProbeWorker as unknown as { pending?: Map<number, Pending> }).pending;
  if (!pending) {
    pending = new Map();
    (runWebUsbProbeWorker as unknown as { pending?: Map<number, Pending> }).pending = pending;
  }
  const pendingMap = pending;

  function rejectAll(err: unknown): void {
    for (const [id, entry] of pendingMap.entries()) {
      pendingMap.delete(id);
      window.clearTimeout(entry.timeoutHandle);
      entry.reject(err);
    }
  }

  if (!worker) {
    worker = new Worker(new URL('./workers/webusb-probe-worker.ts', import.meta.url), { type: 'module' });
    (runWebUsbProbeWorker as unknown as { worker?: Worker }).worker = worker;

    worker.addEventListener('message', (ev: MessageEvent) => {
      const data = ev.data as { id?: unknown } | null;
      const id = typeof data?.id === 'number' ? data.id : null;
      if (id === null) return;
      const entry = pendingMap.get(id);
      if (!entry) return;
      pendingMap.delete(id);
      window.clearTimeout(entry.timeoutHandle);
      entry.resolve(ev.data);
    });

    worker.addEventListener('messageerror', () => {
      rejectAll(new Error('WebUSB probe worker message deserialization failed'));
      // Force a fresh worker next time.
      worker?.terminate();
      (runWebUsbProbeWorker as unknown as { worker?: Worker }).worker = undefined;
    });

    worker.addEventListener('error', (ev) => {
      rejectAll(new Error(ev instanceof ErrorEvent ? ev.message : String(ev)));
      worker?.terminate();
      (runWebUsbProbeWorker as unknown as { worker?: Worker }).worker = undefined;
    });
  }

  const id = nextId;
  (runWebUsbProbeWorker as unknown as { nextId?: number }).nextId = nextId + 1;

  const payload: Record<string, unknown> =
    msg && typeof msg === 'object' && !Array.isArray(msg) ? { ...(msg as Record<string, unknown>) } : { value: msg };
  payload.id = id;

  return await new Promise((resolve, reject) => {
    const timeoutHandle = window.setTimeout(() => {
      pendingMap.delete(id);
      reject(new Error(`WebUSB probe worker timed out after ${timeoutMs}ms`));
    }, timeoutMs);

    pendingMap.set(id, { resolve, reject, timeoutHandle });

    try {
      worker!.postMessage(payload, transfer);
    } catch (err) {
      pendingMap.delete(id);
      window.clearTimeout(timeoutHandle);
      reject(err);
    }
  });
}

function renderWebUsbPanel(report: PlatformFeatureReport): HTMLElement {
  const info = el('pre', { text: '' });
  const output = el('pre', { text: '' });
  const brokerOutput = el('pre', { text: '' });
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

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let selectedDevice: any | null = null;
  let selectedSummary: Record<string, unknown> | null = null;

  function serializeError(err: unknown): { name: string; message: string } {
    if (err instanceof DOMException) return { name: err.name, message: err.message };
    if (err instanceof Error) return { name: err.name, message: err.message };
    if (err && typeof err === 'object') {
      const maybe = err as { name?: unknown; message?: unknown };
      const name = typeof maybe.name === 'string' ? maybe.name : 'Error';
      const message = typeof maybe.message === 'string' ? maybe.message : String(err);
      return { name, message };
    }
    return { name: 'Error', message: String(err) };
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
    const liveSummary = selectedDevice ? summarizeUsbDevice(selectedDevice) : selectedSummary;
    if (selectedDevice) selectedSummary = liveSummary;
    info.textContent =
      `isSecureContext=${(globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true}\n` +
      `navigator.usb=${report.webusb ? 'present' : 'missing'}\n` +
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
    try {
      const resp = await runWebUsbProbeWorker({ type: 'probe' });
      output.textContent = JSON.stringify(resp, null, 2);
    } catch (err) {
      showError(err);
      output.textContent = JSON.stringify({ ok: false, error: serializeError(err) }, null, 2);
    }
  }

  const requestButton = el('button', {
    text: 'Request USB device (chooser)',
    onclick: async () => {
      output.textContent = '';
      clearError();
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

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const usb: any = (navigator as unknown as { usb?: unknown }).usb;
      if (!usb || typeof usb.requestDevice !== 'function') {
        output.textContent = 'navigator.usb.requestDevice is unavailable in this context.';
        return;
      }

      try {
        // Must be called directly from the user gesture handler (transient user activation).
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const options: any = {};
        if (acceptAllDevicesInput.checked) {
          options.filters = [];
          options.acceptAllDevices = true;
        } else {
          // Note: some Chromium versions require at least one filter; `{}` is a best-effort "match all"
          // filter for probing. If this fails, specify vendorId/productId explicitly.
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const filters: any[] = [];
          if (vendorId !== null) {
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            const filter: any = { vendorId };
            if (productId !== null) filter.productId = productId;
            filters.push(filter);
          } else {
            filters.push({});
          }
          options.filters = filters;
        }

        selectedDevice = await usb.requestDevice(options);
        selectedSummary = summarizeUsbDevice(selectedDevice);
        updateInfo();

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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const usb: any = (navigator as unknown as { usb?: unknown }).usb;
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

  const broker = createWebUsbBroker();
  let brokerWorker: Worker | null = null;
  const brokerLines: string[] = [];

  const logBroker = (line: string) => {
    brokerLines.push(line);
    brokerOutput.textContent = brokerLines.join('\n');
  };

  const ensureBrokerWorker = () => {
    if (brokerWorker) return;
    logBroker('Starting WebUSB demo worker…');
    brokerWorker = new Worker(new URL('./workers/webusb-demo.worker.ts', import.meta.url), { type: 'module' });
    broker.attachToWorker(brokerWorker);

    brokerWorker.addEventListener('message', (event: MessageEvent) => {
      const data = event.data as unknown;
      if (!data || typeof data !== 'object') return;
      const msg = data as { type?: unknown; line?: unknown; ok?: unknown; error?: unknown };

      switch (msg.type) {
        case 'WebUsbDemoReady':
          logBroker('Worker ready.');
          break;
        case 'WebUsbDemoLog':
          if (typeof msg.line === 'string') logBroker(msg.line);
          break;
        case 'WebUsbDemoDone':
          if (msg.ok === true) logBroker('Demo done.');
          else logBroker(`Demo failed: ${typeof msg.error === 'string' ? msg.error : 'unknown error'}`);
          break;
        default:
          break;
      }
    });

    brokerWorker.addEventListener('error', (event) => {
      logBroker(`Worker error: ${event instanceof ErrorEvent ? event.message : String(event)}`);
    });

    brokerWorker.addEventListener('messageerror', () => {
      logBroker('Worker message deserialization failed.');
    });
  };

  const brokerDemoButton = el('button', {
    text: 'Request device + run worker I/O via broker',
    onclick: async () => {
      brokerLines.length = 0;
      brokerOutput.textContent = '';
      clearError();

      if (!report.webusb) {
        logBroker('WebUSB is unavailable (navigator.usb is undefined).');
        return;
      }

      const vendorId = parseUsbId(vendorIdInput.value);
      const productId = parseUsbId(productIdInput.value);
      if (productId !== null && vendorId === null) {
        logBroker('productId filter requires vendorId.');
        return;
      }

      // Some Chromium versions require at least one filter; `{}` is a best-effort "match all" filter for the demo.
      const filters: USBDeviceFilter[] = [];
      if (vendorId !== null) {
        const filter: USBDeviceFilter = { vendorId };
        if (productId !== null) filter.productId = productId;
        filters.push(filter);
      } else {
        filters.push({});
      }

      try {
        // Must be called directly from the user gesture handler (transient user activation).
        ensureBrokerWorker();
        const device = await broker.requestDevice({ filters });
        logBroker(
          `Selected deviceId=${device.deviceId} vendorId=0x${device.vendorId.toString(16)} productId=0x${device.productId.toString(16)}` +
            (device.productName ? ` productName=${device.productName}` : ''),
        );
        brokerWorker?.postMessage({ type: 'WebUsbDemoRun', deviceId: device.deviceId });
      } catch (err) {
        showError(err);
        logBroker(err instanceof Error ? err.message : String(err));
      }
    },
  });

  // Initialize info + control state.
  updateInfo();
  // Probe worker-side WebUSB semantics on load so the panel reports both main + worker support.
  void runWorkerProbe();

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
    el('h3', { text: 'Worker I/O via broker (MessagePort RPC)' }),
    el('div', { class: 'row' }, brokerDemoButton),
    brokerOutput,
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
        status.textContent = err instanceof Error ? err.message : String(err);
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

  function stopTone() {
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  function startTone(output: Exclude<Awaited<ReturnType<typeof createAudioOutput>>, { enabled: false }>) {
    stopTone();

    const freqHz = 440;
    const gain = 0.1;
    const channelCount = output.ringBuffer.channelCount;
    const sr = output.context.sampleRate;

    function writeTone(frames: number) {
      const buf = new Float32Array(frames * channelCount);
      for (let i = 0; i < frames; i++) {
        const s = Math.sin(tonePhase * 2 * Math.PI) * gain;
        for (let c = 0; c < channelCount; c++) buf[i * channelCount + c] = s;
        tonePhase += freqHz / sr;
        if (tonePhase >= 1) tonePhase -= 1;
      }
      output.writeInterleaved(buf, sr);
    }

    // Prefill ~100ms to avoid startup underruns.
    writeTone(Math.floor(sr / 10));

    toneTimer = window.setInterval(() => {
      const target = Math.floor(sr / 5); // ~200ms buffered
      const level = output.getBufferLevelFrames();
      const need = Math.max(0, target - level);
      if (need > 0) writeTone(need);

      status.textContent =
        `AudioContext: ${output.context.state}\n` +
        `sampleRate: ${sr}\n` +
        `bufferLevelFrames: ${output.getBufferLevelFrames()}\n` +
        `underrunFrames: ${output.getUnderrunCount()}`;
    }, 50);
  }

  const button = el("button", {
    id: "init-audio-output",
    text: "Init audio output (test tone)",
    onclick: async () => {
      status.textContent = "";
      const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
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

      const workerConfig: AeroConfig = {
        guestMemoryMiB: 256,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
      };

      try {
        workerCoordinator.start(workerConfig);
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
      (globalThis as typeof globalThis & { __aeroAudioOutputWorker?: unknown }).__aeroAudioOutputWorker = output;
      (globalThis as typeof globalThis & { __aeroAudioToneBackendWorker?: unknown }).__aeroAudioToneBackendWorker =
        "cpu-worker-wasm";

      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      try {
        // Prefill the entire ring with silence so the CPU worker has time to attach
        // and begin writing without incurring startup underruns.
        output.writeInterleaved(
          new Float32Array(output.ringBuffer.capacityFrames * output.ringBuffer.channelCount),
          output.context.sampleRate,
        );

        workerCoordinator.setAudioOutputRingBuffer(
          output.ringBuffer.buffer,
          output.context.sampleRate,
          output.ringBuffer.channelCount,
          output.ringBuffer.capacityFrames,
        );

        await output.resume();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }

      status.textContent = "Audio initialized (worker tone backend).";
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Audio" }),
    el("div", { class: "row" }, button, workerButton),
    status,
  );
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
  const urlInput = el('input', { type: 'url', placeholder: 'https://example.com/disk.raw' }) as HTMLInputElement;
  const blockSizeInput = el('input', { type: 'number', value: String(1024), min: '4' }) as HTMLInputElement;
  const cacheLimitInput = el('input', { type: 'number', value: String(512), min: '0' }) as HTMLInputElement;
  const prefetchInput = el('input', { type: 'number', value: String(2), min: '0' }) as HTMLInputElement;
  const maxConcurrentFetchesInput = el('input', { type: 'number', value: String(4), min: '1' }) as HTMLInputElement;
  const stats = el('pre', { text: '' });
  const output = el('pre', { text: '' });

  const probeButton = el('button', { text: 'Probe Range support' }) as HTMLButtonElement;
  const readButton = el('button', { text: 'Read sample bytes' }) as HTMLButtonElement;
  const flushButton = el('button', { text: 'Flush cache' }) as HTMLButtonElement;
  const clearButton = el('button', { text: 'Clear cache' }) as HTMLButtonElement;
  const closeButton = el('button', { text: 'Close' }) as HTMLButtonElement;
  const progress = el('progress', { value: '0', max: '1', style: 'width: 320px' }) as HTMLProgressElement;

  const client = new RuntimeDiskClient();
  let handle: number | null = null;
  let statsPollPending = false;

  function formatMaybeBytes(bytes: number | null): string {
    return bytes === null ? 'off' : formatByteSize(bytes);
  }

  function updateButtons(): void {
    const enabled = enabledInput.checked;
    probeButton.disabled = !enabled;
    readButton.disabled = !enabled;
    flushButton.disabled = !enabled || handle === null;
    clearButton.disabled = !enabled || handle === null;
    closeButton.disabled = !enabled || handle === null;
  }

  function updateModeUi(): void {
    const chunked = modeSelect.value === 'chunked';
    blockSizeInput.disabled = chunked;
    maxConcurrentFetchesInput.disabled = !chunked;
    urlInput.placeholder = chunked ? 'https://example.com/manifest.json' : 'https://example.com/disk.raw';
    probeButton.textContent = chunked ? 'Fetch manifest' : 'Probe Range support';
  }

  enabledInput.addEventListener('change', () => {
    if (!enabledInput.checked) {
      void closeHandle();
    }
    updateButtons();
  });
  modeSelect.addEventListener('change', () => {
    void closeHandle();
    updateModeUi();
    updateButtons();
  });
  updateModeUi();
  updateButtons();

  async function closeHandle(): Promise<void> {
    if (handle === null) return;
    const cur = handle;
    handle = null;
    try {
      await client.closeDisk(cur);
    } catch (err) {
      // Best-effort; if the worker is gone, nothing else to do.
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  }

  async function ensureOpen(): Promise<number> {
    if (handle !== null) return handle;

    const url = urlInput.value.trim();
    if (!url) throw new Error('Enter a URL first.');

    const cacheLimitMiB = Number(cacheLimitInput.value);
    const cacheLimitBytes = cacheLimitMiB <= 0 ? null : cacheLimitMiB * 1024 * 1024;

    const prefetchSequential = Math.max(0, Number(prefetchInput.value) | 0);
    const opened =
      modeSelect.value === 'chunked'
        ? await client.openChunked(url, {
            cacheLimitBytes,
            credentials: credentialsSelect.value as RequestCredentials,
            prefetchSequentialChunks: prefetchSequential,
            maxConcurrentFetches: Math.max(1, Number(maxConcurrentFetchesInput.value) | 0),
            cacheBackend: cacheBackendSelect.value === 'auto' ? undefined : (cacheBackendSelect.value as 'opfs' | 'idb'),
          })
        : await client.openRemote(url, {
            blockSize: Number(blockSizeInput.value) * 1024,
            cacheLimitBytes,
            credentials: credentialsSelect.value as RequestCredentials,
            prefetchSequentialBlocks: prefetchSequential,
            cacheBackend: cacheBackendSelect.value === 'auto' ? undefined : (cacheBackendSelect.value as 'opfs' | 'idb'),
          });
    handle = opened.handle;
    updateButtons();
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

      const hitRateDenom = remote.cacheHits + remote.cacheMisses;
      const hitRate = hitRateDenom > 0 ? remote.cacheHits / hitRateDenom : 0;
      const cacheCoverage = remote.totalSize > 0 ? remote.cachedBytes / remote.totalSize : 0;
      const downloadAmplification = res.io.bytesRead > 0 ? remote.bytesDownloaded / res.io.bytesRead : 0;

      stats.textContent =
        `imageSize=${formatByteSize(remote.totalSize)}\n` +
        `cache=${formatByteSize(remote.cachedBytes)} (${(cacheCoverage * 100).toFixed(2)}%) limit=${formatMaybeBytes(remote.cacheLimitBytes)}\n` +
        `blockSize=${formatByteSize(remote.blockSize)}\n` +
        `ioReads=${res.io.reads} inflightReads=${res.io.inflightReads} lastReadMs=${res.io.lastReadMs === null ? '—' : res.io.lastReadMs.toFixed(1)}\n` +
        `ioBytesRead=${formatByteSize(res.io.bytesRead)} downloadAmp=${downloadAmplification.toFixed(2)}x\n` +
        `requests=${remote.requests} bytesDownloaded=${formatByteSize(remote.bytesDownloaded)}\n` +
        `blockRequests=${remote.blockRequests} hits=${remote.cacheHits} misses=${remote.cacheMisses} inflightJoins=${remote.inflightJoins} hitRate=${(hitRate * 100).toFixed(1)}%\n` +
        `inflightFetches=${remote.inflightFetches} lastFetchMs=${remote.lastFetchMs === null ? '—' : remote.lastFetchMs.toFixed(1)}\n`;
    } catch (err) {
      stats.textContent = err instanceof Error ? err.message : String(err);
    } finally {
      statsPollPending = false;
    }
  }

  window.setInterval(() => void refreshStats(), 250);

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
      output.textContent = err instanceof Error ? err.message : String(err);
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
      output.textContent = err instanceof Error ? err.message : String(err);
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
      output.textContent = err instanceof Error ? err.message : String(err);
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
      progress.value = 1;
      void refreshStats();
      output.textContent = 'Cache cleared.';
      updateButtons();
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
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
      el('label', { text: 'Block KiB (range):' }),
      blockSizeInput,
      el('label', { text: 'Cache MiB (0=off):' }),
      cacheLimitInput,
      el('label', { text: 'Prefetch:' }),
      prefetchInput,
      el('label', { text: 'Max inflight (chunked):' }),
      maxConcurrentFetchesInput,
      probeButton,
      readButton,
      flushButton,
      clearButton,
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
      output.textContent = err instanceof Error ? err.message : String(err);
    });
  };
  button.onclick = run;

  if (!enabled) {
    button.disabled = true;
    output.textContent = `Skipped (${!report.wasmThreads ? 'wasmThreads=false' : 'jit_dynamic_wasm=false'}).`;
  } else {
    run();
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
        status.textContent = err instanceof Error ? err.message : String(err);
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
        status.textContent = err instanceof Error ? err.message : String(err);
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
  globalThis.setInterval(() => {
    window.__aeroUiTicks = (window.__aeroUiTicks ?? 0) + 1;
  }, 25);

  const stateLine = el('div', { class: 'mono', id: 'vm-state', text: 'state=stopped' });
  const heartbeatLine = el('div', { class: 'mono', id: 'vm-heartbeat', text: 'heartbeat=0' });
  const tickLine = el('div', { class: 'mono', id: 'vm-ticks', text: 'uiTicks=0' });
  const snapshotSavedLine = el('div', { class: 'mono', id: 'vm-snapshot-saved', text: 'snapshotSavedTo=none' });
  const resourcesLine = el('div', { class: 'mono', id: 'vm-resources', text: 'resources=unknown' });

  const errorOut = el('pre', { id: 'vm-error', text: '' });
  const snapshotOut = el('pre', { id: 'vm-snapshot', text: '' });

  const guestRamMiB = el('input', { id: 'vm-guest-mib', type: 'number', value: '64', min: '1', step: '1' }) as HTMLInputElement;
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
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
        errorOut.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  globalThis.setInterval(update, 250);

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
  const aero = ((window as unknown as { aero?: Record<string, unknown> }).aero ??= {});
  const candidate = aero.netTrace;
  if (isNetTraceBackend(candidate)) return candidate;
  return {
    isEnabled: () => false,
    enable: () => {
      throw new Error('Network tracing backend not installed (window.aero.netTrace missing).');
    },
    disable: () => {},
    downloadPcapng: async () => {
      throw new Error('Network tracing backend not installed (window.aero.netTrace missing).');
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
    renderJitSmokePanel(report),
    renderMicrophonePanel(),
    renderPerfPanel(report),
    renderHotspotsPanel(report),
    renderEmulatorSafetyPanel(),
    renderNetTracePanel(),
  );
}

render();

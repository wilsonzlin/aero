// NOTE: Repo-root Vite harness entrypoint.
//
// This file exists for debugging and Playwright smoke tests. The production
// browser host lives under `web/` (ADR 0001).
import './style.css';

import { PerfAggregator, PerfWriter, WorkerKind, createPerfChannel } from '../web/src/perf/index.js';
import { installAeroGlobal } from '../web/src/runtime/aero_global';
import { installNetTraceUI, type NetTraceBackend } from '../web/src/net/trace_ui';
import { RuntimeDiskClient } from '../web/src/storage/runtime_disk_client';
import { formatByteSize } from '../web/src/storage/disk_image_store';

import { createHotspotsPanel } from './ui/hud_hotspots.js';

import { createAudioOutput } from './platform/audio';
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from './platform/features';
import { importFileToOpfs } from './platform/opfs';
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

// Install optional `window.aero.bench` helpers early so automation can invoke
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
  { timeoutMs = 10_000 } = {},
): Promise<unknown> {
  const worker = new Worker(new URL('./workers/webusb-probe-worker.ts', import.meta.url), { type: 'module' });

  return await new Promise((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      worker.terminate();
      reject(new Error(`WebUSB probe worker timed out after ${timeoutMs}ms`));
    }, timeoutMs);

    const cleanup = () => {
      window.clearTimeout(timeout);
      worker.terminate();
    };

    worker.addEventListener('message', (ev: MessageEvent) => {
      cleanup();
      resolve(ev.data);
    });

    worker.addEventListener('messageerror', () => {
      cleanup();
      reject(new Error('WebUSB probe worker message deserialization failed'));
    });

    worker.addEventListener('error', (ev) => {
      cleanup();
      reject(new Error(ev instanceof ErrorEvent ? ev.message : String(ev)));
    });

    try {
      worker.postMessage(msg);
    } catch (err) {
      cleanup();
      reject(err);
    }
  });
}

function renderWebUsbPanel(report: PlatformFeatureReport): HTMLElement {
  const info = el('pre', { text: '' });
  const output = el('pre', { text: '' });
  const errorTitle = el('div', { class: 'bad', text: '' });
  const errorDetails = el('div', { class: 'hint', text: '' });
  const errorRaw = el('pre', { class: 'mono', text: '' });
  const errorHints = el('ul');
  const vendorIdInput = el('input', { type: 'text', placeholder: '0x1234 (optional)' }) as HTMLInputElement;
  const productIdInput = el('input', { type: 'text', placeholder: '0x5678 (optional)' }) as HTMLInputElement;
  const interfaceSelect = el('select') as HTMLSelectElement;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let selectedDevice: any | null = null;

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
    info.textContent =
      `isSecureContext=${(globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true}\n` +
      `navigator.usb=${report.webusb ? 'present' : 'missing'}\n` +
      `userActivation.isActive=${userActivation?.isActive ?? 'n/a'}\n` +
      `userActivation.hasBeenActive=${userActivation?.hasBeenActive ?? 'n/a'}\n` +
      `selectedDevice=${selectedDevice ? JSON.stringify(summarizeUsbDevice(selectedDevice)) : 'none'}\n`;

    const hasSelected = !!selectedDevice;
    const enabled = report.webusb && hasSelected;
    openButton.disabled = !enabled;
    closeButton.disabled = !enabled;
    claimButton.disabled = !enabled;
    interfaceSelect.disabled = !enabled;
    refreshInterfaceSelect();
  }

  const requestButton = el('button', {
    text: 'Request USB device (chooser)',
    onclick: async () => {
      output.textContent = '';
      clearError();
      selectedDevice = null;
      updateInfo();

      if (!report.webusb) {
        output.textContent = 'WebUSB is unavailable (navigator.usb is undefined).';
        return;
      }

      const vendorId = parseUsbId(vendorIdInput.value);
      const productId = parseUsbId(productIdInput.value);
      if (productId !== null && vendorId === null) {
        output.textContent = 'productId filter requires vendorId.';
        return;
      }

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const usb: any = (navigator as unknown as { usb?: unknown }).usb;
      if (!usb || typeof usb.requestDevice !== 'function') {
        output.textContent = 'navigator.usb.requestDevice is unavailable in this context.';
        return;
      }

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

      try {
        // Must be called directly from the user gesture handler (transient user activation).
        selectedDevice = await usb.requestDevice({ filters });
        updateInfo();
        output.textContent = JSON.stringify({ selected: summarizeUsbDevice(selectedDevice) }, null, 2);
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
      output.textContent = '';
      clearError();
      try {
        const resp = await runWebUsbProbeWorker({ type: 'probe' });
        output.textContent = JSON.stringify(resp, null, 2);
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  const cloneButton = el('button', {
    text: 'Try sending selected device to worker (structured clone)',
    onclick: async () => {
      output.textContent = '';
      clearError();
      if (!selectedDevice) {
        output.textContent = 'Select a device first (Request USB device).';
        return;
      }

      try {
        const resp = await runWebUsbProbeWorker({ type: 'clone-test', device: selectedDevice });
        output.textContent = JSON.stringify(resp, null, 2);
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  // Initialize info + control state.
  updateInfo();

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
      el('label', { text: 'vendorId:' }),
      vendorIdInput,
      el('label', { text: 'productId:' }),
      productIdInput,
      requestButton,
      listButton,
    ),
    el('div', { class: 'row' }, openButton, closeButton, interfaceSelect, claimButton),
    el('div', { class: 'row' }, workerProbeButton, cloneButton),
    output,
    errorTitle,
    errorDetails,
    errorRaw,
    errorHints,
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
  // `window.aero.perf.export()` is installed by `startPerfTelemetry`.
  // Until then (or when disabled), render an empty panel.
  if (!report.wasmThreads) {
    return el(
      'div',
      { class: 'panel' },
      el('h2', { text: 'Hotspots' }),
      el('pre', { text: 'Hotspots unavailable: requires cross-origin isolation + SharedArrayBuffer + Atomics.' }),
    );
  }

  const perfFacade = {
    export: () => globalThis.aero?.perf?.export?.() ?? { hotspots: [] },
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
            prefetchSequentialChunks: prefetchSequential,
            maxConcurrentFetches: Math.max(1, Number(maxConcurrentFetchesInput.value) | 0),
            cacheBackend: cacheBackendSelect.value === 'auto' ? undefined : (cacheBackendSelect.value as 'opfs' | 'idb'),
          })
        : await client.openRemote(url, {
            blockSize: Number(blockSizeInput.value) * 1024,
            cacheLimitBytes,
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

let perfHud: HTMLElement | null = null;
let perfStarted = false;

function renderPerfPanel(report: PlatformFeatureReport): HTMLElement {
  const supported = report.sharedArrayBuffer && typeof Atomics !== 'undefined';
  const hud = el('pre', { text: supported ? 'Initializing…' : 'Perf telemetry unavailable (SharedArrayBuffer/Atomics missing).' });
  perfHud = hud;
 
  return el(
    'div',
    { class: 'panel' },
    el('h2', { text: 'Perf telemetry' }),
    el(
      'div',
      {
        text:
          'Exports window.aero.perf.export() for automation. ' +
          'Main thread records frame times; a synthetic worker emits instruction counts.',
      },
    ),
    hud,
  );
}

function startPerfTelemetry(report: PlatformFeatureReport): void {
  if (perfStarted) return;
  perfStarted = true;

  if (!perfHud) return;
  if (!report.wasmThreads) {
    perfHud.textContent = 'Perf telemetry unavailable: requires cross-origin isolation + SharedArrayBuffer + Atomics.';
    return;
  }

  const channel = createPerfChannel({
    capacity: 1024,
    workerKinds: [WorkerKind.Main, WorkerKind.CPU],
  });

  const mainWriter = new PerfWriter(channel.buffers[WorkerKind.Main], {
    workerKind: WorkerKind.Main,
    runStartEpochMs: channel.runStartEpochMs,
  });

  const aggregator = new PerfAggregator(channel, { windowSize: 120, captureSize: 2000 });

  const worker = new Worker(new URL('./perf_worker.ts', import.meta.url), { type: 'module' });
  worker.postMessage({ type: 'init', channel, workerKind: WorkerKind.CPU });
  worker.addEventListener('message', (ev: MessageEvent) => {
    const msg = ev.data as { type?: string; hotspots?: unknown } | null;
    if (msg?.type === 'hotspots' && Array.isArray(msg.hotspots)) {
      aggregator.setHotspots(msg.hotspots);
    }
  });

  let enabled = true;
  function setEnabled(next: boolean): void {
    enabled = Boolean(next);
    mainWriter.setEnabled(enabled);
    worker.postMessage({ type: 'setEnabled', enabled });
  }

  const aero = (globalThis.aero ??= {});
  aero.perf = {
    export: () => aggregator.export(),
    getStats: () => aggregator.getStats(),
    setEnabled,
  };

  // Ensure optional perf benchmarks (e.g. WebGPU microbench) are installed and
  // wired into the export payload without clobbering `aero.perf`.
  installAeroGlobal();

  let frameId = 0;
  let lastNow = performance.now();

  function tick(now: number): void {
    const dt = now - lastNow;
    lastNow = now;
    frameId = (frameId + 1) >>> 0;

    const usedHeap = (performance as unknown as { memory?: { usedJSHeapSize?: number } }).memory?.usedJSHeapSize ?? 0;
    mainWriter.frameSample(frameId, {
      durations: { frame_ms: dt },
      counters: { memory_bytes: BigInt(usedHeap) },
    });

    worker.postMessage({ type: 'frame', frameId, dt });

    aggregator.drain();
    const stats = aggregator.getStats();
    perfHud!.textContent =
      `window=${stats.frames}/${stats.windowSize} frames\n` +
      `avg frame=${stats.avgFrameMs.toFixed(2)}ms p95=${stats.p95FrameMs.toFixed(2)}ms\n` +
      `avg fps=${stats.avgFps.toFixed(1)} 1% low=${stats.fps1pLow.toFixed(1)}\n` +
      `avg MIPS=${stats.avgMips.toFixed(1)}\n` +
      `enabled=${enabled}\n`;

    requestAnimationFrame(tick);
  }

  requestAnimationFrame(tick);
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

  startPerfTelemetry(report);
}

render();

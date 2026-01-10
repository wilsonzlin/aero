import './style.css';

import { PerfAggregator, PerfWriter, WorkerKind, createPerfChannel } from '../web/src/perf/index.js';
import { installNetTraceUI, type NetTraceBackend } from '../web/src/net/trace_ui';

import { createAudioOutput } from './platform/audio';
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from './platform/features';
import { importFileToOpfs } from './platform/opfs';
import { requestWebGpuDevice } from './platform/webgpu';
import { VmCoordinator } from './emulator/vmCoordinator.js';

declare global {
  interface Window {
    __aeroUiTicks?: number;
    __aeroVm?: VmCoordinator;
  }
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<Node | string | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) continue;
    if (key === 'class') {
      node.className = String(value);
    } else if (key === 'text') {
      node.textContent = String(value);
    } else if (key.startsWith('on') && typeof value === 'function') {
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
    'webgpu',
    'opfs',
    'audioWorklet',
    'offscreenCanvas',
  ];

  const tbody = el('tbody');
  for (const key of orderedKeys) {
    const val = report[key];
    tbody.append(
      el(
        'tr',
        {},
        el('th', { text: key }),
        el('td', { class: val ? 'ok' : 'bad', text: val ? 'supported' : 'missing' }),
      ),
    );
  }

  return el(
    'table',
    {},
    el('thead', {}, el('tr', {}, el('th', { text: 'feature' }), el('th', { text: 'status' }))),
    tbody,
  );
}

function renderWebGpuPanel(): HTMLElement {
  const output = el('pre', { text: '' });
  const button = el('button', {
    text: 'Request WebGPU device',
    onclick: async () => {
      output.textContent = '';
      try {
        const { adapter, preferredFormat } = await requestWebGpuDevice({ powerPreference: 'high-performance' });
        output.textContent = JSON.stringify(
          {
            adapterInfo: 'requestAdapter succeeded',
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

  return el('div', { class: 'panel' }, el('h2', { text: 'WebGPU' }), el('div', { class: 'row' }, button), output);
}

function renderOpfsPanel(): HTMLElement {
  const status = el('pre', { text: '' });
  const progress = el('progress', { value: '0', max: '1', style: 'width: 320px' }) as HTMLProgressElement;
  const destPathInput = el('input', { type: 'text', value: 'images/disk.img' }) as HTMLInputElement;
  const fileInput = el('input', { type: 'file' }) as HTMLInputElement;

  const importButton = el('button', {
    text: 'Import to OPFS',
    onclick: async () => {
      status.textContent = '';
      progress.value = 0;
      const file = fileInput.files?.[0];
      if (!file) {
        status.textContent = 'Pick a file first.';
        return;
      }
      const destPath = destPathInput.value.trim();
      if (!destPath) {
        status.textContent = 'Destination path must not be empty.';
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

  fileInput.addEventListener('change', () => {
    const file = fileInput.files?.[0];
    if (file) destPathInput.value = `images/${file.name}`;
  });

  return el(
    'div',
    { class: 'panel' },
    el('h2', { text: 'OPFS (disk image import)' }),
    el(
      'div',
      { class: 'row' },
      el('label', { text: 'File:' }),
      fileInput,
      el('label', { text: 'Dest path:' }),
      destPathInput,
      importButton,
      progress,
    ),
    status,
  );
}

function renderAudioPanel(): HTMLElement {
  const status = el('pre', { text: '' });
  const button = el('button', {
    text: 'Init audio output',
    onclick: async () => {
      status.textContent = '';
      const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: 'interactive' });
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      try {
        await output.resume();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }
      status.textContent =
        'Audio initialized. Ring buffer allocated; processor is a stub (will output silence until samples are written).';
    },
  });

  return el('div', { class: 'panel' }, el('h2', { text: 'Audio' }), el('div', { class: 'row' }, button), status);
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

  const errorOut = el('pre', { id: 'vm-error', text: '' });
  const snapshotOut = el('pre', { id: 'vm-snapshot', text: '' });

  const guestRamMiB = el('input', { id: 'vm-guest-mib', type: 'number', value: '64', min: '1', step: '1' }) as HTMLInputElement;
  const maxGuestRamMiB = el('input', { id: 'vm-max-guest-mib', type: 'number', value: '512', min: '1', step: '1' }) as HTMLInputElement;
  const maxDiskCacheMiB = el('input', { id: 'vm-max-disk-cache-mib', type: 'number', value: '128', min: '1', step: '1' }) as HTMLInputElement;
  const maxShaderCacheMiB = el('input', { id: 'vm-max-shader-cache-mib', type: 'number', value: '64', min: '1', step: '1' }) as HTMLInputElement;
  const autoSaveSnapshot = el('input', { id: 'vm-auto-snapshot', type: 'checkbox' }) as HTMLInputElement;

  let vm: VmCoordinator | null = null;
  let visibilityListenerInstalled = false;

  function update(): void {
    const state = vm?.state ?? 'stopped';
    stateLine.textContent = `state=${state}`;
    const lastHeartbeat = vm?.lastHeartbeat as { totalInstructions?: number } | null | undefined;
    const totalInstructions = lastHeartbeat?.totalInstructions ?? 0;
    heartbeatLine.textContent = `lastHeartbeatAt=${vm?.lastHeartbeatAt ?? 0} totalInstructions=${totalInstructions}`;
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
    renderOpfsPanel(),
    renderAudioPanel(),
    renderPerfPanel(report),
    renderEmulatorSafetyPanel(),
    renderNetTracePanel(),
  );

  startPerfTelemetry(report);
}

render();

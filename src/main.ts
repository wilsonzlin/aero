import './style.css';

import { PerfAggregator, PerfWriter, WorkerKind, createPerfChannel } from '../web/src/perf/index.js';

import { createAudioOutput } from './platform/audio';
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from './platform/features';
import { importFileToOpfs } from './platform/opfs';
import { requestWebGpuDevice } from './platform/webgpu';

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

  globalThis.aero = {
    perf: {
      export: () => aggregator.export(),
      getStats: () => aggregator.getStats(),
      setEnabled,
    },
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
  );

  startPerfTelemetry(report);
}

render();

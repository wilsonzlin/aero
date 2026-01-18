import './hud.css';

import type { PerfApi, PerfHudSnapshot } from './types';
import { unrefBestEffort } from '../unrefSafe';

export type PerfHudHandle = {
  show(): void;
  hide(): void;
  toggle(): void;
};

const HUD_UPDATE_HZ = 5;
const SPARKLINE_SAMPLES = 120;

type PerfTraceApi = {
  traceStart(): void;
  traceStop(): void;
  exportTrace(opts?: { asString?: boolean }): Promise<unknown | string>;
  traceEnabled?: boolean;
};

const formatMs = (ms: number | undefined): string => {
  if (ms === undefined || !Number.isFinite(ms)) return '-';
  return `${ms.toFixed(2)} ms`;
};

const formatMs0 = (ms: number | undefined): string => {
  if (ms === undefined || !Number.isFinite(ms)) return '-';
  return `${ms.toFixed(0)} ms`;
};

const formatFps = (fps: number | undefined): string => {
  if (fps === undefined || !Number.isFinite(fps)) return '-';
  return `${fps.toFixed(1)}`;
};

const formatMips = (mips: number | undefined): string => {
  if (mips === undefined || !Number.isFinite(mips)) return '-';
  return `${mips.toFixed(1)}`;
};

const formatBytesPerSec = (bytesPerSec: number | undefined): string => {
  if (bytesPerSec === undefined || !Number.isFinite(bytesPerSec)) return '-';
  const abs = Math.abs(bytesPerSec);
  if (abs < 1024) return `${bytesPerSec.toFixed(0)} B/s`;
  if (abs < 1024 * 1024) return `${(bytesPerSec / 1024).toFixed(1)} KB/s`;
  if (abs < 1024 * 1024 * 1024) return `${(bytesPerSec / (1024 * 1024)).toFixed(1)} MB/s`;
  return `${(bytesPerSec / (1024 * 1024 * 1024)).toFixed(1)} GB/s`;
};

const formatBytes = (bytes: number | undefined): string => {
  if (bytes === undefined || !Number.isFinite(bytes)) return '-';
  const abs = Math.abs(bytes);
  if (abs < 1024) return `${bytes.toFixed(0)} B`;
  if (abs < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (abs < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
};

const isTextInput = (target: EventTarget | null): boolean => {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA') return true;
  return target.isContentEditable;
};

const setText = (el: HTMLElement, next: string): void => {
  if (el.textContent === next) return;
  el.textContent = next;
};

const setupCanvas = (canvas: HTMLCanvasElement, cssWidth: number, cssHeight: number): CanvasRenderingContext2D => {
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(cssWidth * dpr));
  canvas.height = Math.max(1, Math.floor(cssHeight * dpr));
  const ctx = canvas.getContext('2d');
  if (!ctx) {
    throw new Error('Failed to acquire 2D canvas context for perf HUD.');
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return ctx;
};

const drawSparkline = (
  ctx: CanvasRenderingContext2D,
  cssWidth: number,
  cssHeight: number,
  values: Float32Array,
  cursor: number,
  count: number,
  color: string,
): void => {
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  if (count === 0) return;

  let min = Number.POSITIVE_INFINITY;
  let max = Number.NEGATIVE_INFINITY;
  for (let i = 0; i < count; i += 1) {
    const v = values[(cursor + SPARKLINE_SAMPLES - count + i) % SPARKLINE_SAMPLES];
    if (!Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
  }

  if (!Number.isFinite(min) || !Number.isFinite(max)) return;

  const range = max - min;
  const pad = range === 0 ? 1 : range * 0.1;
  const lo = min - pad;
  const hi = max + pad;

  ctx.beginPath();
  let started = false;
  for (let i = 0; i < count; i += 1) {
    const v = values[(cursor + SPARKLINE_SAMPLES - count + i) % SPARKLINE_SAMPLES];
    if (!Number.isFinite(v)) continue;
    const x = (i / Math.max(1, count - 1)) * cssWidth;
    const y = cssHeight - ((v - lo) / (hi - lo)) * cssHeight;
    if (!started) {
      ctx.moveTo(x, y);
      started = true;
    } else {
      ctx.lineTo(x, y);
    }
  }
  if (!started) return;

  ctx.strokeStyle = color;
  ctx.lineWidth = 1.5;
  ctx.stroke();
};

export const installHud = (perf: PerfApi): PerfHudHandle => {
  const traceApi: PerfTraceApi = perf as unknown as PerfTraceApi;

  const devMenu = (() => {
    let existing = document.querySelector<HTMLDivElement>('#aero-dev-menu');
    if (existing) return existing;
    existing = document.createElement('div');
    existing.id = 'aero-dev-menu';
    existing.className = 'aero-dev-menu';
    document.body.append(existing);
    return existing;
  })();

  const devToggleBtn = document.createElement('button');
  devToggleBtn.type = 'button';
  devToggleBtn.textContent = 'Perf HUD';
  devMenu.append(devToggleBtn);

  const hud = document.createElement('div');
  hud.className = 'aero-perf-hud';
  hud.hidden = true;

  const header = document.createElement('div');
  header.className = 'aero-perf-header';

  const title = document.createElement('div');
  title.className = 'aero-perf-title';
  title.textContent = 'Performance';

  const controls = document.createElement('div');
  controls.className = 'aero-perf-controls';

  const captureBtn = document.createElement('button');
  captureBtn.type = 'button';
  captureBtn.textContent = 'Start';

  const resetBtn = document.createElement('button');
  resetBtn.type = 'button';
  resetBtn.textContent = 'Reset';

  const downloadBtn = document.createElement('button');
  downloadBtn.type = 'button';
  downloadBtn.textContent = 'Download';

  const traceToggleBtn = document.createElement('button');
  traceToggleBtn.type = 'button';
  traceToggleBtn.textContent = 'Trace';

  const traceDownloadBtn = document.createElement('button');
  traceDownloadBtn.type = 'button';
  traceDownloadBtn.textContent = 'Trace JSON';

  const hasTraceApi =
    typeof traceApi.traceStart === 'function' &&
    typeof traceApi.traceStop === 'function' &&
    typeof traceApi.exportTrace === 'function';

  if (hasTraceApi) {
    controls.append(traceToggleBtn, traceDownloadBtn);
  }

  controls.append(captureBtn, resetBtn, downloadBtn);
  header.append(title, controls);

  const metrics = document.createElement('div');
  metrics.className = 'aero-perf-metrics';

  const makeRow = (label: string): HTMLElement => {
    const labelEl = document.createElement('div');
    labelEl.className = 'aero-perf-label';
    labelEl.textContent = label;
    const valueEl = document.createElement('div');
    valueEl.className = 'aero-perf-value';
    valueEl.textContent = '-';
    metrics.append(labelEl, valueEl);
    return valueEl;
  };

  const fpsRow = makeRow('FPS (avg / 1% low)');
  const frameTimeRow = makeRow('Frame time (avg / p95)');
  const mipsRow = makeRow('MIPS (avg / p95)');
  const cpuRow = makeRow('CPU (avg)');
  const gpuRow = makeRow('GPU (avg)');
  const ioRow = makeRow('IO (avg)');
  const jitRow = makeRow('JIT (avg)');
  const jitCacheHitRow = makeRow('JIT cache hit');
  const jitBlocksRow = makeRow('JIT blocks compiled');
  const jitCompileRateRow = makeRow('JIT compile rate');
  const drawCallsRow = makeRow('Draw calls (avg/frame)');
  const pipelineSwitchesRow = makeRow('Pipeline switches (avg/frame)');
  const gpuUploadRow = makeRow('GPU upload');
  const gpuTimingRow = makeRow('GPU timing');
  const ioBytesRow = makeRow('IO throughput');
  const hostHeapRow = makeRow('Host heap');
  const guestRamRow = makeRow('Guest RAM');
  const wasmMemoryRow = makeRow('WASM memory');
  const gpuMemoryRow = makeRow('GPU memory (est)');
  const cachesRow = makeRow('Code caches');
  const memoryPeaksRow = makeRow('Memory peaks');
  const captureRow = makeRow('Capture');
  const inputLatencyRow = makeRow('Input latency (p50/p95)');
  const inputQueueRow = makeRow('Input queue');
  const longTasksRow = makeRow('Main thread stalls');

  const sparklines = document.createElement('div');
  sparklines.className = 'aero-perf-sparklines';

  const makeSparklineBox = (label: string): { box: HTMLElement; canvas: HTMLCanvasElement } => {
    const box = document.createElement('div');
    box.className = 'aero-perf-sparkline';
    const titleEl = document.createElement('div');
    titleEl.className = 'aero-perf-sparkline-title';
    titleEl.textContent = label;
    const canvas = document.createElement('canvas');
    box.append(titleEl, canvas);
    return { box, canvas };
  };

  const frameSpark = makeSparklineBox('Frame time');
  const mipsSpark = makeSparklineBox('MIPS');
  const memorySpark = makeSparklineBox('Memory');
  sparklines.append(frameSpark.box, mipsSpark.box, memorySpark.box);

  hud.append(header, metrics, sparklines);
  document.body.append(hud);

  const sparkCssWidth = 160;
  const sparkCssHeight = 34;
  const frameSparkCtx = setupCanvas(frameSpark.canvas, sparkCssWidth, sparkCssHeight);
  const mipsSparkCtx = setupCanvas(mipsSpark.canvas, sparkCssWidth, sparkCssHeight);
  const memorySparkCtx = setupCanvas(memorySpark.canvas, sparkCssWidth, sparkCssHeight);

  const frameSparkValues = new Float32Array(SPARKLINE_SAMPLES);
  const mipsSparkValues = new Float32Array(SPARKLINE_SAMPLES);
  const memorySparkValues = new Float32Array(SPARKLINE_SAMPLES);
  frameSparkValues.fill(Number.NaN);
  mipsSparkValues.fill(Number.NaN);
  memorySparkValues.fill(Number.NaN);

  let sparkCursor = 0;
  let sparkCount = 0;

  const snapshot: PerfHudSnapshot = {
    nowMs: 0,
    capture: {
      active: false,
      durationMs: 0,
      droppedRecords: 0,
      records: 0,
    },
  };

  let captureActive = false;
  let traceActive = false;
  let updateTimer: number | null = null;

  const update = () => {
    perf.getHudSnapshot(snapshot);

    const fps = `${formatFps(snapshot.fpsAvg)} / ${formatFps(snapshot.fps1Low)}`;
    const frameTime = `${formatMs(snapshot.frameTimeAvgMs)} / ${formatMs(snapshot.frameTimeP95Ms)}`;
    const mips = `${formatMips(snapshot.mipsAvg)} / ${formatMips(snapshot.mipsP95)}`;

    setText(fpsRow, fps);
    setText(frameTimeRow, frameTime);
    setText(mipsRow, mips);

    const breakdown = snapshot.breakdownAvgMs;
    setText(cpuRow, formatMs(breakdown?.cpu));
    setText(gpuRow, formatMs(breakdown?.gpu));
    setText(ioRow, formatMs(breakdown?.io));
    setText(jitRow, formatMs(breakdown?.jit));

    const jit = snapshot.jit;
    if (!jit || !jit.enabled || jit.rolling.windowMs === 0) {
      setText(jitCacheHitRow, '-');
      setText(jitBlocksRow, '-');
      setText(jitCompileRateRow, '-');
    } else {
      setText(jitCacheHitRow, `${(jit.rolling.cacheHitRate * 100).toFixed(1)}%`);

      const blocksTotal = jit.totals.tier1.blocksCompiled + jit.totals.tier2.blocksCompiled;
      setText(
        jitBlocksRow,
        `${blocksTotal} (t1 ${jit.totals.tier1.blocksCompiled} · t2 ${jit.totals.tier2.blocksCompiled}) · ${jit.rolling.blocksCompiledPerSec.toFixed(1)}/s`,
      );

      setText(jitCompileRateRow, `${jit.rolling.compileMsPerSec.toFixed(2)} ms/s`);
    }

    setText(drawCallsRow, snapshot.drawCallsPerFrame === undefined ? '-' : snapshot.drawCallsPerFrame.toFixed(1));
    setText(
      pipelineSwitchesRow,
      snapshot.pipelineSwitchesPerFrame === undefined ? '-' : snapshot.pipelineSwitchesPerFrame.toFixed(1),
    );
    setText(gpuUploadRow, formatBytesPerSec(snapshot.gpuUploadBytesPerSec));

    const gpuTimingSupported = snapshot.gpuTimingSupported;
    const gpuTimingEnabled = snapshot.gpuTimingEnabled;
    let gpuTimingText = '-';
    if (gpuTimingSupported === false) gpuTimingText = 'n/a';
    else if (gpuTimingSupported === true) gpuTimingText = gpuTimingEnabled ? 'on' : 'off';
    setText(gpuTimingRow, gpuTimingText);
    setText(ioBytesRow, formatBytesPerSec(snapshot.ioBytesPerSec));

    const heapUsed = formatBytes(snapshot.hostJsHeapUsedBytes);
    const heapTotal = formatBytes(snapshot.hostJsHeapTotalBytes);
    const heapLimit = formatBytes(snapshot.hostJsHeapLimitBytes);
    setText(
      hostHeapRow,
      heapUsed === '-'
        ? '-'
        : heapLimit === '-' || heapLimit === heapTotal
          ? `${heapUsed} / ${heapTotal}`
          : `${heapUsed} / ${heapTotal} (limit ${heapLimit})`,
    );

    setText(guestRamRow, formatBytes(snapshot.guestRamBytes));

    const wasmBytes = formatBytes(snapshot.wasmMemoryBytes);
    const wasmPages = snapshot.wasmMemoryPages === undefined ? '-' : `${Math.floor(snapshot.wasmMemoryPages)} pages`;
    const wasmMaxPages = snapshot.wasmMemoryMaxPages === undefined ? '' : ` / ${snapshot.wasmMemoryMaxPages} max`;
    setText(wasmMemoryRow, wasmBytes === '-' ? '-' : `${wasmBytes} (${wasmPages}${wasmMaxPages})`);

    setText(gpuMemoryRow, formatBytes(snapshot.gpuEstimatedBytes));

    const jitBytes = formatBytes(snapshot.jitCodeCacheBytes);
    const shaderBytes = formatBytes(snapshot.shaderCacheBytes);
    setText(
      cachesRow,
      jitBytes === '-' && shaderBytes === '-' ? '-' : `JIT ${jitBytes} · Shaders ${shaderBytes}`,
    );

    const peakJs = formatBytes(snapshot.peakHostJsHeapUsedBytes);
    const peakWasm = formatBytes(snapshot.peakWasmMemoryBytes);
    const peakGpu = formatBytes(snapshot.peakGpuEstimatedBytes);
    setText(
      memoryPeaksRow,
      peakJs === '-' && peakWasm === '-' && peakGpu === '-' ? '-' : `JS ${peakJs} · WASM ${peakWasm} · GPU ${peakGpu}`,
    );

    const durationSec = snapshot.capture.durationMs / 1000;
    setText(
      captureRow,
      `${snapshot.capture.active ? 'REC' : 'idle'} · ${durationSec.toFixed(1)}s · dropped ${snapshot.capture.droppedRecords} · ${snapshot.capture.records} samples`,
    );

    const resp = snapshot.responsiveness;
    if (!resp) {
      setText(inputLatencyRow, '-');
      setText(inputQueueRow, '-');
      setText(longTasksRow, '-');
    } else {
      const capInj = `${formatMs(resp.capToInjectP50Ms)} / ${formatMs(resp.capToInjectP95Ms)}`;
      const injCons = `${formatMs(resp.injectToConsumeP50Ms)} / ${formatMs(resp.injectToConsumeP95Ms)}`;
      const capPres = `${formatMs(resp.capToPresentP50Ms)} / ${formatMs(resp.capToPresentP95Ms)}`;
      setText(inputLatencyRow, `cap→inj ${capInj} · inj→cpu ${injCons} · cap→prs ${capPres}`);

      const depth = resp.queueDepth === undefined ? '-' : `${resp.queueDepth}`;
      const oldest = formatMs0(resp.queueOldestAgeMs);
      setText(inputQueueRow, `depth ${depth} · oldest ${oldest}`);

      if (resp.longTaskWarning) {
        setText(longTasksRow, resp.longTaskWarning);
      } else if (resp.longTaskCount !== undefined || resp.longTaskMaxMs !== undefined) {
        setText(longTasksRow, `count ${resp.longTaskCount ?? 0} · max ${formatMs0(resp.longTaskMaxMs)} · last ${formatMs0(resp.longTaskLastMs)}`);
      } else {
        setText(longTasksRow, '-');
      }
    }

    captureActive = snapshot.capture.active;
    setText(captureBtn, captureActive ? 'Stop' : 'Start');

    if (hasTraceApi) {
      traceActive = traceApi.traceEnabled === true;
      setText(traceToggleBtn, traceActive ? 'Trace Stop' : 'Trace Start');
    }

    const ftSample = snapshot.lastFrameTimeMs ?? snapshot.frameTimeAvgMs ?? Number.NaN;
    const mipsSample = snapshot.lastMips ?? snapshot.mipsAvg ?? Number.NaN;
    const memSample =
      snapshot.hostJsHeapUsedBytes ?? snapshot.wasmMemoryBytes ?? snapshot.gpuEstimatedBytes ?? Number.NaN;

    frameSparkValues[sparkCursor] = ftSample;
    mipsSparkValues[sparkCursor] = mipsSample;
    memorySparkValues[sparkCursor] = memSample;

    sparkCursor = (sparkCursor + 1) % SPARKLINE_SAMPLES;
    sparkCount = Math.min(SPARKLINE_SAMPLES, sparkCount + 1);

    drawSparkline(frameSparkCtx, sparkCssWidth, sparkCssHeight, frameSparkValues, sparkCursor, sparkCount, '#61dafb');
    drawSparkline(mipsSparkCtx, sparkCssWidth, sparkCssHeight, mipsSparkValues, sparkCursor, sparkCount, '#7CFC90');
    drawSparkline(
      memorySparkCtx,
      sparkCssWidth,
      sparkCssHeight,
      memorySparkValues,
      sparkCursor,
      sparkCount,
      '#ffcc66',
    );
  };

  const startUpdates = () => {
    if (updateTimer !== null) return;
    perf.setHudActive(true);
    update();
    const timer = window.setInterval(update, 1000 / HUD_UPDATE_HZ);
    unrefBestEffort(timer);
    updateTimer = timer as unknown as number;
  };

  const stopUpdates = () => {
    if (updateTimer === null) return;
    window.clearInterval(updateTimer);
    updateTimer = null;
    perf.setHudActive(false);
  };

  const show = () => {
    if (!hud.hidden) return;
    hud.hidden = false;
    startUpdates();
  };

  const hide = () => {
    if (hud.hidden) return;
    hud.hidden = true;
    stopUpdates();
  };

  const toggle = () => {
    if (hud.hidden) show();
    else hide();
  };

  devToggleBtn.addEventListener('click', toggle);

  window.addEventListener('keydown', (ev) => {
    if (ev.repeat) return;
    if (isTextInput(ev.target)) return;
    // If another layer (e.g. VM input capture) already consumed the keystroke, do not
    // trigger HUD hotkeys.
    if (ev.defaultPrevented) return;

    if (ev.key === 'F2' || (ev.ctrlKey && ev.shiftKey && (ev.code === 'KeyP' || ev.key.toLowerCase() === 'p'))) {
      ev.preventDefault();
      toggle();
    }
  });

  captureBtn.addEventListener('click', () => {
    if (captureActive) perf.captureStop();
    else perf.captureStart();
    update();
  });

  resetBtn.addEventListener('click', () => {
    perf.captureReset();
    update();
  });

  downloadBtn.addEventListener('click', () => {
    const data = perf.export();
    const json = JSON.stringify(data);
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `aero-perf-${new Date().toISOString().replace(/[:.]/g, '-')}.json`;
    document.body.append(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  });

  if (hasTraceApi) {
    traceToggleBtn.addEventListener('click', () => {
      if (traceActive) traceApi.traceStop?.();
      else traceApi.traceStart?.();
      update();
    });

    let traceDownloadResetTimer: number | null = null;

    traceDownloadBtn.addEventListener('click', async () => {
      if (traceDownloadBtn.disabled) return;
      if (traceDownloadResetTimer !== null) {
        window.clearTimeout(traceDownloadResetTimer);
        traceDownloadResetTimer = null;
      }

      const idleLabel = 'Trace JSON';
      traceDownloadBtn.disabled = true;
      setText(traceDownloadBtn, 'Exporting…');
      try {
        const data = await traceApi.exportTrace?.({ asString: true });
        if (data === undefined) {
          throw new Error('Trace export API is unavailable.');
        }

        const payload = typeof data === 'string' ? data : JSON.stringify(data);
        const blob = new Blob([payload], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `aero-trace-${new Date().toISOString().replace(/[:.]/g, '-')}.json`;
        document.body.append(a);
        a.click();
        a.remove();
        URL.revokeObjectURL(url);
        setText(traceDownloadBtn, idleLabel);
      } catch (err) {
        console.error(err);
        setText(traceDownloadBtn, 'Trace export failed');
        const resetTimer = window.setTimeout(() => {
          traceDownloadResetTimer = null;
          setText(traceDownloadBtn, idleLabel);
        }, 2500);
        unrefBestEffort(resetTimer);
        traceDownloadResetTimer = resetTimer as unknown as number;
      } finally {
        traceDownloadBtn.disabled = false;
      }
    });
  }

  return { show, hide, toggle };
};

import { createPerfChannel, PerfAggregator, PerfWriter, WorkerKind } from "../src/perf/index.js";

const hud = document.getElementById("hud");

const channel = createPerfChannel({
  capacity: 1024,
  workerKinds: [WorkerKind.Main, WorkerKind.CPU],
});

const mainWriter = new PerfWriter(channel.buffers[WorkerKind.Main], {
  workerKind: WorkerKind.Main,
  runStartEpochMs: channel.runStartEpochMs,
});

const aggregator = new PerfAggregator(channel, { windowSize: 120, captureSize: 2000 });

const worker = new Worker(new URL("./perf_worker.js", import.meta.url), { type: "module" });
worker.postMessage({
  type: "init",
  channel,
  workerKind: WorkerKind.CPU,
});

let enabled = true;

function setEnabled(next) {
  enabled = !!next;
  mainWriter.setEnabled(enabled);
  worker.postMessage({ type: "setEnabled", enabled });
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

function tick(now) {
  const dt = now - lastNow;
  lastNow = now;
  frameId = (frameId + 1) >>> 0;

  const usedHeap = performance.memory?.usedJSHeapSize ?? 0;

  mainWriter.frameSample(frameId, {
    durations: { frame_ms: dt },
    counters: { memory_bytes: BigInt(usedHeap) },
  });

  worker.postMessage({ type: "frame", frameId, dt });

  aggregator.drain();

  const stats = aggregator.getStats();
  hud.textContent =
    `window=${stats.frames}/${stats.windowSize} frames\n` +
    `avg frame=${stats.avgFrameMs.toFixed(2)}ms p95=${stats.p95FrameMs.toFixed(2)}ms\n` +
    `avg fps=${stats.avgFps.toFixed(1)} 1% low=${stats.fps1pLow.toFixed(1)}\n` +
    `avg MIPS=${stats.avgMips.toFixed(1)}\n` +
    `enabled=${enabled}\n` +
    `\n` +
    `Try in console:\n` +
    `  aero.perf.export()\n` +
    `  aero.perf.setEnabled(false)\n`;

  requestAnimationFrame(tick);
}

requestAnimationFrame(tick);


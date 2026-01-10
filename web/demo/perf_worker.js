import { PerfWriter } from "../src/perf/index.js";

let writer = null;
let enabled = true;

self.onmessage = (ev) => {
  const msg = ev.data;
  if (msg.type === "init") {
    writer = new PerfWriter(msg.channel.buffers[msg.workerKind], {
      workerKind: msg.workerKind,
      runStartEpochMs: msg.channel.runStartEpochMs,
    });
    return;
  }

  if (msg.type === "setEnabled") {
    enabled = !!msg.enabled;
    if (writer) writer.setEnabled(enabled);
    return;
  }

  if (msg.type === "frame") {
    if (!writer || !enabled) return;
    const frameId = msg.frameId >>> 0;
    const dt = msg.dt;

    // Synthetic "CPU worker" metrics.
    const cpuMs = dt * 0.6;
    const instructions = BigInt(Math.round(dt * 50_000)); // 50k instructions per ms (~50 MIPS)

    writer.frameSample(frameId, {
      durations: { cpu_ms: cpuMs },
      counters: { instructions },
    });
  }
};


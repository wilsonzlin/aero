/// <reference lib="webworker" />

import { PerfWriter } from '../web/src/perf/writer.js';

type InitMessage = {
  type: 'init';
  channel: { buffers: Record<string, SharedArrayBuffer>; runStartEpochMs: number };
  workerKind: number;
};

type FrameMessage = { type: 'frame'; frameId: number; dt: number };
type SetEnabledMessage = { type: 'setEnabled'; enabled: boolean };
type Message = InitMessage | FrameMessage | SetEnabledMessage;

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let writer: PerfWriter | null = null;
let enabled = true;

ctx.onmessage = (ev: MessageEvent<Message>) => {
  const msg = ev.data;
  if (msg.type === 'init') {
    writer = new PerfWriter(msg.channel.buffers[String(msg.workerKind)] ?? msg.channel.buffers[msg.workerKind], {
      workerKind: msg.workerKind,
      runStartEpochMs: msg.channel.runStartEpochMs,
    });
    writer.setEnabled(enabled);
    return;
  }

  if (msg.type === 'setEnabled') {
    enabled = Boolean(msg.enabled);
    if (writer) writer.setEnabled(enabled);
    return;
  }

  if (msg.type === 'frame') {
    if (!writer || !enabled) return;

    // Synthetic worker-side work for PF-002/003 validation.
    const cpuMs = msg.dt * 0.6;
    const instructions = BigInt(Math.round(msg.dt * 50_000)); // 50k instructions per ms (~50 MIPS)

    writer.frameSample(msg.frameId >>> 0, {
      durations: { cpu_ms: cpuMs },
      counters: { instructions },
    });
  }
};


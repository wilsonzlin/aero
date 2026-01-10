/// <reference lib="webworker" />

import { PerfWriter } from '../web/src/perf/writer.js';
import { HotspotTracker } from './perf/hotspot_tracker.js';

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
let hotspots: HotspotTracker | null = null;
let lastHotspotSendMs = 0;

ctx.onmessage = (ev: MessageEvent<Message>) => {
  const msg = ev.data;
  if (msg.type === 'init') {
    writer = new PerfWriter(msg.channel.buffers[String(msg.workerKind)] ?? msg.channel.buffers[msg.workerKind], {
      workerKind: msg.workerKind,
      runStartEpochMs: msg.channel.runStartEpochMs,
    });
    writer.setEnabled(enabled);
    hotspots = new HotspotTracker({ capacity: 256 });
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
    const instructionsNum = Math.round(msg.dt * 50_000); // 50k instructions per ms (~50 MIPS)
    const instructions = BigInt(instructionsNum);

    writer.frameSample(msg.frameId >>> 0, {
      durations: { cpu_ms: cpuMs },
      counters: { instructions },
    });

    // PF-005: attribute instructions to a couple of synthetic "basic blocks".
    if (hotspots) {
      const hotInstr = Math.round(instructionsNum * 0.9);
      const coldInstr = Math.max(0, instructionsNum - hotInstr);
      hotspots.recordBlock(0x1000, hotInstr);
      hotspots.recordBlock(0x2000, coldInstr);

      const nowMs = typeof performance !== 'undefined' ? performance.now() : Date.now();
      if (nowMs - lastHotspotSendMs >= 500) {
        lastHotspotSendMs = nowMs;
        ctx.postMessage({ type: 'hotspots', hotspots: hotspots.snapshot({ limit: 50 }) });
      }
    }
  }
};

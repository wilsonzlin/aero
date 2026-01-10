import test from "node:test";
import assert from "node:assert/strict";

import { createSpscRingBufferSharedArrayBuffer, SpscRingBuffer } from "../web/src/perf/ring_buffer.js";
import { WorkerKind } from "../web/src/perf/record.js";
import { createPerfChannel } from "../web/src/perf/shared.js";
import { PerfWriter } from "../web/src/perf/writer.js";
import { PerfAggregator } from "../web/src/perf/aggregator.js";

test("SpscRingBuffer basic write/read", () => {
  const sab = createSpscRingBufferSharedArrayBuffer({ capacity: 4, recordSize: 4 });
  const rb = new SpscRingBuffer(sab);

  for (let i = 1; i <= 4; i++) {
    const ok = rb.tryWriteRecord((view, offset) => {
      view.setUint32(offset, i, true);
    });
    assert.equal(ok, true);
  }

  const droppedBefore = rb.getDroppedCount();
  const overflowOk = rb.tryWriteRecord((view, offset) => view.setUint32(offset, 5, true));
  assert.equal(overflowOk, false);
  assert.equal(rb.getDroppedCount(), droppedBefore + 1);

  for (let i = 1; i <= 4; i++) {
    const v = rb.tryReadRecord((view, offset) => view.getUint32(offset, true));
    assert.equal(v, i);
  }
  assert.equal(rb.tryReadRecord((view, offset) => view.getUint32(offset, true)), null);
});

test("SpscRingBuffer wraparound preserves ordering", () => {
  const sab = createSpscRingBufferSharedArrayBuffer({ capacity: 3, recordSize: 4 });
  const rb = new SpscRingBuffer(sab);

  const write = (value) =>
    rb.tryWriteRecord((view, offset) => {
      view.setUint32(offset, value, true);
    });
  const read = () => rb.tryReadRecord((view, offset) => view.getUint32(offset, true));

  assert.equal(write(1), true);
  assert.equal(write(2), true);
  assert.equal(write(3), true);

  assert.equal(read(), 1);
  assert.equal(read(), 2);

  assert.equal(write(4), true);
  assert.equal(write(5), true);

  assert.equal(read(), 3);
  assert.equal(read(), 4);
  assert.equal(read(), 5);
  assert.equal(read(), null);
});

test("PerfAggregator merges per-frame samples and exports JSON without bigint", () => {
  const channel = createPerfChannel({ capacity: 16, workerKinds: [WorkerKind.Main, WorkerKind.CPU, WorkerKind.GPU] });

  const mainWriter = new PerfWriter(channel.buffers[WorkerKind.Main], {
    workerKind: WorkerKind.Main,
    runStartEpochMs: channel.runStartEpochMs,
  });

  const cpuWriter = new PerfWriter(channel.buffers[WorkerKind.CPU], {
    workerKind: WorkerKind.CPU,
    runStartEpochMs: channel.runStartEpochMs,
  });

  const gpuWriter = new PerfWriter(channel.buffers[WorkerKind.GPU], {
    workerKind: WorkerKind.GPU,
    runStartEpochMs: channel.runStartEpochMs,
  });

  const aggregator = new PerfAggregator(channel, { windowSize: 10, captureSize: 20 });

  mainWriter.frameSample(1, { durations: { frame_ms: 16 }, counters: { memory_bytes: 123n } });
  cpuWriter.frameSample(1, { durations: { cpu_ms: 10 }, counters: { instructions: 9_000_000n } });
  gpuWriter.graphicsSample(1, {
    counters: { render_passes: 2, pipeline_switches: 3, bind_group_changes: 4, upload_bytes: 1024n },
    durations: { cpu_translate_ms: 1.5, cpu_encode_ms: 0.5, gpu_time_ms: 2 },
    gpu_timing: { supported: true, enabled: true },
  });

  aggregator.drain();

  const stats = aggregator.getStats();
  assert.equal(stats.frames, 1);
  assert.ok(stats.avgFrameMs > 0);
  assert.ok(stats.avgFps > 0);

  const exported = aggregator.export();
  const json = JSON.stringify(exported);
  assert.ok(json.includes("\"schema_version\":1"));

  const frame0 = exported.samples.frames[0];
  assert.equal(frame0.frame_id, 1);
  assert.equal(frame0.counters.instructions, "9000000");
  assert.equal(frame0.counters.memory_bytes, "123");
  assert.equal(frame0.graphics.render_passes, 2);
  assert.equal(frame0.graphics.pipeline_switches, 3);
  assert.equal(frame0.graphics.bind_group_changes, 4);
  assert.equal(frame0.graphics.upload_bytes, "1024");
  assert.equal(frame0.graphics.cpu_translate_ms, 1.5);
  assert.equal(frame0.graphics.cpu_encode_ms, 0.5);
  assert.equal(frame0.graphics.gpu_time_ms, 2);
  assert.equal(frame0.graphics.gpu_timing.supported, true);
  assert.equal(frame0.graphics.gpu_timing.enabled, true);
});

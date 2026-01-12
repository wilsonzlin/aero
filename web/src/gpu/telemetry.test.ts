import { describe, expect, it } from "vitest";

import { GpuTelemetry } from "./telemetry";

describe("FixedHistogram", () => {
  it("tracks underflow/overflow, computes stats, and returns bucket-midpoint percentiles", () => {
    const telemetry = new GpuTelemetry({
      // Use small buckets so the percentile midpoints are easy to reason about.
      frameTimeHistogram: { bucketSize: 1, min: 0, max: 10 },
    });

    const hist = telemetry.frameTimeMs;

    // Mix in-range values, underflow, and overflow.
    for (const value of [-1, 0, 0, 1, 1, 9, 10, 20]) {
      hist.add(value);
    }

    const snap = hist.snapshot();

    expect(snap.underflow).toBe(1);
    expect(snap.overflow).toBe(2);
    expect(snap.stats.count).toBe(8);

    // Stats reflect *seen* values, including under/overflow.
    expect(snap.stats.min).toBe(-1);
    expect(snap.stats.max).toBe(20);
    expect(snap.stats.mean).toBe(5);

    // Percentiles are approximated using bucket midpoints; overflow is treated as `max`.
    expect(snap.stats.p50).toBe(1.5);
    expect(snap.stats.p95).toBe(10);
    expect(snap.stats.p99).toBe(10);

    // Buckets should contain only in-range values.
    expect(snap.buckets[0]).toBe(2);
    expect(snap.buckets[1]).toBe(2);
    expect(snap.buckets[9]).toBe(1);
  });
});

describe("GpuTelemetry", () => {
  it("records per-frame timing and estimates dropped frames from endFrame cadence", () => {
    const telemetry = new GpuTelemetry({ frameBudgetMs: 10 });

    telemetry.beginFrame(0);
    telemetry.recordTextureUploadBytes(1024);
    telemetry.endFrame(5);

    telemetry.beginFrame(15);
    telemetry.endFrame(20);

    const snap = telemetry.snapshot();
    expect(snap.frameTimeMs.stats.count).toBe(2);

    // interval = 20 - 5 = 15ms; missed = max(0, round(15/10)-1) = 1
    expect(snap.droppedFrames).toBe(1);
  });

  it("includes pipeline cache stats + computes average upload bandwidth in snapshots", () => {
    const telemetry = new GpuTelemetry({
      textureUploadBytesPerFrameHistogram: { bucketSize: 1, min: 0, max: 1000 },
    });

    telemetry.recordPipelineCacheHit();
    telemetry.recordPipelineCacheHit();
    telemetry.recordPipelineCacheMiss();
    telemetry.setPipelineCacheStats({ entries: 7, sizeBytes: 1234 });

    telemetry.beginFrame(0);
    telemetry.recordTextureUploadBytes(100);
    telemetry.endFrame(100);

    telemetry.beginFrame(900);
    telemetry.recordTextureUploadBytes(300);
    telemetry.endFrame(1000);

    const snap = telemetry.snapshot();

    expect(snap.pipelineCache.hits).toBe(2);
    expect(snap.pipelineCache.misses).toBe(1);
    expect(snap.pipelineCache.hitRate).toBeCloseTo(2 / 3);
    expect(snap.pipelineCache.entries).toBe(7);
    expect(snap.pipelineCache.sizeBytes).toBe(1234);

    expect(snap.textureUpload.bytesTotal).toBe(400);
    expect(snap.textureUpload.bytesPerFrame.stats.count).toBe(2);
    expect(snap.textureUpload.bytesPerFrame.stats.mean).toBe(200);

    // wallTimeTotalMs = lastFrameEndMs - firstFrameStartMs = 1000ms => bandwidth == bytesTotal.
    expect(snap.wallTimeTotalMs).toBe(1000);
    expect(snap.textureUpload.bandwidthBytesPerSecAvg).toBe(400);

    // Telemetry snapshots must be structured-cloneable for postMessage/serialization.
    expect(structuredClone(snap)).toEqual(snap);
  });
});


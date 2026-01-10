import { FixedRingBuffer } from './fixed-ring-buffer.js';
import { LogHistogram } from './log-histogram.js';
import { RunningStats } from './running-stats.js';
import { frameTimeMsToFps, msToUs, usToMs } from './units.js';

export class FrameTimeStats {
  #frameTimes;
  #histogram;
  #recent;

  constructor({
    keepLastNSamples = 0,
    histogramSubBucketCount = 1024,
    histogramMaxExponent = 30,
  } = {}) {
    this.#frameTimes = new RunningStats();
    this.#histogram = new LogHistogram({
      subBucketCount: histogramSubBucketCount,
      maxExponent: histogramMaxExponent,
    });
    this.#recent = new FixedRingBuffer(keepLastNSamples);
  }

  get frames() {
    return this.#frameTimes.count;
  }

  clear() {
    this.#frameTimes.clear();
    this.#histogram.clear();
    this.#recent.clear();
  }

  pushFrameTimeMs(frameTimeMs) {
    this.#frameTimes.push(frameTimeMs);
    this.#histogram.record(msToUs(frameTimeMs));
    this.#recent.push(frameTimeMs);
  }

  merge(other) {
    if (!(other instanceof FrameTimeStats)) {
      throw new TypeError('FrameTimeStats.merge: other must be a FrameTimeStats');
    }
    this.#frameTimes.merge(other.#frameTimes);
    this.#histogram.merge(other.#histogram);

    for (const v of other.#recent.toArray()) {
      this.#recent.push(v);
    }
  }

  getRecentFrameTimesMs() {
    return this.#recent.toArray();
  }

  summary() {
    const frames = this.#frameTimes.count;
    if (frames === 0) {
      return {
        frames: 0,
        totalTimeMs: 0,
        meanFrameTimeMs: Number.NaN,
        minFrameTimeMs: Number.NaN,
        maxFrameTimeMs: Number.NaN,
        varianceFrameTimeMs2: Number.NaN,
        stdevFrameTimeMs: Number.NaN,
        covFrameTime: Number.NaN,
        frameTimeP50Ms: Number.NaN,
        frameTimeP95Ms: Number.NaN,
        frameTimeP99Ms: Number.NaN,
        frameTimeP999Ms: Number.NaN,
        fpsAvg: Number.NaN,
        fpsMedian: Number.NaN,
        fpsP95: Number.NaN,
        fps1Low: Number.NaN,
        fps0_1Low: Number.NaN,
      };
    }

    const totalTimeMs = this.#frameTimes.sum;
    const meanFrameTimeMs = this.#frameTimes.mean;
    const varianceFrameTimeMs2 = this.#frameTimes.variancePopulation;
    const stdevFrameTimeMs = this.#frameTimes.stdevPopulation;
    const covFrameTime = this.#frameTimes.coefficientOfVariation;

    const p50Ms = usToMs(this.#histogram.quantile(0.5));
    const p95Ms = usToMs(this.#histogram.quantile(0.95));
    const p99Ms = usToMs(this.#histogram.quantile(0.99));
    const p999Ms = usToMs(this.#histogram.quantile(0.999));

    const fpsAvg = totalTimeMs > 0 ? (frames * 1000) / totalTimeMs : Number.NaN;

    return {
      frames,
      totalTimeMs,
      meanFrameTimeMs,
      minFrameTimeMs: this.#frameTimes.min,
      maxFrameTimeMs: this.#frameTimes.max,
      varianceFrameTimeMs2,
      stdevFrameTimeMs,
      covFrameTime,
      frameTimeP50Ms: p50Ms,
      frameTimeP95Ms: p95Ms,
      frameTimeP99Ms: p99Ms,
      frameTimeP999Ms: p999Ms,
      fpsAvg,
      fpsMedian: frameTimeMsToFps(p50Ms),
      fpsP95: frameTimeMsToFps(p95Ms),
      fps1Low: frameTimeMsToFps(p99Ms),
      fps0_1Low: frameTimeMsToFps(p999Ms),
    };
  }

  toJSON() {
    return {
      type: 'FrameTimeStats',
      frameTimes: this.#frameTimes.toJSON(),
      histogram: this.#histogram.toJSON(),
      recent: this.#recent.toArray(),
      recentCapacity: this.#recent.capacity,
    };
  }

  static fromJSON(data) {
    if (!data || data.type !== 'FrameTimeStats') {
      throw new TypeError('FrameTimeStats.fromJSON: invalid payload');
    }

    const stats = new FrameTimeStats({ keepLastNSamples: data.recentCapacity });
    stats.#frameTimes = RunningStats.fromJSON(data.frameTimes);
    stats.#histogram = LogHistogram.fromJSON(data.histogram);

    for (const v of data.recent) stats.#recent.push(v);
    return stats;
  }
}

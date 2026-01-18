import { LogHistogram, msToUs, usToMs } from './stats';
import { unrefBestEffort } from '../unrefSafe';

export type ResponsivenessHudSnapshot = {
  capToInjectP50Ms?: number;
  capToInjectP95Ms?: number;
  injectToConsumeP50Ms?: number;
  injectToConsumeP95Ms?: number;
  capToPresentP50Ms?: number;
  capToPresentP95Ms?: number;
  queueDepth?: number;
  queueOldestAgeMs?: number;
  longTaskCount?: number;
  longTaskMaxMs?: number;
  longTaskLastMs?: number;
  longTaskWarning?: string;
  eventLoopLagP95Ms?: number;
  rafDeltaP95Ms?: number;
};

export type ResponsivenessExport = {
  capabilities: {
    performance_observer: boolean;
    longtask: boolean;
    event_timing: boolean;
    consume_timestamps: boolean;
  };
  input_latency: {
    capture_to_inject_ms: { p50: number | null; p95: number | null };
    inject_to_consume_ms: { p50: number | null; p95: number | null } | null;
    capture_to_present_ms: { p50: number | null; p95: number | null };
    queue: { depth: number | null; oldest_event_age_ms: number | null };
  };
  event_loop_lag_ms: { p50: number | null; p95: number | null };
  raf_delta_ms: { p50: number | null; p95: number | null };
  long_tasks:
    | {
        count: number;
        total_duration_ms: number;
        max_duration_ms: number;
        last_duration_ms: number | null;
        last_end_ms: number | null;
      }
    | null;
  event_timing_ms: { p50: number | null; p95: number | null } | null;
};

type InputEventTiming = {
  tCaptureMs: number;
  tInjectedMs?: number;
  tConsumedMs?: number;
  presented: boolean;
};

const qMs = (hist: LogHistogram, q: number): number | undefined => {
  const us = hist.quantile(q);
  if (!Number.isFinite(us)) return undefined;
  const ms = usToMs(us);
  return Number.isFinite(ms) ? ms : undefined;
};

const maxMs = (hist: LogHistogram): number | undefined => {
  const us = hist.max;
  if (!Number.isFinite(us)) return undefined;
  const ms = usToMs(us);
  return Number.isFinite(ms) ? ms : undefined;
};

const toNullable = (value: number | undefined): number | null => (value === undefined ? null : value);

const recordMs = (hist: LogHistogram, valueMs: number) => {
  if (!Number.isFinite(valueMs)) return;
  if (valueMs < 0) return;
  const us = msToUs(valueMs);
  if (!Number.isFinite(us)) return;
  const quantized = Math.max(1, Math.round(us));
  hist.record(quantized);
};

export class ResponsivenessTracker {
  private active = false;

  private capToInject = new LogHistogram();
  private injectToConsume = new LogHistogram();
  private capToPresent = new LogHistogram();
  private rafDelta = new LogHistogram();
  private eventLoopLag = new LogHistogram();
  private eventTiming = new LogHistogram();

  private lastPresentNowMs: number | undefined;

  private inputEvents = new Map<number, InputEventTiming>();
  private pendingPresent: number[] = [];
  private pendingPresentSet = new Set<number>();

  private queueDepth: number | undefined;
  private queueOldestCaptureMs: number | null | undefined;

  private hasConsumeTimestamps = false;

  private longTaskObserver: PerformanceObserver | undefined;
  private eventTimingObserver: PerformanceObserver | undefined;
  private eventLoopLagTimer: number | undefined;
  private eventLoopLagIntervalMs = 50;
  private expectedEventLoopTickMs: number | undefined;

  private longTaskCount = 0;
  private longTaskTotalDurationMs = 0;
  private longTaskMaxDurationMs = 0;
  private longTaskLastDurationMs: number | null = null;
  private longTaskLastEndMs: number | null = null;

  private eventTimingDurationThresholdMs = 16;
  private collectEventTiming = false;

  readonly capabilities = {
    performanceObserver: typeof PerformanceObserver !== 'undefined',
    longtask: false,
    eventTiming: false,
  };

  setActive(active: boolean) {
    if (this.active === active) return;
    this.active = active;
    if (active) {
      this.startLongTasks();
      this.startEventLoopLag();
      if (this.collectEventTiming) this.startEventTiming();
    } else {
      this.stopLongTasks();
      this.stopEventLoopLag();
      this.stopEventTiming();
    }
  }

  setEventLoopLagIntervalMs(intervalMs: number) {
    if (!Number.isFinite(intervalMs) || intervalMs <= 0) return;
    this.eventLoopLagIntervalMs = intervalMs;
    if (this.active) {
      this.stopEventLoopLag();
      this.startEventLoopLag();
    }
  }

  setCollectEventTiming(enabled: boolean, { durationThresholdMs }: { durationThresholdMs?: number } = {}) {
    this.collectEventTiming = enabled;
    if (typeof durationThresholdMs === 'number' && Number.isFinite(durationThresholdMs) && durationThresholdMs >= 0) {
      this.eventTimingDurationThresholdMs = durationThresholdMs;
    }
    if (!this.active) return;
    if (enabled) this.startEventTiming();
    else this.stopEventTiming();
  }

  reset(): void {
    this.capToInject = new LogHistogram();
    this.injectToConsume = new LogHistogram();
    this.capToPresent = new LogHistogram();
    this.rafDelta = new LogHistogram();
    this.eventLoopLag = new LogHistogram();
    this.eventTiming = new LogHistogram();

    this.lastPresentNowMs = undefined;
    this.inputEvents.clear();
    this.pendingPresent = [];
    this.pendingPresentSet.clear();
    this.queueDepth = undefined;
    this.queueOldestCaptureMs = undefined;
    this.hasConsumeTimestamps = false;

    this.longTaskCount = 0;
    this.longTaskTotalDurationMs = 0;
    this.longTaskMaxDurationMs = 0;
    this.longTaskLastDurationMs = null;
    this.longTaskLastEndMs = null;
  }

  noteInputCaptured(id: number, tCaptureMs = performance.now()): void {
    if (!this.active) return;
    this.inputEvents.set(id, { tCaptureMs, presented: false });
  }

  noteInputInjected(
    id: number,
    tInjectedMs = performance.now(),
    queueDepth?: number,
    queueOldestCaptureMs?: number | null,
  ): void {
    if (!this.active) return;
    const rec = this.inputEvents.get(id) ?? { tCaptureMs: tInjectedMs, presented: false };
    rec.tInjectedMs = tInjectedMs;
    this.inputEvents.set(id, rec);
    recordMs(this.capToInject, tInjectedMs - rec.tCaptureMs);

    if (queueDepth !== undefined) this.queueDepth = queueDepth;
    if (queueOldestCaptureMs !== undefined) this.queueOldestCaptureMs = queueOldestCaptureMs;

    if (!this.hasConsumeTimestamps) this.enqueuePresent(id);
  }

  noteInputConsumed(
    id: number,
    tConsumedMs = performance.now(),
    queueDepth?: number,
    queueOldestCaptureMs?: number | null,
  ): void {
    if (!this.active) return;
    this.hasConsumeTimestamps = true;

    const rec = this.inputEvents.get(id);
    if (rec) {
      rec.tConsumedMs = tConsumedMs;
      if (rec.tInjectedMs !== undefined) {
        recordMs(this.injectToConsume, tConsumedMs - rec.tInjectedMs);
      }
      if (rec.presented) this.inputEvents.delete(id);
    }

    if (queueDepth !== undefined) this.queueDepth = queueDepth;
    if (queueOldestCaptureMs !== undefined) this.queueOldestCaptureMs = queueOldestCaptureMs;

    this.enqueuePresent(id);
  }

  notePresent(nowMs = performance.now()): void {
    if (!this.active) return;

    if (this.lastPresentNowMs !== undefined) {
      recordMs(this.rafDelta, nowMs - this.lastPresentNowMs);
    }
    this.lastPresentNowMs = nowMs;

    if (this.pendingPresent.length === 0) return;

    for (const id of this.pendingPresent) {
      const rec = this.inputEvents.get(id);
      if (!rec) continue;
      recordMs(this.capToPresent, nowMs - rec.tCaptureMs);
      rec.presented = true;
      if (!this.hasConsumeTimestamps || rec.tConsumedMs !== undefined) {
        this.inputEvents.delete(id);
      }
    }

    this.pendingPresent = [];
    this.pendingPresentSet.clear();
  }

  getHudSnapshot(out: ResponsivenessHudSnapshot): ResponsivenessHudSnapshot {
    out.capToInjectP50Ms = qMs(this.capToInject, 0.5);
    out.capToInjectP95Ms = qMs(this.capToInject, 0.95);

    if (this.hasConsumeTimestamps) {
      out.injectToConsumeP50Ms = qMs(this.injectToConsume, 0.5);
      out.injectToConsumeP95Ms = qMs(this.injectToConsume, 0.95);
    } else {
      out.injectToConsumeP50Ms = undefined;
      out.injectToConsumeP95Ms = undefined;
    }

    out.capToPresentP50Ms = qMs(this.capToPresent, 0.5);
    out.capToPresentP95Ms = qMs(this.capToPresent, 0.95);

    out.queueDepth = this.queueDepth;
    if (this.queueOldestCaptureMs === null) {
      out.queueOldestAgeMs = undefined;
    } else if (this.queueOldestCaptureMs === undefined) {
      out.queueOldestAgeMs = undefined;
    } else {
      const age = performance.now() - this.queueOldestCaptureMs;
      out.queueOldestAgeMs = age >= 0 && Number.isFinite(age) ? age : undefined;
    }

    out.longTaskCount = this.capabilities.longtask ? this.longTaskCount : undefined;
    out.longTaskMaxMs = this.capabilities.longtask ? this.longTaskMaxDurationMs : undefined;
    out.longTaskLastMs = this.capabilities.longtask ? this.longTaskLastDurationMs ?? undefined : undefined;

    const warning = this.buildLongTaskWarning();
    out.longTaskWarning = warning ?? undefined;

    out.eventLoopLagP95Ms = qMs(this.eventLoopLag, 0.95);
    out.rafDeltaP95Ms = qMs(this.rafDelta, 0.95);

    return out;
  }

  export(): ResponsivenessExport {
    const capToInjectP50 = qMs(this.capToInject, 0.5);
    const capToInjectP95 = qMs(this.capToInject, 0.95);
    const injToConsumeP50 = this.hasConsumeTimestamps ? qMs(this.injectToConsume, 0.5) : undefined;
    const injToConsumeP95 = this.hasConsumeTimestamps ? qMs(this.injectToConsume, 0.95) : undefined;
    const capToPresentP50 = qMs(this.capToPresent, 0.5);
    const capToPresentP95 = qMs(this.capToPresent, 0.95);

    const nowMs = performance.now();
    const oldestAgeMs =
      this.queueOldestCaptureMs === undefined || this.queueOldestCaptureMs === null
        ? null
        : Math.max(0, nowMs - this.queueOldestCaptureMs);

    return {
      capabilities: {
        performance_observer: this.capabilities.performanceObserver,
        longtask: this.capabilities.longtask,
        event_timing: this.capabilities.eventTiming,
        consume_timestamps: this.hasConsumeTimestamps,
      },
      input_latency: {
        capture_to_inject_ms: { p50: toNullable(capToInjectP50), p95: toNullable(capToInjectP95) },
        inject_to_consume_ms: this.hasConsumeTimestamps
          ? { p50: toNullable(injToConsumeP50), p95: toNullable(injToConsumeP95) }
          : null,
        capture_to_present_ms: { p50: toNullable(capToPresentP50), p95: toNullable(capToPresentP95) },
        queue: { depth: this.queueDepth ?? null, oldest_event_age_ms: oldestAgeMs },
      },
      raf_delta_ms: { p50: toNullable(qMs(this.rafDelta, 0.5)), p95: toNullable(qMs(this.rafDelta, 0.95)) },
      event_loop_lag_ms: {
        p50: toNullable(qMs(this.eventLoopLag, 0.5)),
        p95: toNullable(qMs(this.eventLoopLag, 0.95)),
      },
      long_tasks: this.capabilities.longtask
        ? {
            count: this.longTaskCount,
            total_duration_ms: this.longTaskTotalDurationMs,
            max_duration_ms: this.longTaskMaxDurationMs,
            last_duration_ms: this.longTaskLastDurationMs,
            last_end_ms: this.longTaskLastEndMs,
          }
        : null,
      event_timing_ms: this.capabilities.eventTiming
        ? { p50: toNullable(qMs(this.eventTiming, 0.5)), p95: toNullable(qMs(this.eventTiming, 0.95)) }
        : null,
    };
  }

  private enqueuePresent(id: number): void {
    if (this.pendingPresentSet.has(id)) return;
    this.pendingPresentSet.add(id);
    this.pendingPresent.push(id);
  }

  private buildLongTaskWarning(): string | null {
    if (!this.capabilities.longtask) return null;
    if (this.longTaskLastDurationMs === null || this.longTaskLastEndMs === null) return null;
    const age = performance.now() - this.longTaskLastEndMs;
    if (age > 2000) return null;
    if (this.longTaskLastDurationMs < 50) return null;
    return `main thread blocked ${this.longTaskLastDurationMs.toFixed(0)}ms`;
  }

  private startLongTasks(): void {
    if (!this.capabilities.performanceObserver) return;
    if (this.longTaskObserver) return;
    try {
      this.longTaskObserver = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          const dur = entry.duration;
          this.longTaskCount += 1;
          this.longTaskTotalDurationMs += dur;
          this.longTaskMaxDurationMs = Math.max(this.longTaskMaxDurationMs, dur);
          this.longTaskLastDurationMs = dur;
          this.longTaskLastEndMs = entry.startTime + dur;
        }
      });
      this.longTaskObserver.observe({ type: 'longtask', buffered: true });
      this.capabilities.longtask = true;
    } catch {
      this.longTaskObserver = undefined;
      this.capabilities.longtask = false;
    }
  }

  private stopLongTasks(): void {
    this.longTaskObserver?.disconnect();
    this.longTaskObserver = undefined;
  }

  private startEventTiming(): void {
    if (!this.capabilities.performanceObserver) return;
    if (this.eventTimingObserver) return;
    try {
      this.eventTimingObserver = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          recordMs(this.eventTiming, entry.duration);
        }
      });
      const observeOpts: PerformanceObserverInit & { durationThreshold?: number } = {
        type: 'event',
        buffered: true,
        durationThreshold: this.eventTimingDurationThresholdMs,
      };
      this.eventTimingObserver.observe(observeOpts);
      this.capabilities.eventTiming = true;
    } catch {
      this.eventTimingObserver = undefined;
      this.capabilities.eventTiming = false;
    }
  }

  private stopEventTiming(): void {
    this.eventTimingObserver?.disconnect();
    this.eventTimingObserver = undefined;
  }

  private startEventLoopLag(): void {
    if (this.eventLoopLagTimer !== undefined) return;
    const interval = this.eventLoopLagIntervalMs;
    this.expectedEventLoopTickMs = performance.now() + interval;
    this.eventLoopLagTimer = window.setInterval(() => {
      const now = performance.now();
      const expected = this.expectedEventLoopTickMs;
      if (expected === undefined) return;
      const lag = Math.max(0, now - expected);
      recordMs(this.eventLoopLag, lag);
      this.expectedEventLoopTickMs = expected + interval;
    }, interval);
    unrefBestEffort(this.eventLoopLagTimer);
  }

  private stopEventLoopLag(): void {
    if (this.eventLoopLagTimer === undefined) return;
    window.clearInterval(this.eventLoopLagTimer);
    this.eventLoopLagTimer = undefined;
    this.expectedEventLoopTickMs = undefined;
  }
}

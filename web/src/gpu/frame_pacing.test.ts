import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FramePacer, type FramePresenter, type FrameSubmission } from "./frame_pacing";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (err: unknown) => void;
};

function defer<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("gpu/frame_pacing FramePacer", () => {
  const g = globalThis as unknown as {
    requestAnimationFrame?: unknown;
    cancelAnimationFrame?: unknown;
  } & Record<string, unknown>;

  const originalRaf = g.requestAnimationFrame;
  const originalCancel = g.cancelAnimationFrame;

  let nowMs = 0;

  let nextRafId = 1;
  let rafQueue: Array<{ id: number; cb: FrameRequestCallback }> = [];

  function setNow(ms: number): void {
    nowMs = ms;
  }

  function tickRaf(timestamp: number): void {
    const next = rafQueue.shift();
    expect(next).toBeDefined();
    next!.cb(timestamp);
  }

  beforeEach(() => {
    nowMs = 0;
    nextRafId = 1;
    rafQueue = [];

    vi.spyOn(performance, "now").mockImplementation(() => nowMs);

    g.requestAnimationFrame = ((cb: FrameRequestCallback) => {
      const id = nextRafId++;
      rafQueue.push({ id, cb });
      return id;
    }) as unknown as typeof requestAnimationFrame;

    g.cancelAnimationFrame = ((id: number) => {
      rafQueue = rafQueue.filter((item) => item.id !== id);
    }) as unknown as typeof cancelAnimationFrame;
  });

  afterEach(() => {
    // Don't leak scheduled RAF callbacks into other test cases.
    rafQueue = [];

    if (originalRaf === undefined) {
      delete g.requestAnimationFrame;
    } else {
      g.requestAnimationFrame = originalRaf;
    }

    if (originalCancel === undefined) {
      delete g.cancelAnimationFrame;
    } else {
      g.cancelAnimationFrame = originalCancel;
    }

    vi.restoreAllMocks();
  });

  it("drop-oldest (default) drops the oldest pending frame when maxPendingFrames=1", async () => {
    const presented: string[] = [];

    const presenter: FramePresenter<string> = {
      present(frame): FrameSubmission {
        presented.push(frame);
        return { submittedAt: nowMs, workDone: Promise.resolve() };
      },
    };

    const pacer = new FramePacer<string>({
      presenter,
      maxPendingFrames: 1,
      // Make it clear we're not limited by in-flight frames for this test.
      maxFramesInFlight: 2,
    });

    pacer.start();
    expect(rafQueue).toHaveLength(1);

    setNow(1);
    expect(pacer.enqueue("oldest")).toBe(true);

    setNow(2);
    expect(pacer.enqueue("newest")).toBe(true);

    expect(pacer.getTelemetry().framesDropped).toBe(1);

    // Tick the next vsync: should submit the newest frame.
    setNow(16);
    tickRaf(16);
    await Promise.resolve();

    expect(presented).toEqual(["newest"]);

    const snap = pacer.getTelemetry();
    expect(snap.framesEnqueued).toBe(2);
    expect(snap.framesDropped).toBe(1);
    expect(snap.framesPresented).toBe(1);
    expect(snap.framesInFlight).toBe(0);

    pacer.stop();
    expect(rafQueue).toHaveLength(0);
  });

  it("drop-newest drops the newest pending frame when maxPendingFrames=1", async () => {
    const presented: string[] = [];

    const presenter: FramePresenter<string> = {
      present(frame): FrameSubmission {
        presented.push(frame);
        return { submittedAt: nowMs, workDone: Promise.resolve() };
      },
    };

    const pacer = new FramePacer<string>({
      presenter,
      maxPendingFrames: 1,
      dropPolicy: "drop-newest",
      maxFramesInFlight: 2,
    });

    pacer.start();
    expect(rafQueue).toHaveLength(1);

    setNow(1);
    expect(pacer.enqueue("oldest")).toBe(true);

    setNow(2);
    // With drop-newest, the enqueue itself fails.
    expect(pacer.enqueue("newest")).toBe(false);

    expect(pacer.getTelemetry().framesDropped).toBe(1);

    setNow(16);
    tickRaf(16);
    await Promise.resolve();

    expect(presented).toEqual(["oldest"]);

    const snap = pacer.getTelemetry();
    expect(snap.framesEnqueued).toBe(2);
    expect(snap.framesDropped).toBe(1);
    expect(snap.framesPresented).toBe(1);
    expect(snap.framesInFlight).toBe(0);

    pacer.stop();
    expect(rafQueue).toHaveLength(0);
  });

  it("limits frames in flight (maxFramesInFlight=1) until workDone resolves + updates telemetry latencies", async () => {
    const presented: string[] = [];
    const workDone = defer<void>();
    const workDone2 = defer<void>();
    let presentCalls = 0;

    const presenter: FramePresenter<string> = {
      present(frame, timestamp): FrameSubmission {
        presented.push(frame);
        presentCalls += 1;
        return presentCalls === 1
          ? { submittedAt: timestamp, workDone: workDone.promise }
          : { submittedAt: timestamp, workDone: workDone2.promise };
      },
    };

    const pacer = new FramePacer<string>({
      presenter,
      maxFramesInFlight: 1,
      maxPendingFrames: 2,
    });

    pacer.start();
    expect(rafQueue).toHaveLength(1);

    setNow(0);
    pacer.enqueue("frame-1");
    setNow(5);
    pacer.enqueue("frame-2");

    // Tick 1: submits frame-1 (in flight count -> 1).
    setNow(10);
    tickRaf(10);
    expect(presented).toEqual(["frame-1"]);

    {
      const snap = pacer.getTelemetry();
      expect(snap.framesEnqueued).toBe(2);
      expect(snap.framesDropped).toBe(0);
      expect(snap.framesPresented).toBe(1);
      expect(snap.framesInFlight).toBe(1);
      // enqueueToSubmit for frame-1: 10 - 0 = 10ms.
      expect(snap.averageEnqueueToSubmitLatencyMs).toBe(10);
      expect(snap.maxEnqueueToSubmitLatencyMs).toBe(10);
    }

    // Tick 2: while frame-1 is still in flight, frame-2 should *not* be submitted.
    setNow(20);
    tickRaf(20);
    expect(presented).toEqual(["frame-1"]);
    expect(pacer.getTelemetry().framesInFlight).toBe(1);

    // Resolve the first submission and observe workDone latency.
    setNow(30);
    workDone.resolve();
    await Promise.resolve();

    {
      const snap = pacer.getTelemetry();
      // workDone latency for frame-1: 30 - 10 = 20ms.
      expect(snap.framesInFlight).toBe(0);
      expect(snap.workDoneLatencySamples).toBe(1);
      expect(snap.averageWorkDoneLatencyMs).toBe(20);
      expect(snap.maxWorkDoneLatencyMs).toBe(20);
    }

    // Tick 3: now that frame-1 completed, frame-2 can be submitted.
    setNow(40);
    tickRaf(40);
    expect(presented).toEqual(["frame-1", "frame-2"]);

    setNow(60);
    workDone2.resolve();
    await Promise.resolve();

    const finalSnap = pacer.getTelemetry();
    expect(finalSnap.framesPresented).toBe(2);
    expect(finalSnap.framesInFlight).toBe(0);
    expect(finalSnap.maxFramesInFlightObserved).toBe(1);

    pacer.stop();
    expect(rafQueue).toHaveLength(0);
  });
});


export interface FrameSubmission {
  submittedAt: DOMHighResTimeStamp;
  workDone: Promise<void>;
}

export interface FramePresenter<TFrame> {
  onAnimationFrame?(timestamp: DOMHighResTimeStamp, framesInFlight: number): void;
  present(frame: TFrame, timestamp: DOMHighResTimeStamp): FrameSubmission;
}

export type FrameDropPolicy = "drop-oldest" | "drop-newest";

export interface FramePacerOptions<TFrame> {
  presenter: FramePresenter<TFrame>;
  maxFramesInFlight?: number;
  maxPendingFrames?: number;
  dropPolicy?: FrameDropPolicy;
  produceFrameOnVsync?: (timestamp: DOMHighResTimeStamp) => TFrame | null;
  onVsync?: (timestamp: DOMHighResTimeStamp) => void;
}

export interface FramePacingTelemetry {
  framesPresented: number;
  framesDropped: number;
  framesEnqueued: number;
  framesInFlight: number;
  maxFramesInFlightObserved: number;
  averageFramesInFlight: number;
  averageEnqueueToSubmitLatencyMs: number;
  maxEnqueueToSubmitLatencyMs: number;
  averageWorkDoneLatencyMs: number;
  maxWorkDoneLatencyMs: number;
  workDoneLatencySamples: number;
}

interface QueuedFrame<TFrame> {
  frame: TFrame;
  enqueuedAt: DOMHighResTimeStamp;
}

export class FramePacer<TFrame> {
  private readonly presenter: FramePresenter<TFrame>;
  private readonly maxFramesInFlight: number;
  private readonly maxPendingFrames: number;
  private readonly dropPolicy: FrameDropPolicy;
  private readonly produceFrameOnVsync?: (timestamp: DOMHighResTimeStamp) => TFrame | null;
  private readonly onVsync?: (timestamp: DOMHighResTimeStamp) => void;

  private running = false;
  private rafHandle: number | null = null;

  private pendingFrames: QueuedFrame<TFrame>[] = [];

  private framesPresented = 0;
  private framesDropped = 0;
  private framesEnqueued = 0;

  private framesInFlight = 0;
  private maxFramesInFlightObserved = 0;

  private inFlightSamples = 0;
  private inFlightSum = 0;

  private enqueueToSubmitLatencySamples = 0;
  private enqueueToSubmitLatencySumMs = 0;
  private enqueueToSubmitLatencyMaxMs = 0;

  private workDoneLatencySamples = 0;
  private workDoneLatencySumMs = 0;
  private workDoneLatencyMaxMs = 0;

  constructor(options: FramePacerOptions<TFrame>) {
    this.presenter = options.presenter;
    this.maxFramesInFlight = Math.max(1, options.maxFramesInFlight ?? 2);
    this.maxPendingFrames = Math.max(1, options.maxPendingFrames ?? 1);
    this.dropPolicy = options.dropPolicy ?? "drop-oldest";
    this.produceFrameOnVsync = options.produceFrameOnVsync;
    this.onVsync = options.onVsync;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.rafHandle = requestAnimationFrame(this.onAnimationFrame);
  }

  stop(): void {
    this.running = false;
    if (this.rafHandle !== null) {
      cancelAnimationFrame(this.rafHandle);
      this.rafHandle = null;
    }
  }

  enqueue(frame: TFrame): boolean {
    const now = performance.now();
    const queued: QueuedFrame<TFrame> = { frame, enqueuedAt: now };
    this.framesEnqueued += 1;

    if (this.pendingFrames.length >= this.maxPendingFrames) {
      if (this.dropPolicy === "drop-newest") {
        this.framesDropped += 1;
        return false;
      }
      this.pendingFrames.shift();
      this.framesDropped += 1;
    }

    this.pendingFrames.push(queued);
    return true;
  }

  shouldProduceFrame(): boolean {
    return (
      this.pendingFrames.length < this.maxPendingFrames &&
      this.framesInFlight < this.maxFramesInFlight
    );
  }

  getTelemetry(): FramePacingTelemetry {
    const averageFramesInFlight =
      this.inFlightSamples === 0 ? 0 : this.inFlightSum / this.inFlightSamples;
    const averageEnqueueToSubmitLatencyMs =
      this.enqueueToSubmitLatencySamples === 0
        ? 0
        : this.enqueueToSubmitLatencySumMs / this.enqueueToSubmitLatencySamples;
    const averageWorkDoneLatencyMs =
      this.workDoneLatencySamples === 0
        ? 0
        : this.workDoneLatencySumMs / this.workDoneLatencySamples;

    return {
      framesPresented: this.framesPresented,
      framesDropped: this.framesDropped,
      framesEnqueued: this.framesEnqueued,
      framesInFlight: this.framesInFlight,
      maxFramesInFlightObserved: this.maxFramesInFlightObserved,
      averageFramesInFlight,
      averageEnqueueToSubmitLatencyMs,
      maxEnqueueToSubmitLatencyMs: this.enqueueToSubmitLatencyMaxMs,
      averageWorkDoneLatencyMs,
      maxWorkDoneLatencyMs: this.workDoneLatencyMaxMs,
      workDoneLatencySamples: this.workDoneLatencySamples,
    };
  }

  private readonly onAnimationFrame = (timestamp: DOMHighResTimeStamp) => {
    if (!this.running) return;

    this.onVsync?.(timestamp);
    this.presenter.onAnimationFrame?.(timestamp, this.framesInFlight);

    if (this.produceFrameOnVsync && this.shouldProduceFrame()) {
      const produced = this.produceFrameOnVsync(timestamp);
      if (produced !== null) {
        this.enqueue(produced);
      }
    }

    if (this.framesInFlight < this.maxFramesInFlight && this.pendingFrames.length > 0) {
      const queued = this.pendingFrames.shift();
      if (queued) {
        const now = performance.now();
        const enqueueLatencyMs = now - queued.enqueuedAt;
        this.enqueueToSubmitLatencySamples += 1;
        this.enqueueToSubmitLatencySumMs += enqueueLatencyMs;
        this.enqueueToSubmitLatencyMaxMs = Math.max(
          this.enqueueToSubmitLatencyMaxMs,
          enqueueLatencyMs,
        );

        this.framesInFlight += 1;
        this.maxFramesInFlightObserved = Math.max(
          this.maxFramesInFlightObserved,
          this.framesInFlight,
        );

        try {
          const submission = this.presenter.present(queued.frame, timestamp);
          this.framesPresented += 1;
          submission.workDone.then(
            () => {
              this.onWorkDone(submission.submittedAt);
            },
            () => {
              this.onWorkDone(submission.submittedAt);
            },
          );
        } catch {
          this.framesInFlight = Math.max(0, this.framesInFlight - 1);
        }
      }
    }

    this.inFlightSamples += 1;
    this.inFlightSum += this.framesInFlight;

    if (this.running) {
      this.rafHandle = requestAnimationFrame(this.onAnimationFrame);
    } else {
      this.rafHandle = null;
    }
  };

  private onWorkDone(submittedAt: DOMHighResTimeStamp): void {
    const now = performance.now();
    const latencyMs = now - submittedAt;
    this.workDoneLatencySamples += 1;
    this.workDoneLatencySumMs += latencyMs;
    this.workDoneLatencyMaxMs = Math.max(this.workDoneLatencyMaxMs, latencyMs);

    this.framesInFlight = Math.max(0, this.framesInFlight - 1);
  }
}

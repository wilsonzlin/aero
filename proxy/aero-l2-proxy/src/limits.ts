export class ConnectionCounter {
  private active = 0;
  private readonly maxConnections: number;

  // NOTE: This file is executed directly by Node's `--experimental-strip-types`
  // loader in unit tests. Node's "strip-only" TypeScript support does not handle
  // TS parameter properties, so we declare fields explicitly.
  constructor(maxConnections: number) {
    this.maxConnections = maxConnections;
  }

  getActive(): number {
    return this.active;
  }

  canAccept(): boolean {
    if (this.maxConnections <= 0) return true;
    return this.active < this.maxConnections;
  }

  acquire(): boolean {
    if (!this.canAccept()) return false;
    this.active += 1;
    return true;
  }

  release(): void {
    if (this.active > 0) this.active -= 1;
  }
}

export type SessionQuotaOptions = Readonly<{
  maxBytes: number;
  maxFramesPerSecond: number;
  now?: () => number;
}>;

export type QuotaDecision = { ok: true } | { ok: false; reason: string };

export class SessionQuota {
  private readonly maxBytes: number;
  private readonly maxFramesPerSecond: number;
  private readonly now: () => number;

  private totalBytes = 0;
  private windowStartMs: number;
  private framesInWindow = 0;

  constructor(opts: SessionQuotaOptions) {
    this.maxBytes = opts.maxBytes;
    this.maxFramesPerSecond = opts.maxFramesPerSecond;
    this.now = opts.now ?? Date.now;
    this.windowStartMs = this.now();
  }

  onRxFrame(bytes: number): QuotaDecision {
    return this.onFrame(bytes, true);
  }

  onTxFrame(bytes: number): QuotaDecision {
    return this.onFrame(bytes, false);
  }

  private onFrame(bytes: number, countTowardFps: boolean): QuotaDecision {
    if (!Number.isFinite(bytes) || bytes < 0) {
      return { ok: false, reason: "Invalid frame size" };
    }

    const nowMs = this.now();
    if (nowMs - this.windowStartMs >= 1000) {
      this.windowStartMs = nowMs;
      this.framesInWindow = 0;
    }

    if (countTowardFps && this.maxFramesPerSecond > 0) {
      this.framesInWindow += 1;
      if (this.framesInWindow > this.maxFramesPerSecond) {
        return { ok: false, reason: "Frame rate limit exceeded" };
      }
    }

    this.totalBytes += bytes;
    if (this.maxBytes > 0 && this.totalBytes > this.maxBytes) {
      return { ok: false, reason: "Byte quota exceeded" };
    }

    return { ok: true };
  }
}

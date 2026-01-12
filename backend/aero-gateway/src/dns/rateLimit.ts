type Bucket = {
  tokens: number;
  updatedAtMs: number;
  lastSeenMs: number;
};

export class TokenBucketRateLimiter {
  private readonly qps: number;
  private readonly burst: number;
  private readonly nowMs: () => number;
  private readonly buckets = new Map<string, Bucket>();

  constructor(qps: number, burst: number, nowMs: () => number = Date.now) {
    this.qps = qps;
    this.burst = burst;
    this.nowMs = nowMs;
  }

  allow(key: string): boolean {
    // Allow disabling the limiter via config; consistent with other gateway knobs
    // where 0 means "disabled".
    if (this.qps <= 0) return true;
    if (this.burst <= 0) return true;

    const now = this.nowMs();
    const bucket = this.buckets.get(key);

    if (!bucket) {
      this.buckets.set(key, { tokens: this.burst - 1, updatedAtMs: now, lastSeenMs: now });
      this.maybePrune(now);
      return true;
    }

    const elapsedSeconds = Math.max(0, (now - bucket.updatedAtMs) / 1000);
    bucket.tokens = Math.min(this.burst, bucket.tokens + elapsedSeconds * this.qps);
    bucket.updatedAtMs = now;
    bucket.lastSeenMs = now;

    if (bucket.tokens < 1) return false;
    bucket.tokens -= 1;
    return true;
  }

  bucketCount(): number {
    return this.buckets.size;
  }

  private maybePrune(now: number) {
    if (this.buckets.size <= 10_000) return;

    const pruneBefore = now - 10 * 60 * 1000;
    for (const [key, bucket] of this.buckets) {
      if (bucket.lastSeenMs < pruneBefore) this.buckets.delete(key);
    }
  }
}

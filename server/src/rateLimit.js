export class TokenBucket {
  constructor({ capacity, refillPerSecond }) {
    if (!Number.isFinite(capacity) || capacity <= 0) throw new Error("Invalid capacity");
    if (!Number.isFinite(refillPerSecond) || refillPerSecond <= 0) throw new Error("Invalid refillPerSecond");
    this.capacity = capacity;
    this.refillPerSecond = refillPerSecond;
    this.tokens = capacity;
    this.lastRefillMs = Date.now();
  }

  #refill() {
    const now = Date.now();
    const elapsedMs = now - this.lastRefillMs;
    if (elapsedMs <= 0) return;
    this.lastRefillMs = now;
    this.tokens = Math.min(this.capacity, this.tokens + (elapsedMs / 1000) * this.refillPerSecond);
  }

  tryRemove(count) {
    this.#refill();
    if (count <= this.tokens) {
      this.tokens -= count;
      return true;
    }
    return false;
  }
}


export interface DnsCacheKey {
  name: string;
  type: number;
  class: number;
}

export function makeCacheKey(key: DnsCacheKey): string {
  return `${key.name}|${key.type}|${key.class}`;
}

export interface DnsCacheEntry {
  expiresAtMs: number;
  response: Buffer;
}

export class DnsCache {
  private readonly entries = new Map<string, DnsCacheEntry>();
  private readonly maxEntries: number;
  private readonly maxTtlSeconds: number;
  private readonly nowMs: () => number;

  constructor(maxEntries: number, maxTtlSeconds: number, nowMs: () => number = Date.now) {
    this.maxEntries = maxEntries;
    this.maxTtlSeconds = maxTtlSeconds;
    this.nowMs = nowMs;
  }

  get(key: string): Buffer | null {
    const entry = this.entries.get(key);
    if (!entry) return null;

    const now = this.nowMs();
    if (entry.expiresAtMs <= now) {
      this.entries.delete(key);
      return null;
    }

    // Refresh recency (LRU-ish) by reinserting.
    this.entries.delete(key);
    this.entries.set(key, entry);
    return entry.response;
  }

  set(key: string, response: Buffer, ttlSeconds: number) {
    const boundedTtl = Math.min(this.maxTtlSeconds, Math.max(0, Math.floor(ttlSeconds)));
    if (boundedTtl === 0) return;

    const expiresAtMs = this.nowMs() + boundedTtl * 1000;

    this.entries.delete(key);
    this.entries.set(key, { expiresAtMs, response });

    while (this.entries.size > this.maxEntries) {
      const oldestKey = this.entries.keys().next().value as string | undefined;
      if (!oldestKey) break;
      this.entries.delete(oldestKey);
    }
  }
}

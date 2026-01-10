import { resourceLimitExceeded } from './errors.js';

export class SizedLruCache {
  constructor({ maxBytes, name }) {
    this.name = name;
    this.maxBytes = maxBytes;
    this._bytes = 0;
    this._entries = new Map();
  }

  get bytes() {
    return this._bytes;
  }

  get(key) {
    const entry = this._entries.get(key);
    if (!entry) return undefined;
    this._entries.delete(key);
    this._entries.set(key, entry);
    return entry.value;
  }

  set(key, value, sizeBytes) {
    if (sizeBytes > this.maxBytes) {
      throw resourceLimitExceeded({ resource: this.name, requestedBytes: sizeBytes, maxBytes: this.maxBytes });
    }

    const existing = this._entries.get(key);
    if (existing) {
      this._entries.delete(key);
      this._bytes -= existing.sizeBytes;
    }

    this._entries.set(key, { value, sizeBytes });
    this._bytes += sizeBytes;
    this._evict();
  }

  clear() {
    this._entries.clear();
    this._bytes = 0;
  }

  _evict() {
    while (this._bytes > this.maxBytes) {
      const oldestKey = this._entries.keys().next().value;
      if (oldestKey === undefined) break;
      const oldest = this._entries.get(oldestKey);
      this._entries.delete(oldestKey);
      this._bytes -= oldest.sizeBytes;
    }
  }
}


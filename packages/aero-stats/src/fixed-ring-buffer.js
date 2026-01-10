export class FixedRingBuffer {
  #capacity;
  #buf;
  #next = 0;
  #size = 0;

  constructor(capacity) {
    if (!Number.isInteger(capacity) || capacity < 0) {
      throw new TypeError(
        `FixedRingBuffer: capacity must be a non-negative integer, got ${capacity}`,
      );
    }
    this.#capacity = capacity;
    this.#buf = new Array(capacity);
  }

  get capacity() {
    return this.#capacity;
  }

  get size() {
    return this.#size;
  }

  push(value) {
    if (this.#capacity === 0) return;
    this.#buf[this.#next] = value;
    this.#next = (this.#next + 1) % this.#capacity;
    this.#size = Math.min(this.#size + 1, this.#capacity);
  }

  toArray() {
    if (this.#size === 0) return [];

    const result = new Array(this.#size);
    const start = (this.#next - this.#size + this.#capacity) % this.#capacity;
    for (let i = 0; i < this.#size; i += 1) {
      result[i] = this.#buf[(start + i) % this.#capacity];
    }
    return result;
  }
}


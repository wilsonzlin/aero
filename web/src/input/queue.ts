import type { HostInputEvent } from "./types";

export interface InputQueueSnapshot {
  depth: number;
  oldest_capture_ms: number | null;
}

export interface InputQueue<T extends HostInputEvent = HostInputEvent> {
  push(event: T): boolean;
  shift(): T | undefined;
  snapshot(): InputQueueSnapshot;
}

const DEFAULT_IN_MEMORY_QUEUE_CAPACITY = 4096;

export class InMemoryInputQueue<T extends HostInputEvent = HostInputEvent> implements InputQueue<T> {
  private readonly events: T[] = [];
  readonly capacity: number;

  constructor({ capacity = DEFAULT_IN_MEMORY_QUEUE_CAPACITY }: { capacity?: number } = {}) {
    // Be defensive: capacity may come from user config. `NaN` in particular would make
    // `events.length >= capacity` always false, resulting in unbounded growth.
    if (Number.isFinite(capacity)) {
      const c = Math.floor(capacity);
      this.capacity = c >= 0 ? c : DEFAULT_IN_MEMORY_QUEUE_CAPACITY;
    } else {
      this.capacity = DEFAULT_IN_MEMORY_QUEUE_CAPACITY;
    }
  }

  push(event: T): boolean {
    if (this.events.length >= this.capacity) return false;
    this.events.push(event);
    return true;
  }

  shift(): T | undefined {
    return this.events.shift();
  }

  snapshot(): InputQueueSnapshot {
    return {
      depth: this.events.length,
      oldest_capture_ms: this.events.length > 0 ? this.events[0]!.t_capture_ms : null,
    };
  }
}

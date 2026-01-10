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

export class InMemoryInputQueue<T extends HostInputEvent = HostInputEvent> implements InputQueue<T> {
  private readonly events: T[] = [];
  readonly capacity: number;

  constructor({ capacity = 4096 }: { capacity?: number } = {}) {
    this.capacity = capacity;
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

import type { HostInputEvent } from "./types";
import type { InputQueue, InputQueueSnapshot } from "./queue";

export interface InputConsumerHooks {
  on_consumed?: (args: { id: number; t_consumed_ms: number; queue: InputQueueSnapshot }) => void;
}

export function drainInputQueue(
  queue: InputQueue<HostInputEvent>,
  handler: (event: HostInputEvent) => void,
  hooks: InputConsumerHooks = {},
) {
  while (true) {
    const evt = queue.shift();
    if (!evt) break;
    const t_consumed_ms = performance.now();
    hooks.on_consumed?.({ id: evt.id, t_consumed_ms, queue: queue.snapshot() });
    handler(evt);
  }
}

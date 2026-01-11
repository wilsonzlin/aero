import type { UsbHostCompletion } from "./usb_passthrough_types";
import { UsbProxyRing } from "./usb_proxy_ring";

type CompletionHandler = (completion: UsbHostCompletion) => void;

type DispatcherEntry = {
  ring: UsbProxyRing;
  handlers: Set<CompletionHandler>;
  timer: ReturnType<typeof setInterval> | null;
  drainIntervalMs: number;
};

const DEFAULT_DRAIN_INTERVAL_MS = 4;

// A completion ring is a single-producer (main thread) -> single-consumer (worker thread) queue.
//
// The worker side, however, can have multiple runtimes attached to the same MessagePort (e.g.
// `WebUsbPassthroughRuntime` + `WebUsbUhciHarnessRuntime`). With `postMessage`, completions are
// broadcast to all listeners automatically. With a ring buffer, only one consumer can pop.
//
// This dispatcher provides the same "broadcast" behaviour by ensuring there is exactly one ring
// drain loop per completion ring buffer and then fan-out to all subscribed runtimes.
const completionDispatchers = new WeakMap<SharedArrayBuffer, DispatcherEntry>();

function drain(entry: DispatcherEntry): void {
  const { ring, handlers } = entry;
  if (handlers.size === 0) return;

  // eslint-disable-next-line no-constant-condition
  while (true) {
    let completion: UsbHostCompletion | null = null;
    try {
      completion = ring.popCompletion();
    } catch {
      // Treat ring corruption as a fatal condition for the fast path. Consumers will still
      // receive any postMessage-based fallbacks.
      return;
    }

    if (!completion) break;

    for (const handler of handlers) {
      try {
        handler(completion);
      } catch {
        // Ignore subscriber errors; a single runtime shouldn't prevent other runtimes from
        // receiving completions.
      }
    }
  }
}

function ensureTimer(entry: DispatcherEntry): void {
  if (entry.timer) return;
  entry.timer = setInterval(() => drain(entry), entry.drainIntervalMs);
  (entry.timer as unknown as { unref?: () => void }).unref?.();
}

function maybeStopTimer(entry: DispatcherEntry): void {
  if (!entry.timer) return;
  if (entry.handlers.size !== 0) return;
  clearInterval(entry.timer);
  entry.timer = null;
}

export function subscribeUsbProxyCompletionRing(
  buffer: SharedArrayBuffer,
  handler: CompletionHandler,
  options: { drainIntervalMs?: number } = {},
): () => void {
  const requestedInterval = options.drainIntervalMs ?? DEFAULT_DRAIN_INTERVAL_MS;
  let entry = completionDispatchers.get(buffer);
  if (!entry) {
    entry = {
      ring: new UsbProxyRing(buffer),
      handlers: new Set(),
      timer: null,
      drainIntervalMs: requestedInterval,
    };
    completionDispatchers.set(buffer, entry);
  } else if (requestedInterval < entry.drainIntervalMs) {
    // Prefer the smallest requested interval to keep latency low when multiple runtimes subscribe.
    entry.drainIntervalMs = requestedInterval;
    if (entry.timer) {
      clearInterval(entry.timer);
      entry.timer = null;
    }
  }

  entry.handlers.add(handler);
  ensureTimer(entry);

  // Drain once immediately so subscribers see any completions that were queued before they attached.
  drain(entry);

  let unsubscribed = false;
  return () => {
    if (unsubscribed) return;
    unsubscribed = true;
    entry.handlers.delete(handler);
    maybeStopTimer(entry);
  };
}


import type { UsbHostCompletion } from "./usb_passthrough_types";
import { UsbProxyRing } from "./usb_proxy_ring";
import { unrefBestEffort } from "../unrefSafe";

type CompletionHandler = (completion: UsbHostCompletion) => void;
type ErrorHandler = (err: unknown) => void;

type DispatcherEntry = {
  ring: UsbProxyRing;
  handlers: Map<CompletionHandler, ErrorHandler | undefined>;
  timer: ReturnType<typeof setInterval> | null;
  drainIntervalMs: number;
  broken: boolean;
  lastError: unknown | null;
};

const DEFAULT_DRAIN_INTERVAL_MS = 4;

// Bound per-tick completion-ring drains so a busy or malicious producer can't keep
// the worker spinning in the drain loop (starving unrelated I/O work). When the
// caps are hit we continue draining on the next interval.
const MAX_USB_COMPLETION_RING_RECORDS_PER_DRAIN_TICK = 256;
// Approximate payload byte budget for draining. Large completion payloads (bulkIn /
// controlIn data) require copying out of the SharedArrayBuffer; draining too many
// in one tick can create large transient allocations.
const MAX_USB_COMPLETION_RING_BYTES_PER_DRAIN_TICK = 1024 * 1024;

// When the VM runtime is snapshot-paused, the IO worker must not allow async completion delivery
// (including the SharedArrayBuffer completion-ring fast path) to mutate guest-visible state while
// the snapshot writer is reading guest RAM/device blobs.
//
// The IO worker toggles this flag during `vm.snapshot.pause`/`vm.snapshot.resume`.
let completionDispatchPaused = false;

export function setUsbProxyCompletionRingDispatchPaused(paused: boolean): void {
  completionDispatchPaused = Boolean(paused);
}

// A completion ring is a single-producer (main thread) -> single-consumer (worker thread) queue.
//
// The worker side, however, can have multiple runtimes attached to the same MessagePort (e.g.
// `WebUsbPassthroughRuntime` + `WebUsbUhciHarnessRuntime`). With `postMessage`, completions are
// broadcast to all listeners automatically. With a ring buffer, only one consumer can pop.
//
// This dispatcher provides the same "broadcast" behaviour by ensuring there is exactly one ring
// drain loop per completion ring buffer and then fan-out to all subscribed runtimes.
const completionDispatchers = new WeakMap<SharedArrayBuffer, DispatcherEntry>();

function completionPayloadBytes(completion: UsbHostCompletion): number {
  if (completion.status === "success") {
    if (completion.kind === "controlIn" || completion.kind === "bulkIn") {
      return completion.data.byteLength >>> 0;
    }
    return 0;
  }
  return 0;
}

function drain(entry: DispatcherEntry): void {
  if (completionDispatchPaused) return;
  if (entry.broken) return;
  const { ring, handlers } = entry;
  if (handlers.size === 0) return;

  let remainingRecords = MAX_USB_COMPLETION_RING_RECORDS_PER_DRAIN_TICK;
  let remainingBytes = MAX_USB_COMPLETION_RING_BYTES_PER_DRAIN_TICK;

  while (remainingRecords > 0 && remainingBytes > 0) {
    let completion: UsbHostCompletion | null = null;
    try {
      completion = ring.popCompletion();
    } catch (err) {
      // Treat ring corruption as a fatal condition for the fast path. Consumers must fall back
      // to `postMessage`-based completions.
      entry.broken = true;
      entry.lastError = err;
      if (entry.timer) {
        clearInterval(entry.timer);
        entry.timer = null;
      }
      for (const onError of handlers.values()) {
        if (!onError) continue;
        try {
          onError(err);
        } catch {
          // ignore subscriber errors
        }
      }
      return;
    }

    if (!completion) break;

    for (const handler of handlers.keys()) {
      try {
        handler(completion);
      } catch {
        // Ignore subscriber errors; a single runtime shouldn't prevent other runtimes from
        // receiving completions.
      }
    }

    remainingRecords -= 1;
    remainingBytes -= completionPayloadBytes(completion);
  }
}

function ensureTimer(entry: DispatcherEntry): void {
  if (entry.broken) return;
  if (entry.timer) return;
  entry.timer = setInterval(() => drain(entry), entry.drainIntervalMs);
  unrefBestEffort(entry.timer);
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
  options: { drainIntervalMs?: number; onError?: ErrorHandler } = {},
): () => void {
  const requestedInterval = options.drainIntervalMs ?? DEFAULT_DRAIN_INTERVAL_MS;
  let entry = completionDispatchers.get(buffer);
  if (!entry) {
    entry = {
      ring: new UsbProxyRing(buffer),
      handlers: new Map(),
      timer: null,
      drainIntervalMs: requestedInterval,
      broken: false,
      lastError: null,
    };
    completionDispatchers.set(buffer, entry);
  } else if (!entry.broken && requestedInterval < entry.drainIntervalMs) {
    // Prefer the smallest requested interval to keep latency low when multiple runtimes subscribe.
    entry.drainIntervalMs = requestedInterval;
    if (entry.timer) {
      clearInterval(entry.timer);
      entry.timer = null;
    }
  }

  let unsubscribed = false;
  entry.handlers.set(handler, options.onError);

  if (entry.broken) {
    // If the ring was already marked broken (e.g. another runtime detected corruption),
    // notify new subscribers immediately so they can fall back to postMessage.
    const onError = options.onError;
    const err = entry.lastError ?? new Error("USB completion ring dispatcher is broken.");
    if (onError) {
      queueMicrotask(() => {
        if (unsubscribed) return;
        try {
          onError(err);
        } catch {
          // ignore subscriber errors
        }
      });
    }
  } else {
    ensureTimer(entry);

    // Drain after the current call stack so other runtimes processing the same `usb.ringAttach`
    // event have a chance to subscribe before we consume entries from the single-consumer ring.
    queueMicrotask(() => drain(entry));
  }

  return () => {
    if (unsubscribed) return;
    unsubscribed = true;
    entry.handlers.delete(handler);
    maybeStopTimer(entry);
  };
}

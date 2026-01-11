import {
  isUsbCompletionMessage,
  isUsbHostAction,
  isUsbSelectedMessage,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";

export type UsbPassthroughBridgeLike = {
  drain_actions(): unknown;
  push_completion(completion: UsbHostCompletion): void;
  reset(): void;
  pending_summary?(): unknown;
  free(): void;
};

export type UsbBrokerPortLike = Pick<MessagePort, "addEventListener" | "removeEventListener" | "postMessage"> & {
  start?: () => void;
};

export type WebUsbPassthroughRuntimeMetrics = {
  actionsForwarded: number;
  completionsApplied: number;
  pendingCompletions: number;
  lastError: string | null;
};

function createDeferred<T>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (err: unknown) => void } {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function formatError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

function normalizeActionId(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "bigint") {
    if (value < 0n) return null;
    if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new Error(`USB action id is too large for JS number: ${value.toString()}`);
    }
    return Number(value);
  }
  return null;
}

function normalizeActionKind(value: unknown): UsbHostAction["kind"] | null {
  switch (value) {
    case "controlIn":
    case "controlOut":
    case "bulkIn":
    case "bulkOut":
      return value;
    default:
      return null;
  }
}

type PendingItem = {
  resolve: (completion: UsbHostCompletion) => void;
  reject: (err: unknown) => void;
};

/**
 * Worker-side passthrough runtime that drains USB host actions from WASM
 * (`UsbPassthroughBridge`) and proxies them to the main thread `UsbBroker` via
 * `postMessage`.
 */
export class WebUsbPassthroughRuntime {
  readonly #bridge: UsbPassthroughBridgeLike;
  readonly #port: UsbBrokerPortLike;
  readonly #pollIntervalMs: number;

  /**
   * `usb.selected ok:false` indicates the passthrough device is unavailable
   * (disconnect/revoked/chooser error). In that state we stop pumping and reset
   * the bridge until a subsequent `ok:true` arrives.
   *
   * Keep the default as "not blocked" so the runtime still functions if it is
   * instantiated after an `ok:true` broadcast (e.g. WASM finishes loading after
   * the user selects a device).
   */
  #blocked = false;
  #desiredRunning = false;
  #pollTimer: number | undefined;
  #pollInFlight = false;

  readonly #pending = new Map<number, PendingItem>();

  #actionsForwarded = 0;
  #completionsApplied = 0;
  #lastError: string | null = null;

  readonly #onMessage: EventListener;

  constructor(options: { bridge: UsbPassthroughBridgeLike; port: UsbBrokerPortLike; pollIntervalMs?: number }) {
    this.#bridge = options.bridge;
    this.#port = options.port;
    this.#pollIntervalMs = options.pollIntervalMs ?? 8;

    this.#onMessage = (ev) => {
      const data = (ev as MessageEvent<unknown>).data;

      if (isUsbCompletionMessage(data)) {
        this.handleCompletion(data.completion);
        return;
      }

      if (isUsbSelectedMessage(data)) {
        this.handleSelected(data);
      }
    };

    this.#port.addEventListener("message", this.#onMessage);
    // When using addEventListener() MessagePorts need start() to begin dispatch.
    (this.#port as unknown as { start?: () => void }).start?.();
  }

  start(): void {
    this.#desiredRunning = true;
    this.ensurePolling();
  }

  stop(): void {
    this.#desiredRunning = false;
    this.stopPolling();
    this.cancelPending("WebUSB passthrough stopped.");
  }

  destroy(): void {
    this.stop();
    this.#port.removeEventListener("message", this.#onMessage);
    try {
      this.#bridge.free();
    } catch (err) {
      this.#lastError = formatError(err);
    }
  }

  getMetrics(): WebUsbPassthroughRuntimeMetrics {
    return {
      actionsForwarded: this.#actionsForwarded,
      completionsApplied: this.#completionsApplied,
      pendingCompletions: this.#pending.size,
      lastError: this.#lastError,
    };
  }

  pendingSummary(): unknown {
    try {
      return this.#bridge.pending_summary?.();
    } catch (err) {
      this.#lastError = formatError(err);
      return null;
    }
  }

  async pollOnce(): Promise<void> {
    if (this.#blocked) return;
    if (this.#pollInFlight) return;

    this.#pollInFlight = true;
    try {
      let drained: unknown;
      try {
        drained = this.#bridge.drain_actions();
      } catch (err) {
        this.#lastError = formatError(err);
        return;
      }

      if (!drained) return;
      if (!Array.isArray(drained)) {
        this.#lastError = `UsbPassthroughBridge.drain_actions() returned non-array: ${typeof drained}`;
        return;
      }

      const awaiters: Array<Promise<UsbHostCompletion>> = [];

      for (const raw of drained) {
        const record = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : null;
        let id: number | null = null;
        let kind: UsbHostAction["kind"] | null = null;
        try {
          id = record ? normalizeActionId(record.id) : null;
          kind = record ? normalizeActionKind(record.kind) : null;
        } catch (err) {
          // If WASM handed us an id too large to represent safely, reset the bridge to
          // avoid deadlocking the Rust-side queue on an action we can never complete.
          this.#lastError = formatError(err);
          try {
            this.#bridge.reset();
          } catch (resetErr) {
            this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
          }
          break;
        }

        let candidate: unknown = raw;
        if (record && typeof record.id === "bigint" && id !== null) {
          candidate = { ...record, id };
        }

        if (!isUsbHostAction(candidate)) {
          // Avoid deadlocking the Rust-side queue: send an error completion back if we can find an id.
          if (id !== null && kind !== null) {
            try {
              this.#bridge.push_completion(usbErrorCompletion(kind, id, "Invalid UsbHostAction received from WASM."));
              this.#completionsApplied++;
            } catch (err) {
              this.#lastError = formatError(err);
            }
          } else if (id !== null) {
            this.#lastError = "Invalid UsbHostAction received from WASM (missing kind).";
          } else {
            this.#lastError = "Invalid UsbHostAction received from WASM (missing id/kind).";
          }
          continue;
        }

        const action = candidate;
        const { id: actionId } = action;
        if (this.#pending.has(actionId)) {
          this.#lastError = `Duplicate UsbHostAction id received from WASM: ${actionId}`;
          try {
            this.#bridge.push_completion(
              usbErrorCompletion(action.kind, actionId, `Duplicate UsbHostAction id received from WASM: ${actionId}`),
            );
            this.#completionsApplied++;
          } catch (err) {
            this.#lastError = formatError(err);
          }
          continue;
        }

        const deferred = createDeferred<UsbHostCompletion>();
        this.#pending.set(actionId, { resolve: deferred.resolve, reject: deferred.reject });

        const msg: UsbActionMessage = { type: "usb.action", action };
        try {
          this.#port.postMessage(msg);
        } catch (err) {
          this.#pending.delete(actionId);
          deferred.reject(err);
          this.#lastError = formatError(err);
          continue;
        }

        this.#actionsForwarded++;
        awaiters.push(deferred.promise);
      }

      if (awaiters.length === 0) return;

      try {
        await Promise.all(awaiters);
      } catch (err) {
        // Cancellations/resets reject in-flight actions. Treat as best-effort and
        // remember the most recent error for debugging.
        this.#lastError = formatError(err);
      }
    } finally {
      this.#pollInFlight = false;
    }
  }

  private handleCompletion(completion: UsbHostCompletion): void {
    const pending = this.#pending.get(completion.id);
    if (!pending) return;
    this.#pending.delete(completion.id);

    try {
      this.#bridge.push_completion(completion);
      this.#completionsApplied++;
    } catch (err) {
      this.#lastError = formatError(err);
    } finally {
      pending.resolve(completion);
    }
  }

  private handleSelected(msg: UsbSelectedMessage): void {
    if (msg.ok) {
      this.#blocked = false;
      this.ensurePolling();
      return;
    }

    this.#blocked = true;
    this.stopPolling();
    this.cancelPending(msg.error ?? "WebUSB device disconnected.");

    try {
      this.#bridge.reset();
    } catch (err) {
      this.#lastError = formatError(err);
    }
  }

  private cancelPending(reason: string): void {
    if (this.#pending.size === 0) return;
    const err = new Error(reason);
    for (const item of this.#pending.values()) {
      try {
        item.reject(err);
      } catch {
        // ignore
      }
    }
    this.#pending.clear();
  }

  private ensurePolling(): void {
    if (!this.#desiredRunning) return;
    if (this.#blocked) return;
    if (this.#pollIntervalMs <= 0) return;
    if (this.#pollTimer !== undefined) return;

    this.#pollTimer = setInterval(() => {
      void this.pollOnce();
    }, this.#pollIntervalMs) as unknown as number;
  }

  private stopPolling(): void {
    if (this.#pollTimer === undefined) return;
    clearInterval(this.#pollTimer);
    this.#pollTimer = undefined;
  }
}

import {
  isUsbCompletionMessage,
  isUsbSelectedMessage,
  isUsbSetupPacket,
  getTransferablesForUsbActionMessage,
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

type UsbHostActionKind = UsbHostAction["kind"];

function normalizeActionId(value: unknown): number | null {
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 0) return null;
    return value;
  }
  if (typeof value === "bigint") {
    if (value < 0n) return null;
    if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new Error(`USB action id is too large for JS number: ${value.toString()}`);
    }
    return Number(value);
  }
  return null;
}

function normalizeU8(value: unknown): number | null {
  const asNum = typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : null;
  if (asNum === null) return null;
  if (!Number.isFinite(asNum) || !Number.isInteger(asNum)) return null;
  if (asNum < 0 || asNum > 0xff) return null;
  return asNum;
}

function normalizeU32(value: unknown): number | null {
  const asNum = typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : null;
  if (asNum === null) return null;
  if (!Number.isFinite(asNum) || !Number.isInteger(asNum)) return null;
  if (asNum < 0 || asNum > 0xffff_ffff) return null;
  return asNum;
}

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array {
  // `postMessage(..., transfer)` only supports `ArrayBuffer`, not `SharedArrayBuffer`.
  // Copy here so USB payloads can be transferred between the worker and main thread.
  if (bytes.buffer instanceof ArrayBuffer) return bytes;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

function normalizeBytes(value: unknown): Uint8Array | null {
  if (value instanceof Uint8Array) return ensureArrayBufferBacked(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer) {
    const src = new Uint8Array(value);
    const out = new Uint8Array(src.byteLength);
    out.set(src);
    return out;
  }
  if (Array.isArray(value)) {
    if (!value.every((v) => typeof v === "number" && Number.isFinite(v))) return null;
    return Uint8Array.from(value as number[]);
  }
  return null;
}

function normalizeUsbHostActionKind(value: unknown): UsbHostActionKind | null {
  if (value === "controlIn" || value === "controlOut" || value === "bulkIn" || value === "bulkOut") return value;
  return null;
}

function normalizeUsbHostAction(raw: unknown): UsbHostAction | null {
  if (!raw || typeof raw !== "object") return null;
  const obj = raw as Record<string, unknown>;
  const kind = normalizeUsbHostActionKind(obj.kind);
  if (!kind) return null;
  const id = normalizeActionId(obj.id);
  if (id === null) return null;

  switch (kind) {
    case "controlIn": {
      if (!isUsbSetupPacket(obj.setup)) return null;
      return { kind: "controlIn", id, setup: obj.setup };
    }
    case "controlOut": {
      if (!isUsbSetupPacket(obj.setup)) return null;
      const data = normalizeBytes(obj.data);
      if (!data) return null;
      return { kind: "controlOut", id, setup: obj.setup, data };
    }
    case "bulkIn": {
      const endpoint = normalizeU8(obj.endpoint);
      const length = normalizeU32(obj.length);
      if (endpoint === null || length === null) return null;
      return { kind: "bulkIn", id, endpoint, length };
    }
    case "bulkOut": {
      const endpoint = normalizeU8(obj.endpoint);
      const data = normalizeBytes(obj.data);
      if (endpoint === null || !data) return null;
      return { kind: "bulkOut", id, endpoint, data };
    }
    default: {
      const neverKind: never = kind;
      void neverKind;
      return null;
    }
  }
}

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
        return;
      }

      // If a `usb.*` envelope arrives but fails validation, synthesize a fallback
      // completion (or reset the bridge) so we don't deadlock pending actions.
      if (!data || typeof data !== "object") return;
      const record = data as Record<string, unknown>;
      if (record.type === "usb.completion") {
        const completionRaw = record.completion;
        const comp = completionRaw && typeof completionRaw === "object" ? (completionRaw as Record<string, unknown>) : null;
        const id = comp ? normalizeActionId(comp.id) : null;
        const kind = comp ? normalizeUsbHostActionKind(comp.kind) : null;

        if (id !== null && kind !== null) {
          this.#lastError = `Invalid UsbHostCompletion received from broker (kind=${kind} id=${id}).`;
          this.handleCompletion(usbErrorCompletion(kind, id, "Invalid UsbHostCompletion received from broker."));
          return;
        }

        this.#lastError = "Invalid UsbHostCompletion received from broker (missing id/kind).";
        try {
          this.#bridge.reset();
        } catch (resetErr) {
          this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
        }
        this.cancelPending("WebUSB passthrough reset due to invalid completion from broker.");
        return;
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
        let extractedId: number | null = null;
        let extractedKind: UsbHostActionKind | null = null;
        try {
          extractedId = record ? normalizeActionId(record.id) : null;
          extractedKind = record ? normalizeUsbHostActionKind(record.kind) : null;
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

        let action: UsbHostAction | null = null;
        try {
          action = normalizeUsbHostAction(raw);
        } catch (err) {
          this.#lastError = formatError(err);
          try {
            this.#bridge.reset();
          } catch (resetErr) {
            this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
          }
          break;
        }

        if (!action) {
          // Avoid deadlocking the Rust-side queue: send an error completion back if we can find an id/kind.
          if (extractedId !== null && extractedKind !== null) {
            try {
              this.#bridge.push_completion(usbErrorCompletion(extractedKind, extractedId, "Invalid UsbHostAction received from WASM."));
              this.#completionsApplied++;
            } catch (err) {
              this.#lastError = formatError(err);
            }
          } else {
            // Without an id+kind we cannot fabricate a completion for the Rust-side queue.
            // Reset the bridge to clear any stuck actions so the guest can recover.
            this.#lastError =
              extractedId !== null
                ? "Invalid UsbHostAction received from WASM (missing kind)."
                : "Invalid UsbHostAction received from WASM (missing id/kind).";
            try {
              this.#bridge.reset();
            } catch (resetErr) {
              this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
            }
            this.cancelPending("WebUSB passthrough reset due to invalid action from WASM.");
            break;
          }
          continue;
        }

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
          const transfer = getTransferablesForUsbActionMessage(msg);
          if (transfer) {
            try {
              this.#port.postMessage(msg, transfer);
            } catch {
              // Some ArrayBuffers (e.g. WebAssembly.Memory.buffer) cannot be transferred.
              // Fall back to a regular structured clone (copy) rather than failing the
              // whole passthrough action.
              this.#port.postMessage(msg);
            }
          } else {
            this.#port.postMessage(msg);
          }
        } catch (err) {
          this.#pending.delete(actionId);
          try {
            this.#bridge.push_completion(
              usbErrorCompletion(action.kind, actionId, `Failed to post usb.action to broker: ${formatError(err)}`),
            );
            this.#completionsApplied++;
          } catch (pushErr) {
            this.#lastError = formatError(pushErr);
          }
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

import {
  isUsbCompletionMessage,
  isUsbRingAttachMessage,
  isUsbSelectedMessage,
  isUsbSetupPacket,
  getTransferablesForUsbActionMessage,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbQuerySelectedMessage,
  type UsbRingAttachMessage,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";
import { UsbProxyRing } from "./usb_proxy_ring";

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
    // Rust-side ids are u32 and must be representable as JS numbers. Keep the
    // runtime strict here so we never forward (or attempt to complete) actions
    // that the WASM bridge cannot match.
    if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff) return null;
    return value;
  }
  if (typeof value === "bigint") {
    if (value < 0n || value > 0xffff_ffffn) return null;
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

function isUsbEndpointAddress(endpoint: number): boolean {
  // `endpoint` is a USB endpoint address. Bit7 is direction; bits0..3 are the endpoint number.
  // Only endpoint numbers 1..=15 are valid for bulk/interrupt transfers.
  return (endpoint & 0x70) === 0 && (endpoint & 0x0f) !== 0;
}

function ensureTransferableBytes(bytes: Uint8Array): Uint8Array {
  // `postMessage(..., transfer)` only supports `ArrayBuffer`, not `SharedArrayBuffer`.
  // Also, transferring detaches the *entire* underlying buffer, so ensure the
  // view covers the full buffer before we opt into transferables.
  const buf = bytes.buffer;
  const canTransfer =
    buf instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === buf.byteLength;
  if (canTransfer) return bytes;

  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

function normalizeBytes(value: unknown): Uint8Array | null {
  if (value instanceof Uint8Array) return ensureTransferableBytes(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer) {
    return ensureTransferableBytes(new Uint8Array(value));
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
      if ((endpoint & 0x80) === 0 || !isUsbEndpointAddress(endpoint)) return null;
      return { kind: "bulkIn", id, endpoint, length };
    }
    case "bulkOut": {
      const endpoint = normalizeU8(obj.endpoint);
      const data = normalizeBytes(obj.data);
      if (endpoint === null || !data) return null;
      if ((endpoint & 0x80) !== 0 || !isUsbEndpointAddress(endpoint)) return null;
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
   * Keep the default as "not blocked" so the runtime still functions even in
   * environments where no `usb.selected` messages are ever delivered (older
   * brokers / direct execution paths).
   *
   * When `initiallyBlocked` is `true`, the runtime sends a one-time
   * `usb.querySelected` request to the broker so it can recover if it missed an
   * earlier `usb.selected ok:true` broadcast (e.g. WASM finished loading after
   * the user selected a device).
   */
  #blocked: boolean;
  #desiredRunning = false;
  #pollTimer: number | undefined;
  #pollInFlight = false;

  readonly #pending = new Map<number, PendingItem>();

  #actionRing: UsbProxyRing | null = null;
  #completionRing: UsbProxyRing | null = null;
  #completionDrainTimer: ReturnType<typeof setInterval> | null = null;

  #actionsForwarded = 0;
  #completionsApplied = 0;
  #lastError: string | null = null;

  readonly #onMessage: EventListener;

  constructor(options: {
    bridge: UsbPassthroughBridgeLike;
    port: UsbBrokerPortLike;
    pollIntervalMs?: number;
    /**
     * Override the initial "blocked" state.
     *
     * By default the runtime starts unblocked so it still functions even if it is
     * instantiated after a `usb.selected ok:true` broadcast (e.g. WASM finishes
     * loading late). Pass `true` if you want to ensure the passthrough bridge does
     * not emit host actions until a selection message is observed.
     */
    initiallyBlocked?: boolean;
  }) {
    this.#bridge = options.bridge;
    this.#port = options.port;
    this.#pollIntervalMs = options.pollIntervalMs ?? 8;
    this.#blocked = options.initiallyBlocked ?? false;

    this.#onMessage = (ev) => {
      const data = (ev as MessageEvent<unknown>).data;

      if (isUsbRingAttachMessage(data)) {
        this.attachRings(data);
        return;
      }

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

    // If we start blocked (common in worker runtimes), proactively query the broker
    // for the current selection state so we don't wedge if we missed an earlier
    // `usb.selected ok:true` broadcast (e.g. WASM initialized late).
    if (this.#blocked) {
      try {
        this.#port.postMessage({ type: "usb.querySelected" } satisfies UsbQuerySelectedMessage);
      } catch {
        // Best-effort: if the broker isn't attached yet (or doesn't understand the
        // message), we'll remain blocked until a real `usb.selected` broadcast arrives.
      }
    }
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
    this.detachRings();
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
      this.drainCompletionRing();

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
            const problems: string[] = [];
            if (extractedId === null) {
              problems.push(record && "id" in record ? "invalid id" : "missing id");
            }
            if (extractedKind === null) {
              problems.push(record && "kind" in record ? "invalid kind" : "missing kind");
            }
            this.#lastError = `Invalid UsbHostAction received from WASM (${problems.join(", ")}).`;
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

        const actionRing = this.#actionRing;
        if (actionRing) {
          try {
            if (actionRing.pushAction(action)) {
              this.#actionsForwarded++;
              awaiters.push(deferred.promise);
              continue;
            }
          } catch (err) {
            this.#lastError = `USB action ring push failed: ${formatError(err)}`;
          }
        }

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
    this.cancelPending(msg.error ?? "WebUSB device not selected.");

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

  private attachRings(msg: UsbRingAttachMessage): void {
    if (this.#actionRing && this.#completionRing) return;
    try {
      this.#actionRing = new UsbProxyRing(msg.actionRing);
      this.#completionRing = new UsbProxyRing(msg.completionRing);
    } catch (err) {
      this.#lastError = `Failed to attach USB proxy rings: ${formatError(err)}`;
      this.detachRings();
      return;
    }

    if (!this.#completionDrainTimer) {
      this.#completionDrainTimer = setInterval(() => this.drainCompletionRing(), 4);
      (this.#completionDrainTimer as unknown as { unref?: () => void }).unref?.();
    }
  }

  private detachRings(): void {
    if (this.#completionDrainTimer) {
      clearInterval(this.#completionDrainTimer);
      this.#completionDrainTimer = null;
    }
    this.#actionRing = null;
    this.#completionRing = null;
  }

  private drainCompletionRing(): void {
    const ring = this.#completionRing;
    if (!ring) return;
    while (true) {
      let completion: UsbHostCompletion | null = null;
      try {
        completion = ring.popCompletion();
      } catch (err) {
        this.#lastError = `USB completion ring pop failed: ${formatError(err)}`;
        return;
      }
      if (!completion) break;
      this.handleCompletion(completion);
    }
  }
}

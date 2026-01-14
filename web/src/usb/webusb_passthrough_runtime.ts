import {
  isUsbCompletionMessage,
  isUsbRingAttachMessage,
  type UsbRingAttachRequestMessage,
  isUsbRingDetachMessage,
  isUsbSelectedMessage,
  isUsbSetupPacket,
  getTransferablesForUsbActionMessage,
  MAX_USB_PROXY_BYTES,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbQuerySelectedMessage,
  type UsbRingAttachMessage,
  type UsbRingDetachMessage,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";
import { UsbProxyRing } from "./usb_proxy_ring";
import { subscribeUsbProxyCompletionRing } from "./usb_proxy_ring_dispatcher";

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
  if (value instanceof Uint8Array) {
    if (value.byteLength > MAX_USB_PROXY_BYTES) return null;
    return ensureTransferableBytes(value);
  }
  if (value instanceof ArrayBuffer) {
    if (value.byteLength > MAX_USB_PROXY_BYTES) return null;
    return new Uint8Array(value);
  }
  if (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer) {
    if (value.byteLength > MAX_USB_PROXY_BYTES) return null;
    return ensureTransferableBytes(new Uint8Array(value));
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_USB_PROXY_BYTES) return null;
    if (!value.every((v) => typeof v === "number" && Number.isFinite(v) && Number.isInteger(v) && v >= 0 && v <= 0xff)) return null;
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
      if (data.byteLength !== obj.setup.wLength) return null;
      return { kind: "controlOut", id, setup: obj.setup, data };
    }
    case "bulkIn": {
      const endpoint = normalizeU8(obj.endpoint);
      const length = normalizeU32(obj.length);
      if (endpoint === null || length === null) return null;
      if (length > MAX_USB_PROXY_BYTES) return null;
      if ((endpoint & 0x80) === 0 || !isUsbEndpointAddress(endpoint)) return null;
      return { kind: "bulkIn", id, endpoint, length };
    }
    case "bulkOut": {
      const endpoint = normalizeU8(obj.endpoint);
      const data = normalizeBytes(obj.data);
      if (endpoint === null || !data) return null;
      if (data.byteLength > MAX_USB_PROXY_BYTES) return null;
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
  readonly #drainActionsFn: () => unknown;
  readonly #pushCompletionFn: (completion: UsbHostCompletion) => void;
  readonly #resetFn: () => void;
  readonly #freeFn: () => void;
  readonly #pendingSummaryFn: (() => unknown) | null;
  readonly #port: UsbBrokerPortLike;
  readonly #pollIntervalMs: number;
  readonly #maxActionsPerPoll: number;

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
  #backlog: unknown[] = [];
  #backlogIndex = 0;

  #actionRing: UsbProxyRing | null = null;
  #actionRingBuffer: SharedArrayBuffer | null = null;
  #completionRingUnsubscribe: (() => void) | null = null;
  #completionRingBuffer: SharedArrayBuffer | null = null;
  #ringDetachSent = false;

  #actionsForwarded = 0;
  #completionsApplied = 0;
  #lastError: string | null = null;

  readonly #onMessage: EventListener;

  constructor(options: {
    bridge: UsbPassthroughBridgeLike;
    port: UsbBrokerPortLike;
    pollIntervalMs?: number;
    /**
     * Maximum number of actions to forward per {@link pollOnce} invocation.
     *
     * This bounds per-tick work in the I/O worker: UHCI may emit many actions in
     * a burst (e.g. bulk transfers), and draining+forwarding all of them in one
     * tick can starve unrelated I/O tasks.
     */
    maxActionsPerPoll?: number;
    /**
     * Override the initial "blocked" state.
     *
     * By default the runtime starts unblocked so it still functions even if it is
     * instantiated after a `usb.selected ok:true` broadcast (e.g. WASM finishes
     * loading late). Pass `true` if you want to ensure the passthrough bridge does
     * not emit host actions until a selection message is observed.
     */
    initiallyBlocked?: boolean;
    /**
     * Optional pre-received `usb.ringAttach` payload.
     *
     * Some worker entrypoints attach their top-level message handler before the
     * WASM USB runtimes are constructed. In that setup, `usb.ringAttach` can
     * arrive before this runtime registers its `message` event listener. Passing
     * the payload here ensures the SAB fast path is still enabled.
     */
    initialRingAttach?: UsbRingAttachMessage;
  }) {
    this.#bridge = options.bridge;
    this.#port = options.port;

    // Backwards compatibility: accept both snake_case and camelCase exports from wasm-bindgen and
    // always invoke extracted methods via `.call(bridge, ...)` to avoid `this` binding pitfalls.
    const bridgeAny = options.bridge as unknown as Record<string, unknown>;
    const drainActions = bridgeAny.drain_actions ?? bridgeAny.drainActions;
    const pushCompletion = bridgeAny.push_completion ?? bridgeAny.pushCompletion;
    const reset = bridgeAny.reset;
    const free = bridgeAny.free;
    const pendingSummary = bridgeAny.pending_summary ?? bridgeAny.pendingSummary;

    if (typeof drainActions !== "function") {
      throw new Error("UsbPassthroughBridge missing drain_actions/drainActions export.");
    }
    if (typeof pushCompletion !== "function") {
      throw new Error("UsbPassthroughBridge missing push_completion/pushCompletion export.");
    }
    if (typeof reset !== "function") {
      throw new Error("UsbPassthroughBridge missing reset() export.");
    }
    if (typeof free !== "function") {
      throw new Error("UsbPassthroughBridge missing free() export.");
    }

    this.#drainActionsFn = drainActions as () => unknown;
    this.#pushCompletionFn = pushCompletion as (completion: UsbHostCompletion) => void;
    this.#resetFn = reset as () => void;
    this.#freeFn = free as () => void;
    this.#pendingSummaryFn = typeof pendingSummary === "function" ? (pendingSummary as () => unknown) : null;

    this.#pollIntervalMs = options.pollIntervalMs ?? 8;
    const max = options.maxActionsPerPoll ?? 64;
    this.#maxActionsPerPoll = Number.isFinite(max) && max > 0 ? Math.floor(max) : Number.POSITIVE_INFINITY;
    this.#blocked = options.initiallyBlocked ?? false;

    this.#onMessage = (ev) => {
      const data = (ev as MessageEvent<unknown>).data;

      if (isUsbRingAttachMessage(data)) {
        this.attachRings(data);
        return;
      }

      if (isUsbRingDetachMessage(data)) {
        this.handleRingDetach(data);
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
          this.#resetFn.call(this.#bridge);
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

    // Request SAB rings from the broker. This is important when the runtime is
    // instantiated after the broker already sent `usb.ringAttach` (e.g. WASM
    // finished loading late). Older brokers may ignore it; we fall back to
    // `postMessage` proxying when rings are unavailable.
    try {
      this.#port.postMessage({ type: "usb.ringAttachRequest" } satisfies UsbRingAttachRequestMessage);
    } catch {
      // Best-effort; ignore if the broker isn't attached yet.
    }

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

    if (options.initialRingAttach) {
      this.attachRings(options.initialRingAttach);
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
    this.#backlog = [];
    this.#backlogIndex = 0;
  }

  destroy(): void {
    this.stop();
    this.detachRings();
    this.#port.removeEventListener("message", this.#onMessage);
    try {
      this.#freeFn.call(this.#bridge);
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
      return this.#pendingSummaryFn?.call(this.#bridge);
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
      let drained: unknown[];
      if (this.#backlogIndex < this.#backlog.length) {
        drained = this.#backlog;
      } else {
        let raw: unknown;
        try {
          raw = this.#drainActionsFn.call(this.#bridge);
        } catch (err) {
          this.#lastError = formatError(err);
          return;
        }

        if (raw == null) return;
        if (!Array.isArray(raw)) {
          this.#lastError = `UsbPassthroughBridge.drain_actions() returned non-array: ${typeof raw}`;
          return;
        }
        this.#backlog = raw;
        this.#backlogIndex = 0;
        drained = this.#backlog;
      }

      const start = this.#backlogIndex;
      const end = Math.min(drained.length, start + this.#maxActionsPerPoll);
      const batch = drained.slice(start, end);
      this.#backlogIndex = end;
      if (this.#backlogIndex >= drained.length) {
        this.#backlog = [];
        this.#backlogIndex = 0;
      }

      const awaiters: Array<Promise<UsbHostCompletion>> = [];

      for (const raw of batch) {
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
            this.#resetFn.call(this.#bridge);
          } catch (resetErr) {
            this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
          } finally {
            this.#backlog = [];
            this.#backlogIndex = 0;
          }
          break;
        }

        let action: UsbHostAction | null = null;
        try {
          action = normalizeUsbHostAction(raw);
        } catch (err) {
          this.#lastError = formatError(err);
          try {
            this.#resetFn.call(this.#bridge);
          } catch (resetErr) {
            this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
          } finally {
            this.#backlog = [];
            this.#backlogIndex = 0;
          }
          break;
        }

        if (!action) {
          // Avoid deadlocking the Rust-side queue: send an error completion back if we can find an id/kind.
          if (extractedId !== null && extractedKind !== null) {
            try {
              this.#pushCompletionFn.call(
                this.#bridge,
                usbErrorCompletion(extractedKind, extractedId, "Invalid UsbHostAction received from WASM."),
              );
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
              this.#resetFn.call(this.#bridge);
            } catch (resetErr) {
              this.#lastError = `${this.#lastError}; reset failed: ${formatError(resetErr)}`;
            } finally {
              this.#backlog = [];
              this.#backlogIndex = 0;
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
            this.#pushCompletionFn.call(
              this.#bridge,
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
            awaiters.push(deferred.promise);
            this.handleRingFailure(`USB action ring push failed: ${formatError(err)}`);
            break;
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
            this.#pushCompletionFn.call(
              this.#bridge,
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
      this.#pushCompletionFn.call(this.#bridge, completion);
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
    this.#backlog = [];
    this.#backlogIndex = 0;

    try {
      this.#resetFn.call(this.#bridge);
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

    const timer = setInterval(() => {
      void this.pollOnce();
    }, this.#pollIntervalMs) as unknown as number;
    (timer as unknown as { unref?: () => void }).unref?.();
    this.#pollTimer = timer;
  }

  private stopPolling(): void {
    if (this.#pollTimer === undefined) return;
    clearInterval(this.#pollTimer);
    this.#pollTimer = undefined;
  }

  private attachRings(msg: UsbRingAttachMessage): void {
    const currentActionBuf = this.#actionRingBuffer;
    const currentCompletionBuf = this.#completionRingBuffer;
    if (currentActionBuf === msg.actionRing && currentCompletionBuf === msg.completionRing) return;

    // `postMessage` cloning produces a new SharedArrayBuffer wrapper object each time even
    // when it references the same underlying shared memory. We must reattach so that all
    // runtimes on the same port converge on a single completion-ring dispatcher key.
    if (this.#actionRing || this.#completionRingUnsubscribe) {
      this.detachRings();
    }
    try {
      this.#actionRing = new UsbProxyRing(msg.actionRing);
      this.#actionRingBuffer = msg.actionRing;
      this.#completionRingBuffer = msg.completionRing;
      this.#completionRingUnsubscribe = subscribeUsbProxyCompletionRing(
        msg.completionRing,
        (completion) => this.handleCompletion(completion),
        { onError: (err) => this.handleRingFailure(`USB completion ring pop failed: ${formatError(err)}`) },
      );
    } catch (err) {
      this.#lastError = `Failed to attach USB proxy rings: ${formatError(err)}`;
      this.detachRings();
      return;
    }

    // Rings are active again; allow future detach requests.
    this.#ringDetachSent = false;
  }

  private detachRings(): void {
    if (this.#completionRingUnsubscribe) {
      this.#completionRingUnsubscribe();
      this.#completionRingUnsubscribe = null;
    }
    this.#actionRing = null;
    this.#actionRingBuffer = null;
    this.#completionRingBuffer = null;
  }

  private handleRingDetach(msg: UsbRingDetachMessage): void {
    const reason = msg.reason ?? "USB proxy rings disabled.";
    const hadRings = this.#actionRing !== null || this.#completionRingUnsubscribe !== null;
    if (!hadRings) {
      // Another runtime on the same MessagePort may have negotiated rings and later detached them.
      // If we were never using rings, treat this as informational and continue proxying via postMessage.
      this.#lastError = reason;
      this.detachRings();
      return;
    }
    this.handleRingFailure(reason, { notifyBroker: false });
  }

  private handleRingFailure(reason: string, options: { notifyBroker?: boolean } = {}): void {
    this.#lastError = reason;
    this.detachRings();
    this.cancelPending(reason);
    this.#backlog = [];
    this.#backlogIndex = 0;
    try {
      this.#resetFn.call(this.#bridge);
    } catch (err) {
      this.#lastError = `${this.#lastError}; reset failed: ${formatError(err)}`;
    }

    const shouldNotify = options.notifyBroker !== false;
    if (!shouldNotify) return;
    if (this.#ringDetachSent) return;
    this.#ringDetachSent = true;
    try {
      this.#port.postMessage({ type: "usb.ringDetach", reason } satisfies UsbRingDetachMessage);
    } catch {
      // ignore
    }
  }
}

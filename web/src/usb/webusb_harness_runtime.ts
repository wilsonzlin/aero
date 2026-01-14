import {
  isUsbCompletionMessage,
  isUsbRingAttachMessage,
  isUsbRingDetachMessage,
  isUsbSelectedMessage,
  isUsbSetupPacket,
  isUsbHostAction,
  isUsbHostCompletion,
  MAX_USB_PROXY_BYTES,
  usbErrorCompletion,
  type SetupPacket,
  type UsbActionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbQuerySelectedMessage,
  type UsbRingAttachMessage,
  type UsbRingAttachRequestMessage,
  type UsbRingDetachMessage,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";
import { UsbProxyRing } from "./usb_proxy_ring";
import { subscribeUsbProxyCompletionRing } from "./usb_proxy_ring_dispatcher";

export type UsbUhciHarnessStartMessage = { type: "usb.harness.start" };
export type UsbUhciHarnessStopMessage = { type: "usb.harness.stop" };

export type UsbUhciHarnessControlMessage = UsbUhciHarnessStartMessage | UsbUhciHarnessStopMessage;

export type WebUsbUhciHarnessRuntimeSnapshot = {
  /**
   * The harness export was present in the WASM module and the runtime runner is available.
   *
   * This is surfaced to the UI so dev builds can explain why the harness can't be started.
   */
  available: boolean;
  /** Start/stop toggle. */
  enabled: boolean;
  /** Set when `usb.selected ok:false` is observed. */
  blocked: boolean;
  tickCount: number;
  actionsForwarded: number;
  completionsApplied: number;
  pendingCompletions: number;
  lastAction: UsbHostAction | null;
  lastCompletion: UsbHostCompletion | null;
  deviceDescriptor: Uint8Array | null;
  configDescriptor: Uint8Array | null;
  lastError: string | null;
};

export type UsbUhciHarnessStatusMessage = { type: "usb.harness.status"; snapshot: WebUsbUhciHarnessRuntimeSnapshot };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isNullableBytes(value: unknown): value is Uint8Array | null {
  return value === null || value instanceof Uint8Array;
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isWebUsbUhciHarnessRuntimeSnapshot(value: unknown): value is WebUsbUhciHarnessRuntimeSnapshot {
  if (!isRecord(value)) return false;
  return (
    typeof value.available === "boolean" &&
    typeof value.enabled === "boolean" &&
    typeof value.blocked === "boolean" &&
    isNonNegativeSafeInteger(value.tickCount) &&
    isNonNegativeSafeInteger(value.actionsForwarded) &&
    isNonNegativeSafeInteger(value.completionsApplied) &&
    isNonNegativeSafeInteger(value.pendingCompletions) &&
    (value.lastAction === null || isUsbHostAction(value.lastAction)) &&
    (value.lastCompletion === null || isUsbHostCompletion(value.lastCompletion)) &&
    isNullableBytes(value.deviceDescriptor) &&
    isNullableBytes(value.configDescriptor) &&
    isNullableString(value.lastError)
  );
}

export function isUsbUhciHarnessStatusMessage(value: unknown): value is UsbUhciHarnessStatusMessage {
  if (!isRecord(value) || value.type !== "usb.harness.status") return false;
  return isWebUsbUhciHarnessRuntimeSnapshot(value.snapshot);
}

export type WebUsbUhciPassthroughHarnessLike = {
  tick(): void;
  drain_actions(): unknown;
  push_completion(completion: UsbHostCompletion): void;
  free(): void;
};

export type UsbBrokerPortLike = Pick<MessagePort, "addEventListener" | "removeEventListener" | "postMessage"> & {
  start?: () => void;
};

type PendingItem = { action: UsbHostAction };

function rewriteActionId(action: UsbHostAction, id: number): UsbHostAction {
  switch (action.kind) {
    case "controlIn":
      return { kind: "controlIn", id, setup: action.setup };
    case "controlOut":
      return { kind: "controlOut", id, setup: action.setup, data: action.data };
    case "bulkIn":
      return { kind: "bulkIn", id, endpoint: action.endpoint, length: action.length };
    case "bulkOut":
      return { kind: "bulkOut", id, endpoint: action.endpoint, data: action.data };
    default: {
      const neverKind: never = action;
      throw new Error(`Unknown UsbHostAction kind: ${String((neverKind as unknown as { kind?: unknown }).kind)}`);
    }
  }
}

function rewriteCompletionId(completion: UsbHostCompletion, id: number): UsbHostCompletion {
  switch (completion.kind) {
    case "controlIn":
    case "bulkIn":
      if (completion.status === "success") return { kind: completion.kind, id, status: "success", data: completion.data };
      if (completion.status === "stall") return { kind: completion.kind, id, status: "stall" };
      return { kind: completion.kind, id, status: "error", message: completion.message };
    case "controlOut":
    case "bulkOut":
      if (completion.status === "success") return { kind: completion.kind, id, status: "success", bytesWritten: completion.bytesWritten };
      if (completion.status === "stall") return { kind: completion.kind, id, status: "stall" };
      return { kind: completion.kind, id, status: "error", message: completion.message };
    default: {
      const neverKind: never = completion;
      throw new Error(`Unknown UsbHostCompletion kind: ${String((neverKind as unknown as { kind?: unknown }).kind)}`);
    }
  }
}

function normalizeActionId(value: unknown): number {
  const maxU32 = 0xffff_ffff;
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 0 || value > maxU32) {
      throw new Error(`USB action id must fit in uint32, got ${String(value)}`);
    }
    return value;
  }
  if (typeof value === "bigint") {
    if (value < 0n) throw new Error(`USB action id must be non-negative, got ${value.toString()}`);
    if (value > BigInt(maxU32)) {
      throw new Error(`USB action id must fit in uint32, got ${value.toString()}`);
    }
    return Number(value);
  }
  throw new Error(`Expected action id to be number or bigint, got ${typeof value}`);
}

function normalizeU8(value: unknown): number {
  const asNum = typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : NaN;
  if (!Number.isFinite(asNum) || !Number.isInteger(asNum) || asNum < 0 || asNum > 0xff) {
    throw new Error(`Expected uint8, got ${String(value)}`);
  }
  return asNum;
}

function normalizeU32(value: unknown): number {
  const asNum = typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : NaN;
  if (!Number.isFinite(asNum) || !Number.isInteger(asNum) || asNum < 0 || asNum > 0xffff_ffff) {
    throw new Error(`Expected uint32, got ${String(value)}`);
  }
  return asNum;
}

function isUsbEndpointAddress(value: number): boolean {
  // `value` should be a USB endpoint address, not just an endpoint number:
  // - bit7 = direction (IN=1, OUT=0)
  // - bits4..6 must be 0 (endpoint numbers are 0..=15)
  // - endpoint 0 is the control pipe and should not be used for bulk/interrupt actions
  return (value & 0x70) === 0 && (value & 0x0f) !== 0;
}

function assertUsbInEndpointAddress(value: number): void {
  if (!isUsbEndpointAddress(value) || (value & 0x80) === 0) {
    throw new Error(`Expected IN endpoint address (e.g. 0x81), got 0x${value.toString(16)}`);
  }
}

function assertUsbOutEndpointAddress(value: number): void {
  if (!isUsbEndpointAddress(value) || (value & 0x80) !== 0) {
    throw new Error(`Expected OUT endpoint address (e.g. 0x02), got 0x${value.toString(16)}`);
  }
}

function normalizeBytes(value: unknown): Uint8Array {
  if (value instanceof Uint8Array) {
    // Avoid leaking (or copying) more than intended across postMessage:
    // - If the bytes reference a SharedArrayBuffer (e.g. WASM threads), copy so we don't
    //   implicitly share the entire underlying SAB.
    // - If the bytes are a subview of a larger ArrayBuffer (e.g. WebAssembly.Memory.buffer),
    //   copy so structured-cloning doesn't duplicate the entire underlying buffer.
    if (value.byteLength > MAX_USB_PROXY_BYTES) {
      throw new Error(`Expected byte payload <= ${MAX_USB_PROXY_BYTES} bytes, got ${value.byteLength}`);
    }

    if (
      value.buffer instanceof ArrayBuffer &&
      value.byteOffset === 0 &&
      value.byteLength === value.buffer.byteLength
    ) {
      return value;
    }
    const out = new Uint8Array(value.byteLength);
    out.set(value);
    return out;
  }
  if (value instanceof ArrayBuffer) {
    if (value.byteLength > MAX_USB_PROXY_BYTES) {
      throw new Error(`Expected byte payload <= ${MAX_USB_PROXY_BYTES} bytes, got ${value.byteLength}`);
    }
    return new Uint8Array(value);
  }
  if (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer) {
    if (value.byteLength > MAX_USB_PROXY_BYTES) {
      throw new Error(`Expected byte payload <= ${MAX_USB_PROXY_BYTES} bytes, got ${value.byteLength}`);
    }
    // See above: copy into an ArrayBuffer-backed Uint8Array so we don't share the whole SAB.
    const out = new Uint8Array(value.byteLength);
    out.set(new Uint8Array(value));
    return out;
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_USB_PROXY_BYTES) {
      throw new Error(`Expected byte payload <= ${MAX_USB_PROXY_BYTES} bytes, got ${value.length}`);
    }
    if (
      !value.every(
        (v) => typeof v === "number" && Number.isFinite(v) && Number.isInteger(v) && v >= 0 && v <= 0xff,
      )
    ) {
      throw new Error("Expected byte array to contain only uint8 numbers");
    }
    return Uint8Array.from(value as number[]);
  }
  throw new Error(`Expected bytes to be Uint8Array, ArrayBuffer, SharedArrayBuffer, or number[]; got ${typeof value}`);
}

function normalizeUsbHostAction(raw: unknown): UsbHostAction {
  if (!raw || typeof raw !== "object") {
    throw new Error(`Expected USB action to be object, got ${raw === null ? "null" : typeof raw}`);
  }
  const obj = raw as Record<string, unknown>;
  const kind = obj.kind;
  const id = normalizeActionId(obj.id);
  if (typeof kind !== "string") throw new Error("USB action missing kind");

  switch (kind as UsbHostAction["kind"]) {
    case "controlIn": {
      const setup = obj.setup;
      if (!isUsbSetupPacket(setup)) throw new Error("controlIn missing/invalid setup packet");
      return { kind: "controlIn", id, setup };
    }
    case "controlOut": {
      const setup = obj.setup;
      if (!isUsbSetupPacket(setup)) throw new Error("controlOut missing/invalid setup packet");
      const data = normalizeBytes(obj.data);
      if (data.byteLength !== setup.wLength) {
        throw new Error(`controlOut payload length mismatch (wLength=${setup.wLength} data=${data.byteLength})`);
      }
      return { kind: "controlOut", id, setup, data };
    }
    case "bulkIn": {
      const endpoint = normalizeU8(obj.endpoint);
      assertUsbInEndpointAddress(endpoint);
      const length = normalizeU32(obj.length);
      if (length > MAX_USB_PROXY_BYTES) {
        throw new Error(`bulkIn length too large: ${length}`);
      }
      return { kind: "bulkIn", id, endpoint, length };
    }
    case "bulkOut": {
      const endpoint = normalizeU8(obj.endpoint);
      assertUsbOutEndpointAddress(endpoint);
      return { kind: "bulkOut", id, endpoint, data: normalizeBytes(obj.data) };
    }
    default:
      throw new Error(`Unknown USB action kind: ${String(kind)}`);
  }
}

function asUsbHostActions(raw: unknown): UsbHostAction[] {
  if (raw === null || raw === undefined) return [];
  if (!Array.isArray(raw)) {
    throw new Error(`Expected harness.drain_actions() to return an array, got ${typeof raw}`);
  }
  return raw.map((action) => normalizeUsbHostAction(action));
}

const GET_DESCRIPTOR = 0x06;
const DESCRIPTOR_TYPE_DEVICE = 0x01;
const DESCRIPTOR_TYPE_CONFIGURATION = 0x02;

type DescriptorCapture = {
  deviceDescriptor: Uint8Array | null;
  configDescriptor: Uint8Array | null;
};

function classifyDescriptorRequest(setup: SetupPacket): "device" | "config" | null {
  if ((setup.bRequest & 0xff) !== GET_DESCRIPTOR) return null;
  const descType = (setup.wValue >> 8) & 0xff;
  if (descType === DESCRIPTOR_TYPE_DEVICE) return "device";
  if (descType === DESCRIPTOR_TYPE_CONFIGURATION) return "config";
  return null;
}

function maybeCaptureDescriptors(
  capture: DescriptorCapture,
  action: UsbHostAction,
  completion: UsbHostCompletion,
): { changed: boolean } {
  if (action.kind !== "controlIn") return { changed: false };
  if (completion.kind !== "controlIn") return { changed: false };
  if (completion.status !== "success") return { changed: false };

  const cls = classifyDescriptorRequest(action.setup);
  if (!cls) return { changed: false };

  const bytes = completion.data;
  if (cls === "device") {
    if (!capture.deviceDescriptor || bytes.byteLength >= capture.deviceDescriptor.byteLength) {
      capture.deviceDescriptor = bytes;
      return { changed: true };
    }
    return { changed: false };
  }

  if (!capture.configDescriptor || bytes.byteLength >= capture.configDescriptor.byteLength) {
    capture.configDescriptor = bytes;
    return { changed: true };
  }
  return { changed: false };
}

function formatError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

function normalizeWebUsbUhciPassthroughHarnessLike(harness: WebUsbUhciPassthroughHarnessLike): WebUsbUhciPassthroughHarnessLike {
  const anyHarness = harness as unknown as Record<string, unknown>;

  // Backwards compatibility: accept camelCase exports and always invoke extracted methods via
  // `.call(harness, ...)` to avoid wasm-bindgen `this` binding pitfalls.
  const tick = anyHarness.tick;
  const drainActions = anyHarness.drain_actions ?? anyHarness.drainActions;
  const pushCompletion = anyHarness.push_completion ?? anyHarness.pushCompletion;
  const free = anyHarness.free;

  if (typeof tick !== "function") throw new Error("WebUsbUhciPassthroughHarness missing tick() export.");
  if (typeof drainActions !== "function") throw new Error("WebUsbUhciPassthroughHarness missing drain_actions/drainActions export.");
  if (typeof pushCompletion !== "function")
    throw new Error("WebUsbUhciPassthroughHarness missing push_completion/pushCompletion export.");
  if (typeof free !== "function") throw new Error("WebUsbUhciPassthroughHarness missing free() export.");

  return {
    tick: () => {
      (tick as () => void).call(harness);
    },
    drain_actions: () => {
      return (drainActions as () => unknown).call(harness);
    },
    push_completion: (completion) => {
      (pushCompletion as (completion: UsbHostCompletion) => void).call(harness, completion);
    },
    free: () => {
      (free as () => void).call(harness);
    },
  };
}

function safeFree(obj: WebUsbUhciPassthroughHarnessLike | null): void {
  if (!obj) return;
  try {
    obj.free();
  } catch {
    // ignore
  }
}

/**
 * Worker-side runner for the optional WASM `WebUsbUhciPassthroughHarness`.
 *
 * The harness is ticked from the I/O worker's main tick loop. It emits `UsbHostAction`
 * messages which are proxied to the main thread's `UsbBroker` (via `usb.action`) and
 * receives `usb.completion` replies that are pushed back into the harness.
 */
export class WebUsbUhciHarnessRuntime {
  readonly #createHarness: () => WebUsbUhciPassthroughHarnessLike;
  readonly #port: UsbBrokerPortLike;
  readonly #onUpdate?: (snapshot: WebUsbUhciHarnessRuntimeSnapshot) => void;

  #enabled = false;
  #blocked = true;
  #harness: WebUsbUhciPassthroughHarnessLike | null = null;

  // Remap harness-emitted ids to a high, monotonically-increasing broker id range.
  //
  // This avoids collisions with ids emitted by other WASM USB action sources (e.g. UsbPassthroughBridge),
  // and prevents a completion from a previous run from accidentally matching a re-used harness id after reset.
  #nextBrokerId = 2_000_000_000;
  readonly #pending = new Map<number, PendingItem>();
  readonly #pendingHarnessIds = new Set<number>();

  readonly #capture: DescriptorCapture = { deviceDescriptor: null, configDescriptor: null };

  #actionRing: UsbProxyRing | null = null;
  #actionRingBuffer: SharedArrayBuffer | null = null;
  #completionRingUnsubscribe: (() => void) | null = null;
  #completionRingBuffer: SharedArrayBuffer | null = null;
  #ringDetachSent = false;

  #tickCount = 0;
  #actionsForwarded = 0;
  #completionsApplied = 0;
  #lastAction: UsbHostAction | null = null;
  #lastCompletion: UsbHostCompletion | null = null;
  #lastError: string | null = null;

  readonly #onMessage: EventListener;

  constructor(options: {
    createHarness: () => WebUsbUhciPassthroughHarnessLike;
    port: UsbBrokerPortLike;
    onUpdate?: (snapshot: WebUsbUhciHarnessRuntimeSnapshot) => void;
    /**
     * Override the initial "blocked" state.
     *
     * Default is `true` so the harness doesn't immediately run before the user selects
     * a WebUSB device. `UsbBroker` will send `usb.selected ok:true` when a device is
     * attached, unblocking the runtime. When starting blocked, the runtime also sends
     * a `usb.querySelected` message to the broker so it can learn about devices that
     * were selected before the harness runner initialized.
     */
    initiallyBlocked?: boolean;
    /**
     * Optional pre-received `usb.ringAttach` payload.
     *
     * Some worker entrypoints attach their top-level message handler before the
     * harness runtime is constructed. In that setup, `usb.ringAttach` can arrive
     * before this runtime registers its `message` event listener. Passing the
     * payload here ensures the SAB fast path is still enabled.
     */
    initialRingAttach?: UsbRingAttachMessage;
  }) {
    this.#createHarness = options.createHarness;
    this.#port = options.port;
    this.#onUpdate = options.onUpdate;
    this.#blocked = options.initiallyBlocked ?? true;

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
      }
    };

    this.#port.addEventListener("message", this.#onMessage);
    // When using addEventListener() MessagePorts need start() to begin dispatch.
    (this.#port as unknown as { start?: () => void }).start?.();

    // Request SAB rings from the broker. This is useful when the runtime starts
    // after the broker already sent `usb.ringAttach` (e.g. WASM loaded late).
    try {
      this.#port.postMessage({ type: "usb.ringAttachRequest" } satisfies UsbRingAttachRequestMessage);
    } catch {
      // ignore
    }

    // If we start blocked, proactively ask the broker for the current selection
    // state so we don't wedge when a device was selected before this harness
    // runtime finished initializing.
    if (this.#blocked) {
      try {
        this.#port.postMessage({ type: "usb.querySelected" } satisfies UsbQuerySelectedMessage);
      } catch {
        // ignore
      }
    }

    if (options.initialRingAttach) {
      this.attachRings(options.initialRingAttach);
    }

    this.emitUpdate();
  }

  start(): void {
    this.resetState();
    this.#enabled = true;
    this.#lastError = null;
    this.ensureHarness();
    this.emitUpdate();
  }

  stop(reason?: string): void {
    this.#enabled = false;
    this.resetState();
    this.#lastError = reason ?? null;
    this.emitUpdate();
  }

  destroy(): void {
    this.stop();
    this.detachRings();
    this.#port.removeEventListener("message", this.#onMessage);
  }

  /**
   * Called on every I/O worker tick while enabled.
   */
  pollOnce(): void {
    if (!this.#enabled) return;
    if (this.#blocked) return;

    const harness = this.#harness;
    if (!harness) return;

    try {
      harness.tick();
      this.#tickCount += 1;
    } catch (err) {
      this.#lastError = `harness.tick() failed: ${formatError(err)}`;
      this.stop(this.#lastError);
      return;
    }

    let drained: unknown;
    try {
      drained = harness.drain_actions();
    } catch (err) {
      this.#lastError = `harness.drain_actions() failed: ${formatError(err)}`;
      this.stop(this.#lastError);
      return;
    }

    let actions: UsbHostAction[];
    try {
      actions = asUsbHostActions(drained);
    } catch (err) {
      this.#lastError = formatError(err);
      this.stop(this.#lastError);
      return;
    }

    if (actions.length === 0) return;

    let changed = false;
    for (const action of actions) {
      const { id } = action;

      if (this.#pendingHarnessIds.has(id)) {
        // Avoid deadlocking the harness on a duplicate id. Push an error completion.
        try {
          harness.push_completion(usbErrorCompletion(action.kind, id, `Duplicate UsbHostAction id: ${id}`));
          this.#completionsApplied += 1;
          changed = true;
        } catch (err) {
          this.#lastError = formatError(err);
        }
        continue;
      }

      const brokerId = this.#nextBrokerId;
      this.#nextBrokerId += 1;
      if (!Number.isSafeInteger(brokerId) || brokerId < 0 || brokerId > 0xffff_ffff) {
        this.#lastError = `WebUsbUhciHarnessRuntime ran out of valid broker action IDs (next=${this.#nextBrokerId})`;
        this.stop(this.#lastError);
        return;
      }
      const brokerAction = rewriteActionId(action, brokerId);

      const actionRing = this.#actionRing;
      if (actionRing) {
        try {
          if (actionRing.pushAction(brokerAction)) {
            this.#pending.set(brokerId, { action });
            this.#pendingHarnessIds.add(id);
            this.#actionsForwarded += 1;
            this.#lastAction = action;
            changed = true;
            continue;
          }
        } catch (err) {
          this.handleRingFailure(`USB action ring push failed: ${formatError(err)}`);
          return;
        }
      }

      const msg: UsbActionMessage = { type: "usb.action", action: brokerAction };
      try {
        this.#port.postMessage(msg);
      } catch (err) {
        // Feed an error completion back into the harness so it can make progress (or fail fast).
        try {
          harness.push_completion(usbErrorCompletion(action.kind, id, `Failed to post usb.action: ${formatError(err)}`));
          this.#completionsApplied += 1;
        } catch (pushErr) {
          this.#lastError = formatError(pushErr);
        }
        this.#lastError = formatError(err);
        changed = true;
        continue;
      }

      this.#pending.set(brokerId, { action });
      this.#pendingHarnessIds.add(id);
      this.#actionsForwarded += 1;
      this.#lastAction = action;
      changed = true;
    }

    if (changed) this.emitUpdate();
  }

  getSnapshot(): WebUsbUhciHarnessRuntimeSnapshot {
    return {
      available: true,
      enabled: this.#enabled,
      blocked: this.#blocked,
      tickCount: this.#tickCount,
      actionsForwarded: this.#actionsForwarded,
      completionsApplied: this.#completionsApplied,
      pendingCompletions: this.#pending.size,
      lastAction: this.#lastAction,
      lastCompletion: this.#lastCompletion,
      deviceDescriptor: this.#capture.deviceDescriptor,
      configDescriptor: this.#capture.configDescriptor,
      lastError: this.#lastError,
    };
  }

  private ensureHarness(): void {
    if (this.#harness) return;
    try {
      this.#harness = normalizeWebUsbUhciPassthroughHarnessLike(this.#createHarness());
    } catch (err) {
      this.#lastError = `Failed to construct WebUsbUhciPassthroughHarness: ${formatError(err)}`;
      this.stop(this.#lastError);
    }
  }

  private handleCompletion(completion: UsbHostCompletion): void {
    const pending = this.#pending.get(completion.id);
    if (!pending) return;
    this.#pending.delete(completion.id);
    this.#pendingHarnessIds.delete(pending.action.id);

    const harness = this.#harness;
    if (!harness) return;

    const harnessCompletion = rewriteCompletionId(completion, pending.action.id);

    let changed = false;
    try {
      harness.push_completion(harnessCompletion);
      this.#completionsApplied += 1;
      this.#lastCompletion = harnessCompletion;
      changed = true;
    } catch (err) {
      this.#lastError = formatError(err);
      changed = true;
    }

    const captureRes = maybeCaptureDescriptors(this.#capture, pending.action, harnessCompletion);
    if (captureRes.changed) changed = true;

    if (changed) this.emitUpdate();
  }

  private handleSelected(msg: UsbSelectedMessage): void {
    if (msg.ok) {
      this.#blocked = false;
      this.emitUpdate();
      return;
    }

    this.#blocked = true;
    this.stop(msg.error ?? "WebUSB device not selected.");
  }

  private emitUpdate(): void {
    if (!this.#onUpdate) return;
    try {
      this.#onUpdate(this.getSnapshot());
    } catch {
      // ignore observer errors
    }
  }

  private resetState(): void {
    this.#pending.clear();
    this.#pendingHarnessIds.clear();
    this.#tickCount = 0;
    this.#actionsForwarded = 0;
    this.#completionsApplied = 0;
    this.#lastAction = null;
    this.#lastCompletion = null;
    this.#capture.deviceDescriptor = null;
    this.#capture.configDescriptor = null;

    safeFree(this.#harness);
    this.#harness = null;
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

    // Completions are drained by `usb_proxy_ring_dispatcher.ts` so multiple runtimes can
    // coexist on the same MessagePort without stealing completions from each other.

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
    this.handleRingFailure(reason, { notifyBroker: false });
  }

  private handleRingFailure(reason: string, options: { notifyBroker?: boolean } = {}): void {
    const hadRings = this.#actionRing !== null || this.#completionRingUnsubscribe !== null;
    this.detachRings();
    this.#lastError = reason;
    if (hadRings) this.cancelPending(reason);
    this.emitUpdate();

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

  private cancelPending(reason: string): void {
    const harness = this.#harness;
    if (!harness) {
      this.#pending.clear();
      this.#pendingHarnessIds.clear();
      return;
    }

    for (const pending of this.#pending.values()) {
      try {
        const completion = usbErrorCompletion(pending.action.kind, pending.action.id, reason);
        harness.push_completion(completion);
        this.#completionsApplied += 1;
        this.#lastCompletion = completion;
      } catch (err) {
        this.#lastError = formatError(err);
        break;
      }
    }

    this.#pending.clear();
    this.#pendingHarnessIds.clear();
  }
}

import {
  isUsbHostAction,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";

type WasmUsbSetupPacket = {
  bmRequestType: number;
  bRequest: number;
  wValue: number;
  wIndex: number;
  wLength: number;
};

type WasmUsbHostAction =
  | { kind: "controlIn"; id: number; setup: WasmUsbSetupPacket }
  | { kind: "controlOut"; id: number; setup: WasmUsbSetupPacket; data: Uint8Array | number[] }
  | { kind: "bulkIn"; id: number; endpoint: number; length: number }
  | { kind: "bulkOut"; id: number; endpoint: number; data: Uint8Array | number[] };

export type UsbPassthroughDemoResult =
  | { status: "success"; data: Uint8Array }
  | { status: "stall" }
  | { status: "error"; message: string };

export type UsbPassthroughDemoResultMessage = { type: "usb.demoResult"; result: UsbPassthroughDemoResult };

export type UsbPassthroughDemoRunMessage =
  | { type: "usb.demo.run"; request: "deviceDescriptor"; length?: number }
  | { type: "usb.demo.run"; request: "configDescriptor"; length?: number };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function isUsbPassthroughDemoResultMessage(value: unknown): value is UsbPassthroughDemoResultMessage {
  if (!value || typeof value !== "object") return false;
  const msg = value as { type?: unknown; result?: unknown };
  if (msg.type !== "usb.demoResult") return false;
  if (!msg.result || typeof msg.result !== "object") return false;
  const result = msg.result as { status?: unknown; data?: unknown; message?: unknown };
  switch (result.status) {
    case "success":
      return result.data instanceof Uint8Array;
    case "stall":
      return true;
    case "error":
      return typeof result.message === "string";
    default:
      return false;
  }
}

export function isUsbPassthroughDemoRunMessage(value: unknown): value is UsbPassthroughDemoRunMessage {
  if (!isRecord(value) || value.type !== "usb.demo.run") return false;
  const request = value.request;
  if (request !== "deviceDescriptor" && request !== "configDescriptor") return false;
  if (value.length === undefined) return true;
  return typeof value.length === "number" && Number.isInteger(value.length) && value.length >= 0 && value.length <= 0xffff;
}

export interface UsbPassthroughDemoApi {
  reset(): void;
  queue_get_device_descriptor(len: number): void;
  queue_get_config_descriptor(len: number): void;
  drain_actions(): unknown;
  push_completion(completion: unknown): void;
  poll_last_result(): unknown;
}

const MAX_COERCED_BYTES = 1024 * 1024;

function ensureTransferableBytes(bytes: Uint8Array): Uint8Array | null {
  // For `postMessage` safety/perf, avoid passing TypedArrays that reference:
  // - a SharedArrayBuffer (would implicitly share the entire underlying buffer), or
  // - a subview of a larger ArrayBuffer (would prevent transfers / expose unrelated bytes).
  if (bytes.byteLength > MAX_COERCED_BYTES) return null;
  const buf = bytes.buffer;
  if (buf instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === buf.byteLength) return bytes;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

function coerceBytes(value: unknown): Uint8Array | null {
  if (value instanceof Uint8Array) return ensureTransferableBytes(value);
  if (value instanceof ArrayBuffer) {
    if (value.byteLength > MAX_COERCED_BYTES) return null;
    return new Uint8Array(value);
  }
  if (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer) {
    if (value.byteLength > MAX_COERCED_BYTES) return null;
    return ensureTransferableBytes(new Uint8Array(value));
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_COERCED_BYTES) return null;
    const out = new Uint8Array(value.length);
    for (let i = 0; i < value.length; i += 1) {
      const b = value[i];
      if (typeof b !== "number" || !Number.isFinite(b) || !Number.isInteger(b) || b < 0 || b > 0xff) return null;
      out[i] = b;
    }
    return out;
  }
  return null;
}

function wasmActionToProxyAction(action: WasmUsbHostAction): UsbHostAction {
  switch (action.kind) {
    case "controlIn":
      return { kind: "controlIn", id: action.id, setup: action.setup };
    case "controlOut": {
      const data = coerceBytes(action.data);
      if (!data) {
        throw new Error("Invalid wasm USB controlOut action payload (expected bytes).");
      }
      return { kind: "controlOut", id: action.id, setup: action.setup, data };
    }
    case "bulkIn":
      return { kind: "bulkIn", id: action.id, endpoint: action.endpoint, length: action.length };
    case "bulkOut": {
      const data = coerceBytes(action.data);
      if (!data) {
        throw new Error("Invalid wasm USB bulkOut action payload (expected bytes).");
      }
      return { kind: "bulkOut", id: action.id, endpoint: action.endpoint, data };
    }
    default: {
      const neverAction: never = action;
      throw new Error(`Unknown wasm USB action kind: ${String((neverAction as { kind?: unknown }).kind)}`);
    }
  }
}

function parseDemoResult(raw: unknown): UsbPassthroughDemoResult | null {
  if (!raw || typeof raw !== "object") return null;
  const v = raw as { status?: unknown; data?: unknown; message?: unknown };
  if (v.status === "stall") return { status: "stall" };
  if (v.status === "error") return { status: "error", message: typeof v.message === "string" ? v.message : "error" };
  if (v.status === "success") {
    const data = coerceBytes(v.data);
    if (!data) return null;
    return { status: "success", data };
  }
  return null;
}

// Allocate proxy action IDs from a high range to avoid colliding with IDs generated by other
// Rust-side `UsbPassthroughDevice` instances (which start at 1 and are part of the canonical
// UsbHostAction wire contract).
//
// Collisions would be disastrous because the main-thread `UsbBroker` echoes completions with the
// same `id`, and the IO worker might have multiple independent USB action sources running
// concurrently (e.g. the UHCI bridge + this demo driver).
const USB_PASSTHROUGH_DEMO_ID_BASE = 1_000_000_000;

export class UsbPassthroughDemoRuntime {
  readonly #demo: UsbPassthroughDemoApi;
  readonly #resetFn: () => void;
  readonly #queueGetDeviceDescriptorFn: (len: number) => void;
  readonly #queueGetConfigDescriptorFn: (len: number) => void;
  readonly #drainActionsFn: () => unknown;
  readonly #pushCompletionFn: (completion: unknown) => void;
  readonly #pollLastResultFn: () => unknown;
  readonly #postMessage: (msg: UsbActionMessage | UsbPassthroughDemoResultMessage) => void;
  readonly #inflightByProxyId = new Map<number, { wasmId: number; kind: UsbHostAction["kind"] }>();
  #nextProxyId = USB_PASSTHROUGH_DEMO_ID_BASE;

  constructor(opts: { demo: UsbPassthroughDemoApi; postMessage: (msg: UsbActionMessage | UsbPassthroughDemoResultMessage) => void }) {
    this.#demo = opts.demo;
    this.#postMessage = opts.postMessage;

    // Backwards compatibility: accept both snake_case and camelCase demo exports and always invoke
    // extracted methods via `.call(demo, ...)` to avoid wasm-bindgen `this` binding pitfalls.
    const demoAny = opts.demo as unknown as Record<string, unknown>;
    const reset = demoAny.reset;
    const queueGetDeviceDescriptor = demoAny.queue_get_device_descriptor ?? demoAny.queueGetDeviceDescriptor;
    const queueGetConfigDescriptor = demoAny.queue_get_config_descriptor ?? demoAny.queueGetConfigDescriptor;
    const drainActions = demoAny.drain_actions ?? demoAny.drainActions;
    const pushCompletion = demoAny.push_completion ?? demoAny.pushCompletion;
    const pollLastResult = demoAny.poll_last_result ?? demoAny.pollLastResult;

    if (typeof reset !== "function") throw new Error("UsbPassthroughDemo missing reset() export.");
    if (typeof queueGetDeviceDescriptor !== "function") {
      throw new Error("UsbPassthroughDemo missing queue_get_device_descriptor/queueGetDeviceDescriptor export.");
    }
    if (typeof queueGetConfigDescriptor !== "function") {
      throw new Error("UsbPassthroughDemo missing queue_get_config_descriptor/queueGetConfigDescriptor export.");
    }
    if (typeof drainActions !== "function") throw new Error("UsbPassthroughDemo missing drain_actions/drainActions export.");
    if (typeof pushCompletion !== "function") throw new Error("UsbPassthroughDemo missing push_completion/pushCompletion export.");
    if (typeof pollLastResult !== "function") throw new Error("UsbPassthroughDemo missing poll_last_result/pollLastResult export.");

    this.#resetFn = reset as () => void;
    this.#queueGetDeviceDescriptorFn = queueGetDeviceDescriptor as (len: number) => void;
    this.#queueGetConfigDescriptorFn = queueGetConfigDescriptor as (len: number) => void;
    this.#drainActionsFn = drainActions as () => unknown;
    this.#pushCompletionFn = pushCompletion as (completion: unknown) => void;
    this.#pollLastResultFn = pollLastResult as () => unknown;
  }

  reset(): void {
    this.#resetFn.call(this.#demo);
    this.#inflightByProxyId.clear();
  }

  run(request: UsbPassthroughDemoRunMessage["request"], length?: number): void {
    this.reset();
    const len =
      typeof length === "number" && Number.isInteger(length) && length >= 0 && length <= 0xffff
        ? length
        : request === "deviceDescriptor"
          ? 18
          : 255;

    if (request === "deviceDescriptor") {
      this.#queueGetDeviceDescriptorFn.call(this.#demo, len);
    } else if (request === "configDescriptor") {
      this.#queueGetConfigDescriptorFn.call(this.#demo, len);
    }

    this.tick();
    this.pollResults();
  }

  onUsbSelected(msg: UsbSelectedMessage): void {
    if (!msg.ok) {
      this.reset();
      return;
    }
    // Auto-run the device descriptor request when a new WebUSB device is selected.
    this.run("deviceDescriptor", 18);
  }

  onUsbCompletion(msg: UsbCompletionMessage): void {
    const completion = msg.completion;
    const info = this.#inflightByProxyId.get(completion.id);
    if (!info) return;
    this.#inflightByProxyId.delete(completion.id);

    if (completion.kind !== info.kind) {
      this.#pushCompletionFn.call(
        this.#demo,
        usbErrorCompletion(
          info.kind,
          info.wasmId,
          `USB completion kind mismatch (expected ${info.kind}, got ${completion.kind})`,
        ),
      );
      this.pollResults();
      return;
    }

    this.#pushCompletionFn.call(this.#demo, { ...completion, id: info.wasmId } satisfies UsbHostCompletion);
    this.pollResults();
  }

  tick(): void {
    const rawActions = this.#drainActionsFn.call(this.#demo);
    if (rawActions === null || rawActions === undefined) return;
    if (!Array.isArray(rawActions)) {
      throw new Error("UsbPassthroughDemo emitted an invalid actions payload (expected array).");
    }
    for (const raw of rawActions) {
      if (!isRecord(raw)) {
        throw new Error("UsbPassthroughDemo emitted an invalid USB action (expected object).");
      }
      const action = raw as WasmUsbHostAction;
      const kind = (action as { kind?: unknown }).kind;
      const id = (action as { id?: unknown }).id;
      if (typeof kind !== "string") {
        throw new Error(`UsbPassthroughDemo emitted an invalid USB action (missing kind).`);
      }
      if (typeof id !== "number" || !Number.isSafeInteger(id) || id < 0 || id > 0xffff_ffff) {
        throw new Error(`UsbPassthroughDemo emitted an invalid USB action id: ${String(id)}`);
      }

      const proxy = wasmActionToProxyAction(action);

      const proxyId = this.#nextProxyId;
      this.#nextProxyId += 1;
      if (!Number.isSafeInteger(proxyId) || proxyId < 0 || proxyId > 0xffff_ffff) {
        throw new Error(`USB passthrough demo ran out of valid action IDs (next=${this.#nextProxyId})`);
      }

      const outgoing = { ...proxy, id: proxyId } satisfies UsbHostAction;
      if (!isUsbHostAction(outgoing)) {
        throw new Error(`UsbPassthroughDemo emitted an invalid USB host action (kind=${proxy.kind}).`);
      }

      this.#inflightByProxyId.set(proxyId, { wasmId: proxy.id, kind: proxy.kind });
      this.#postMessage({ type: "usb.action", action: outgoing });
    }
  }

  pollResults(): void {
    while (true) {
      const raw = this.#pollLastResultFn.call(this.#demo);
      if (raw === null || raw === undefined) return;
      const result = parseDemoResult(raw);
      if (!result) {
        throw new Error("UsbPassthroughDemo emitted an invalid result payload.");
      }
      this.#postMessage({ type: "usb.demoResult", result });
    }
  }
}

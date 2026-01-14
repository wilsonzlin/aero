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
  type UsbProxyActionOptions,
  type UsbQuerySelectedMessage,
  type UsbRingAttachMessage,
  type UsbRingAttachRequestMessage,
  type UsbRingDetachMessage,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";
import { UsbProxyRing } from "./usb_proxy_ring";
import { subscribeUsbProxyCompletionRing } from "./usb_proxy_ring_dispatcher";

// -------------------------------------------------------------------------------------------------
// Worker ↔ main-thread message types
// -------------------------------------------------------------------------------------------------

export type UsbEhciHarnessAttachControllerMessage = { type: "usb.ehciHarness.attachController" };
export type UsbEhciHarnessDetachControllerMessage = { type: "usb.ehciHarness.detachController" };
export type UsbEhciHarnessAttachDeviceMessage = { type: "usb.ehciHarness.attachDevice" };
export type UsbEhciHarnessDetachDeviceMessage = { type: "usb.ehciHarness.detachDevice" };
export type UsbEhciHarnessGetDeviceDescriptorMessage = { type: "usb.ehciHarness.getDeviceDescriptor" };
export type UsbEhciHarnessGetConfigDescriptorMessage = { type: "usb.ehciHarness.getConfigDescriptor" };
export type UsbEhciHarnessClearUsbStsMessage = { type: "usb.ehciHarness.clearUsbSts"; bits: number };

export type UsbEhciHarnessControlMessage =
  | UsbEhciHarnessAttachControllerMessage
  | UsbEhciHarnessDetachControllerMessage
  | UsbEhciHarnessAttachDeviceMessage
  | UsbEhciHarnessDetachDeviceMessage
  | UsbEhciHarnessGetDeviceDescriptorMessage
  | UsbEhciHarnessGetConfigDescriptorMessage
  | UsbEhciHarnessClearUsbStsMessage;

export type WebUsbEhciHarnessRuntimeSnapshot = {
  available: boolean;
  blocked: boolean;

  controllerAttached: boolean;
  deviceAttached: boolean;

  tickCount: number;
  actionsForwarded: number;
  completionsApplied: number;
  pendingCompletions: number;

  irqLevel: boolean;
  usbSts: number;
  usbStsUsbInt: boolean;
  usbStsUsbErrInt: boolean;
  usbStsPcd: boolean;

  lastAction: UsbHostAction | null;
  lastCompletion: UsbHostCompletion | null;
  deviceDescriptor: Uint8Array | null;
  configDescriptor: Uint8Array | null;
  lastError: string | null;
};

export type UsbEhciHarnessStatusMessage = { type: "usb.ehciHarness.status"; snapshot: WebUsbEhciHarnessRuntimeSnapshot };

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

function isWebUsbEhciHarnessRuntimeSnapshot(value: unknown): value is WebUsbEhciHarnessRuntimeSnapshot {
  if (!isRecord(value)) return false;
  return (
    typeof value.available === "boolean" &&
    typeof value.blocked === "boolean" &&
    typeof value.controllerAttached === "boolean" &&
    typeof value.deviceAttached === "boolean" &&
    isNonNegativeSafeInteger(value.tickCount) &&
    isNonNegativeSafeInteger(value.actionsForwarded) &&
    isNonNegativeSafeInteger(value.completionsApplied) &&
    isNonNegativeSafeInteger(value.pendingCompletions) &&
    typeof value.irqLevel === "boolean" &&
    isNonNegativeSafeInteger(value.usbSts) &&
    typeof value.usbStsUsbInt === "boolean" &&
    typeof value.usbStsUsbErrInt === "boolean" &&
    typeof value.usbStsPcd === "boolean" &&
    (value.lastAction === null || isUsbHostAction(value.lastAction)) &&
    (value.lastCompletion === null || isUsbHostCompletion(value.lastCompletion)) &&
    isNullableBytes(value.deviceDescriptor) &&
    isNullableBytes(value.configDescriptor) &&
    isNullableString(value.lastError)
  );
}

export function isUsbEhciHarnessStatusMessage(value: unknown): value is UsbEhciHarnessStatusMessage {
  if (!isRecord(value) || value.type !== "usb.ehciHarness.status") return false;
  return isWebUsbEhciHarnessRuntimeSnapshot(value.snapshot);
}

// -------------------------------------------------------------------------------------------------
// Harness interface (wasm-bindgen export surface)
// -------------------------------------------------------------------------------------------------

export type WebUsbEhciPassthroughHarnessLike = {
  attach_controller(): void;
  detach_controller(): void;
  attach_device(): void;
  detach_device(): void;
  cmd_get_device_descriptor(): void;
  cmd_get_config_descriptor(): void;
  clear_usbsts(bits: number): void;

  tick(): void;
  drain_actions(): unknown;
  push_completion(completion: UsbHostCompletion): void;

  controller_attached(): boolean;
  device_attached(): boolean;
  usbsts(): number;
  irq_level(): boolean;
  last_error(): unknown;

  free(): void;
};

export type UsbBrokerPortLike = Pick<MessagePort, "addEventListener" | "removeEventListener" | "postMessage"> & {
  start?: () => void;
};

type PendingItem = { action: UsbHostAction };

// EHCI harness presents a high-speed view, so we must not apply the UHCI-only
// OTHER_SPEED_CONFIGURATION translation hack.
const EHCI_HARNESS_USB_PROXY_ACTION_OPTIONS: UsbProxyActionOptions = { translateOtherSpeedConfigurationDescriptor: false };

function formatError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
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
    if (value.byteLength > MAX_USB_PROXY_BYTES) {
      throw new Error(`Expected byte payload <= ${MAX_USB_PROXY_BYTES} bytes, got ${value.byteLength}`);
    }
    if (value.buffer instanceof ArrayBuffer && value.byteOffset === 0 && value.byteLength === value.buffer.byteLength) return value;
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
    const out = new Uint8Array(value.byteLength);
    out.set(new Uint8Array(value));
    return out;
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_USB_PROXY_BYTES) {
      throw new Error(`Expected byte payload <= ${MAX_USB_PROXY_BYTES} bytes, got ${value.length}`);
    }
    if (!value.every((v) => typeof v === "number" && Number.isFinite(v) && Number.isInteger(v) && v >= 0 && v <= 0xff)) {
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

function safeFree(obj: WebUsbEhciPassthroughHarnessLike | null): void {
  if (!obj) return;
  try {
    obj.free();
  } catch {
    // ignore
  }
}

function normalizeWebUsbEhciPassthroughHarnessLike(harness: WebUsbEhciPassthroughHarnessLike): WebUsbEhciPassthroughHarnessLike {
  const anyHarness = harness as unknown as Record<string, unknown>;

  // Backwards compatibility: accept camelCase exports and always invoke extracted methods via
  // `.call(harness, ...)` to avoid wasm-bindgen `this` binding pitfalls.
  const attachController = anyHarness.attach_controller ?? anyHarness.attachController;
  const detachController = anyHarness.detach_controller ?? anyHarness.detachController;
  const attachDevice = anyHarness.attach_device ?? anyHarness.attachDevice;
  const detachDevice = anyHarness.detach_device ?? anyHarness.detachDevice;
  const cmdGetDeviceDescriptor = anyHarness.cmd_get_device_descriptor ?? anyHarness.cmdGetDeviceDescriptor;
  const cmdGetConfigDescriptor = anyHarness.cmd_get_config_descriptor ?? anyHarness.cmdGetConfigDescriptor;
  const clearUsbSts = anyHarness.clear_usbsts ?? anyHarness.clearUsbsts ?? anyHarness.clearUsbSts;
  const tick = anyHarness.tick;
  const drainActions = anyHarness.drain_actions ?? anyHarness.drainActions;
  const pushCompletion = anyHarness.push_completion ?? anyHarness.pushCompletion;
  const controllerAttached = anyHarness.controller_attached ?? anyHarness.controllerAttached;
  const deviceAttached = anyHarness.device_attached ?? anyHarness.deviceAttached;
  const usbSts = anyHarness.usbsts ?? anyHarness.usbSts;
  const irqLevel = anyHarness.irq_level ?? anyHarness.irqLevel;
  const lastError = anyHarness.last_error ?? anyHarness.lastError;
  const free = anyHarness.free;

  if (typeof attachController !== "function") throw new Error("WebUsbEhciPassthroughHarness missing attach_controller/attachController export.");
  if (typeof detachController !== "function") throw new Error("WebUsbEhciPassthroughHarness missing detach_controller/detachController export.");
  if (typeof attachDevice !== "function") throw new Error("WebUsbEhciPassthroughHarness missing attach_device/attachDevice export.");
  if (typeof detachDevice !== "function") throw new Error("WebUsbEhciPassthroughHarness missing detach_device/detachDevice export.");
  if (typeof cmdGetDeviceDescriptor !== "function")
    throw new Error("WebUsbEhciPassthroughHarness missing cmd_get_device_descriptor/cmdGetDeviceDescriptor export.");
  if (typeof cmdGetConfigDescriptor !== "function")
    throw new Error("WebUsbEhciPassthroughHarness missing cmd_get_config_descriptor/cmdGetConfigDescriptor export.");
  if (typeof clearUsbSts !== "function") throw new Error("WebUsbEhciPassthroughHarness missing clear_usbsts/clearUsbsts export.");
  if (typeof tick !== "function") throw new Error("WebUsbEhciPassthroughHarness missing tick() export.");
  if (typeof drainActions !== "function") throw new Error("WebUsbEhciPassthroughHarness missing drain_actions/drainActions export.");
  if (typeof pushCompletion !== "function") throw new Error("WebUsbEhciPassthroughHarness missing push_completion/pushCompletion export.");
  if (typeof controllerAttached !== "function")
    throw new Error("WebUsbEhciPassthroughHarness missing controller_attached/controllerAttached export.");
  if (typeof deviceAttached !== "function")
    throw new Error("WebUsbEhciPassthroughHarness missing device_attached/deviceAttached export.");
  if (typeof usbSts !== "function") throw new Error("WebUsbEhciPassthroughHarness missing usbsts/usbSts export.");
  if (typeof irqLevel !== "function") throw new Error("WebUsbEhciPassthroughHarness missing irq_level/irqLevel export.");
  if (typeof lastError !== "function") throw new Error("WebUsbEhciPassthroughHarness missing last_error/lastError export.");
  if (typeof free !== "function") throw new Error("WebUsbEhciPassthroughHarness missing free() export.");

  return {
    attach_controller: () => {
      (attachController as () => void).call(harness);
    },
    detach_controller: () => {
      (detachController as () => void).call(harness);
    },
    attach_device: () => {
      (attachDevice as () => void).call(harness);
    },
    detach_device: () => {
      (detachDevice as () => void).call(harness);
    },
    cmd_get_device_descriptor: () => {
      (cmdGetDeviceDescriptor as () => void).call(harness);
    },
    cmd_get_config_descriptor: () => {
      (cmdGetConfigDescriptor as () => void).call(harness);
    },
    clear_usbsts: (bits) => {
      (clearUsbSts as (bits: number) => void).call(harness, bits >>> 0);
    },
    tick: () => {
      (tick as () => void).call(harness);
    },
    drain_actions: () => {
      return (drainActions as () => unknown).call(harness);
    },
    push_completion: (completion) => {
      (pushCompletion as (completion: UsbHostCompletion) => void).call(harness, completion);
    },
    controller_attached: () => {
      return Boolean((controllerAttached as () => unknown).call(harness));
    },
    device_attached: () => {
      return Boolean((deviceAttached as () => unknown).call(harness));
    },
    usbsts: () => {
      return Number((usbSts as () => unknown).call(harness)) >>> 0;
    },
    irq_level: () => {
      return Boolean((irqLevel as () => unknown).call(harness));
    },
    last_error: () => {
      return (lastError as () => unknown).call(harness);
    },
    free: () => {
      (free as () => void).call(harness);
    },
  };
}

function parseNullableString(value: unknown): string | null {
  return typeof value === "string" ? value : value === null ? null : null;
}

export class WebUsbEhciHarnessRuntime {
  readonly #createHarness: () => WebUsbEhciPassthroughHarnessLike;
  readonly #port: UsbBrokerPortLike;
  readonly #onUpdate?: (snapshot: WebUsbEhciHarnessRuntimeSnapshot) => void;

  #blocked = true;
  #harness: WebUsbEhciPassthroughHarnessLike | null = null;

  #nextBrokerId = 2_100_000_000;
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
    createHarness: () => WebUsbEhciPassthroughHarnessLike;
    port: UsbBrokerPortLike;
    onUpdate?: (snapshot: WebUsbEhciHarnessRuntimeSnapshot) => void;
    initiallyBlocked?: boolean;
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
    (this.#port as unknown as { start?: () => void }).start?.();

    // Request SAB rings from the broker (best-effort).
    try {
      this.#port.postMessage({ type: "usb.ringAttachRequest" } satisfies UsbRingAttachRequestMessage);
    } catch {
      // ignore
    }

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

  destroy(): void {
    this.detachRings();
    safeFree(this.#harness);
    this.#harness = null;
    this.#port.removeEventListener("message", this.#onMessage);
  }

  attachController(): void {
    this.ensureHarness();
    try {
      this.#harness?.attach_controller();
      this.resetRuntimeState({ keepHarness: true, keepErrors: false });
    } catch (err) {
      this.#lastError = `attach_controller failed: ${formatError(err)}`;
    }
    this.emitUpdate();
  }

  detachController(): void {
    try {
      this.#harness?.detach_controller();
    } catch {
      // ignore
    }
    this.resetRuntimeState({ keepHarness: true });
    this.emitUpdate();
  }

  attachDevice(): void {
    this.ensureHarness();
    try {
      this.#harness?.attach_device();
      this.#lastError = null;
    } catch (err) {
      this.#lastError = `attach_device failed: ${formatError(err)}`;
    }
    this.emitUpdate();
  }

  detachDevice(): void {
    try {
      this.#harness?.detach_device();
    } catch {
      // ignore
    }
    this.cancelPending("EHCI harness device detached");
    this.#capture.deviceDescriptor = null;
    this.#capture.configDescriptor = null;
    this.emitUpdate();
  }

  runGetDeviceDescriptor(): void {
    this.ensureHarness();
    try {
      this.#harness?.cmd_get_device_descriptor();
      this.#lastError = null;
    } catch (err) {
      this.#lastError = `cmd_get_device_descriptor failed: ${formatError(err)}`;
    }
    this.emitUpdate();
  }

  runGetConfigDescriptor(): void {
    this.ensureHarness();
    try {
      this.#harness?.cmd_get_config_descriptor();
      this.#lastError = null;
    } catch (err) {
      this.#lastError = `cmd_get_config_descriptor failed: ${formatError(err)}`;
    }
    this.emitUpdate();
  }

  clearUsbSts(bits: number): void {
    this.ensureHarness();
    try {
      this.#harness?.clear_usbsts(bits >>> 0);
    } catch (err) {
      this.#lastError = `clear_usbsts failed: ${formatError(err)}`;
    }
    this.emitUpdate();
  }

  pollOnce(): void {
    if (this.#blocked) return;
    const harness = this.#harness;
    if (!harness) return;

    try {
      harness.tick();
      this.#tickCount += 1;
    } catch (err) {
      this.#lastError = `harness.tick() failed: ${formatError(err)}`;
      this.emitUpdate();
      return;
    }

    // Surface harness-reported errors.
    try {
      const rawErr = harness.last_error();
      const errStr = parseNullableString(rawErr);
      if (errStr) this.#lastError = errStr;
    } catch {
      // ignore
    }

    let drained: unknown;
    try {
      drained = harness.drain_actions();
    } catch (err) {
      this.#lastError = `harness.drain_actions() failed: ${formatError(err)}`;
      this.emitUpdate();
      return;
    }

    let actions: UsbHostAction[];
    try {
      actions = asUsbHostActions(drained);
    } catch (err) {
      this.#lastError = formatError(err);
      this.emitUpdate();
      return;
    }
    if (actions.length === 0) return;

    let changed = false;
    for (const action of actions) {
      const { id } = action;

      if (this.#pendingHarnessIds.has(id)) {
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
        this.#lastError = `WebUsbEhciHarnessRuntime ran out of valid broker action IDs (next=${this.#nextBrokerId})`;
        this.emitUpdate();
        return;
      }
      const brokerAction = rewriteActionId(action, brokerId);

      const actionRing = this.#actionRing;
      if (actionRing) {
        try {
          if (actionRing.pushAction(brokerAction, EHCI_HARNESS_USB_PROXY_ACTION_OPTIONS)) {
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

      const msg: UsbActionMessage = { type: "usb.action", action: brokerAction, options: EHCI_HARNESS_USB_PROXY_ACTION_OPTIONS };
      try {
        this.#port.postMessage(msg);
      } catch (err) {
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

  getSnapshot(): WebUsbEhciHarnessRuntimeSnapshot {
    const harness = this.#harness;
    let controllerAttached = false;
    let deviceAttached = false;
    let usbSts = 0;
    let irqLevel = false;

    if (harness) {
      try {
        controllerAttached = harness.controller_attached();
      } catch {
        // ignore
      }
      try {
        deviceAttached = harness.device_attached();
      } catch {
        // ignore
      }
      try {
        usbSts = harness.usbsts() >>> 0;
      } catch {
        usbSts = 0;
      }
      try {
        irqLevel = !!harness.irq_level();
      } catch {
        irqLevel = false;
      }
    }

    const usbStsUsbInt = (usbSts & 0x1) !== 0;
    const usbStsUsbErrInt = (usbSts & 0x2) !== 0;
    const usbStsPcd = (usbSts & 0x4) !== 0;

    return {
      available: true,
      blocked: this.#blocked,
      controllerAttached,
      deviceAttached,
      tickCount: this.#tickCount,
      actionsForwarded: this.#actionsForwarded,
      completionsApplied: this.#completionsApplied,
      pendingCompletions: this.#pending.size,
      irqLevel,
      usbSts,
      usbStsUsbInt,
      usbStsUsbErrInt,
      usbStsPcd,
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
      this.#harness = normalizeWebUsbEhciPassthroughHarnessLike(this.#createHarness());
    } catch (err) {
      this.#lastError = `Failed to construct WebUsbEhciPassthroughHarness: ${formatError(err)}`;
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
    this.cancelPending(msg.error ?? "WebUSB device not selected.");
    this.emitUpdate();
  }

  private emitUpdate(): void {
    if (!this.#onUpdate) return;
    try {
      this.#onUpdate(this.getSnapshot());
    } catch {
      // ignore
    }
  }

  private resetRuntimeState(options: { keepHarness?: boolean; keepErrors?: boolean } = {}): void {
    this.cancelPending(options.keepErrors ? this.#lastError ?? "reset" : "reset");
    this.#pending.clear();
    this.#pendingHarnessIds.clear();
    this.#tickCount = 0;
    this.#actionsForwarded = 0;
    this.#completionsApplied = 0;
    this.#lastAction = null;
    this.#lastCompletion = null;
    this.#capture.deviceDescriptor = null;
    this.#capture.configDescriptor = null;
    if (!options.keepErrors) this.#lastError = null;

    if (!options.keepHarness) {
      safeFree(this.#harness);
      this.#harness = null;
    }
  }

  private attachRings(msg: UsbRingAttachMessage): void {
    const currentActionBuf = this.#actionRingBuffer;
    const currentCompletionBuf = this.#completionRingBuffer;
    if (currentActionBuf === msg.actionRing && currentCompletionBuf === msg.completionRing) return;

    if (this.#actionRing || this.#completionRingUnsubscribe) {
      this.detachRings();
    }

    try {
      this.#actionRing = new UsbProxyRing(msg.actionRing);
      this.#actionRingBuffer = msg.actionRing;
      this.#completionRingBuffer = msg.completionRing;
      this.#completionRingUnsubscribe = subscribeUsbProxyCompletionRing(msg.completionRing, (completion) => this.handleCompletion(completion), {
        onError: (err) => this.handleRingFailure(`USB completion ring pop failed: ${formatError(err)}`),
      });
    } catch (err) {
      this.#lastError = `Failed to attach USB proxy rings: ${formatError(err)}`;
      this.detachRings();
      return;
    }

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

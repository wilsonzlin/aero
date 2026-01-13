import type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";

export type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";

export const MAX_USB_PROXY_BYTES = 4 * 1024 * 1024;

export type UsbProxyActionOptions = {
  /**
   * When true (default), attempt to fetch `OTHER_SPEED_CONFIGURATION` (0x07) and rewrite it into a
   * `CONFIGURATION` (0x02) descriptor blob when the guest requests
   * `GET_DESCRIPTOR(CONFIGURATION)`.
   *
   * This is a UHCI/full-speed compatibility hack for WebUSB passthrough:
   * high-speed devices often return a configuration descriptor with high-speed max packet sizes,
   * which a USB 1.1 guest cannot use.
   *
   * When a passthrough device is attached to an EHCI/xHCI controller (high-speed view), this must
   * be disabled so the guest sees the device's high-speed descriptors unmodified.
   */
  translateOtherSpeedConfigurationDescriptor?: boolean;
};

export type UsbActionMessage = { type: "usb.action"; action: UsbHostAction; options?: UsbProxyActionOptions };
export type UsbCompletionMessage = { type: "usb.completion"; completion: UsbHostCompletion };
export type UsbRingAttachMessage = {
  type: "usb.ringAttach";
  actionRing: SharedArrayBuffer;
  completionRing: SharedArrayBuffer;
};
/**
 * Request the SharedArrayBuffer USB proxy rings from the broker.
 *
 * This is useful when a worker-side runtime starts after the broker already posted
 * `usb.ringAttach` (e.g. WASM initialized late). Older brokers may ignore this
 * message; the runtime should fall back to `postMessage`-based proxying.
 */
export type UsbRingAttachRequestMessage = { type: "usb.ringAttachRequest" };
/**
 * Disable the SharedArrayBuffer USB proxy rings for this port.
 *
 * The SAB fast path is an optimization; runtimes must be able to fall back to `postMessage`-based
 * proxying at any time (e.g. if a ring becomes corrupted).
 */
export type UsbRingDetachMessage = { type: "usb.ringDetach"; reason?: string };
export type UsbSelectDeviceMessage = { type: "usb.selectDevice"; filters?: USBDeviceFilter[] };
/**
 * Request the current `usb.selected` state from the broker.
 *
 * This is useful when a worker-side runtime starts in a blocked state and may
 * have missed an earlier `usb.selected ok:true` broadcast (e.g. WASM finished
 * initializing after the user selected a device).
 */
export type UsbQuerySelectedMessage = { type: "usb.querySelected" };
export type UsbSelectedMessage =
  | { type: "usb.selected"; ok: true; info: { vendorId: number; productId: number; productName?: string } }
  | { type: "usb.selected"; ok: false; error?: string };

export type UsbGuestWebUsbControllerKind = "xhci" | "ehci" | "uhci";

export type UsbGuestControllerMode = UsbGuestWebUsbControllerKind;

/**
 * Select which guest-visible USB controller should host the WebUSB passthrough device.
 *
 * - `"uhci"`: full-speed (USB 1.1) controller path (default).
 * - `"ehci"`: high-speed (USB 2.0) controller path (when available).
 * - `"xhci"`: high-speed/superspeed controller path (when available).
 */
export type UsbGuestControllerModeMessage = { type: "usb.guest.controller"; mode: UsbGuestControllerMode };

export type UsbGuestWebUsbSnapshot = {
  /** WASM exports are present and the guest-visible passthrough device can be attached. */
  available: boolean;
  /** Whether the guest-visible WebUSB proxy device is currently attached to the active guest controller. */
  attached: boolean;
  /** `true` when `usb.selected ok:false` is active (no physical device selected). */
  blocked: boolean;
  /**
   * Which guest-visible controller backend is being used for WebUSB passthrough.
   *
   * Optional for back-compat with older workers/brokers.
   */
  controllerKind?: UsbGuestWebUsbControllerKind;
  /** Root port index the passthrough device attaches to (controller-specific, 0-based). */
  rootPort: number;
  /** Optional error text (e.g. attach failure). */
  lastError: string | null;
};

export type UsbGuestWebUsbStatusMessage = { type: "usb.guest.status"; snapshot: UsbGuestWebUsbSnapshot };

export type UsbProxyMessage =
  | UsbActionMessage
  | UsbCompletionMessage
  | UsbRingAttachMessage
  | UsbRingAttachRequestMessage
  | UsbRingDetachMessage
  | UsbSelectDeviceMessage
  | UsbQuerySelectedMessage
  | UsbSelectedMessage
  | UsbGuestControllerModeMessage
  | UsbGuestWebUsbStatusMessage;

function transferablesForBytes(bytes: Uint8Array): Transferable[] | undefined {
  // Only `ArrayBuffer` instances are transferable. `SharedArrayBuffer` can be structured-cloned but not transferred.
  if (!(bytes.buffer instanceof ArrayBuffer)) return undefined;
  // Only transfer when the view covers the full buffer so we don't accidentally detach
  // unrelated data from the sender.
  if (bytes.byteOffset !== 0 || bytes.byteLength !== bytes.buffer.byteLength) return undefined;
  return [bytes.buffer];
}

export function getTransferablesForUsbActionMessage(msg: UsbActionMessage): Transferable[] | undefined {
  const action = msg.action;
  switch (action.kind) {
    case "controlOut":
    case "bulkOut":
      return transferablesForBytes(action.data);
    default:
      return undefined;
  }
}

export function getTransferablesForUsbCompletionMessage(msg: UsbCompletionMessage): Transferable[] | undefined {
  const completion = msg.completion;
  switch (completion.kind) {
    case "controlIn":
    case "bulkIn":
      return completion.status === "success" ? transferablesForBytes(completion.data) : undefined;
    default:
      return undefined;
  }
}

export function getTransferablesForUsbProxyMessage(msg: UsbProxyMessage): Transferable[] | undefined {
  switch (msg.type) {
    case "usb.action":
      return getTransferablesForUsbActionMessage(msg);
    case "usb.completion":
      return getTransferablesForUsbCompletionMessage(msg);
    default:
      return undefined;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value);
}

function isUint8(value: unknown): value is number {
  return isSafeInteger(value) && value >= 0 && value <= 0xff;
}

function isUint16(value: unknown): value is number {
  return isSafeInteger(value) && value >= 0 && value <= 0xffff;
}

function isUint32(value: unknown): value is number {
  return isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff;
}

function isUsbEndpointAddress(value: unknown): value is number {
  if (!isUint8(value)) return false;
  // `value` should be a USB endpoint address, not just an endpoint number:
  // - bit7 = direction (IN=1, OUT=0)
  // - bits4..6 must be 0 (endpoint numbers are 0..=15)
  // - endpoint 0 is the control pipe and should not be used for bulk/interrupt actions
  return (value & 0x70) === 0 && (value & 0x0f) !== 0;
}

function isUsbInEndpointAddress(value: unknown): value is number {
  return isUsbEndpointAddress(value) && (value & 0x80) !== 0;
}

function isUsbOutEndpointAddress(value: unknown): value is number {
  return isUsbEndpointAddress(value) && (value & 0x80) === 0;
}

function isUsbBytePayload(value: unknown): value is Uint8Array {
  if (!(value instanceof Uint8Array)) return false;
  if (value.byteLength > MAX_USB_PROXY_BYTES) return false;
  if (!(value.buffer instanceof ArrayBuffer)) return false;
  if (value.buffer.byteLength > MAX_USB_PROXY_BYTES) return false;
  return true;
}

export function isUsbSetupPacket(value: unknown): value is SetupPacket {
  if (!isRecord(value)) return false;
  return (
    isUint8(value.bmRequestType) &&
    isUint8(value.bRequest) &&
    isUint16(value.wValue) &&
    isUint16(value.wIndex) &&
    isUint16(value.wLength)
  );
}

export function isUsbHostAction(value: unknown): value is UsbHostAction {
  if (!isRecord(value)) return false;
  if (!isUint32(value.id) || typeof value.kind !== "string") return false;

  switch (value.kind) {
    case "controlIn":
      return isUsbSetupPacket(value.setup);
    case "controlOut":
      return (
        isUsbSetupPacket(value.setup) &&
        isUsbBytePayload(value.data) &&
        // The control-transfer setup packet includes the expected data-stage size. For correctness,
        // require that the payload length matches so the broker can choose the correct WebUSB call
        // signature (omit the data argument when wLength is 0).
        value.data.byteLength === value.setup.wLength
      );
    case "bulkIn":
      return isUsbInEndpointAddress(value.endpoint) && isUint32(value.length) && value.length <= MAX_USB_PROXY_BYTES;
    case "bulkOut":
      return isUsbOutEndpointAddress(value.endpoint) && isUsbBytePayload(value.data);
    default:
      return false;
  }
}

export function isUsbHostCompletion(value: unknown): value is UsbHostCompletion {
  if (!isRecord(value)) return false;
  if (!isUint32(value.id) || typeof value.kind !== "string" || typeof value.status !== "string") return false;

  switch (value.kind) {
    case "controlIn":
    case "bulkIn":
      if (value.status === "success") return isUsbBytePayload(value.data);
      if (value.status === "stall") return true;
      if (value.status === "error") return typeof value.message === "string";
      return false;
    case "controlOut":
    case "bulkOut":
      if (value.status === "success") return isUint32(value.bytesWritten);
      if (value.status === "stall") return true;
      if (value.status === "error") return typeof value.message === "string";
      return false;
    default:
      return false;
  }
}

export function isUsbActionMessage(value: unknown): value is UsbActionMessage {
  if (!isRecord(value) || value.type !== "usb.action") return false;
  return isUsbHostAction(value.action);
}

export function isUsbCompletionMessage(value: unknown): value is UsbCompletionMessage {
  if (!isRecord(value) || value.type !== "usb.completion") return false;
  return isUsbHostCompletion(value.completion);
}

export function isUsbRingAttachMessage(value: unknown): value is UsbRingAttachMessage {
  if (!isRecord(value) || value.type !== "usb.ringAttach") return false;
  if (typeof SharedArrayBuffer === "undefined") return false;
  if (!(value.actionRing instanceof SharedArrayBuffer)) return false;
  if (!(value.completionRing instanceof SharedArrayBuffer)) return false;
  return true;
}

export function isUsbRingAttachRequestMessage(value: unknown): value is UsbRingAttachRequestMessage {
  return isRecord(value) && value.type === "usb.ringAttachRequest";
}

export function isUsbRingDetachMessage(value: unknown): value is UsbRingDetachMessage {
  if (!isRecord(value) || value.type !== "usb.ringDetach") return false;
  if (value.reason === undefined) return true;
  return typeof value.reason === "string";
}

export function isUsbSelectDeviceMessage(value: unknown): value is UsbSelectDeviceMessage {
  if (!isRecord(value) || value.type !== "usb.selectDevice") return false;
  if (value.filters === undefined) return true;
  return Array.isArray(value.filters);
}

export function isUsbQuerySelectedMessage(value: unknown): value is UsbQuerySelectedMessage {
  return isRecord(value) && value.type === "usb.querySelected";
}

export function isUsbSelectedMessage(value: unknown): value is UsbSelectedMessage {
  if (!isRecord(value) || value.type !== "usb.selected") return false;
  if (typeof value.ok !== "boolean") return false;
  if (value.ok) {
    if (value.error !== undefined) return false;
    if (!isRecord(value.info)) return false;
    if (!isUint16(value.info.vendorId) || !isUint16(value.info.productId)) return false;
    if (value.info.productName !== undefined && typeof value.info.productName !== "string") return false;
    return true;
  }
  if (value.info !== undefined) return false;
  if (value.error !== undefined && typeof value.error !== "string") return false;
  return true;
}

export function isUsbGuestWebUsbStatusMessage(value: unknown): value is UsbGuestWebUsbStatusMessage {
  if (!isRecord(value) || value.type !== "usb.guest.status") return false;
  if (!isRecord(value.snapshot)) return false;
  const snap = value.snapshot;
  if (typeof snap.available !== "boolean") return false;
  if (typeof snap.attached !== "boolean") return false;
  if (typeof snap.blocked !== "boolean") return false;
  if (snap.controllerKind !== undefined) {
    if (snap.controllerKind !== "xhci" && snap.controllerKind !== "ehci" && snap.controllerKind !== "uhci") return false;
  }
  if (!isUint32(snap.rootPort)) return false;
  if (snap.lastError !== null && typeof snap.lastError !== "string") return false;
  return true;
}

export function isUsbGuestControllerModeMessage(value: unknown): value is UsbGuestControllerModeMessage {
  if (!isRecord(value) || value.type !== "usb.guest.controller") return false;
  return value.mode === "uhci" || value.mode === "ehci" || value.mode === "xhci";
}

export function isUsbProxyMessage(value: unknown): value is UsbProxyMessage {
  return (
    isUsbActionMessage(value) ||
    isUsbCompletionMessage(value) ||
    isUsbRingAttachMessage(value) ||
    isUsbRingAttachRequestMessage(value) ||
    isUsbRingDetachMessage(value) ||
    isUsbSelectDeviceMessage(value) ||
    isUsbQuerySelectedMessage(value) ||
    isUsbSelectedMessage(value) ||
    isUsbGuestControllerModeMessage(value) ||
    isUsbGuestWebUsbStatusMessage(value)
  );
}

export function usbErrorCompletion(kind: UsbHostAction["kind"], id: number, message: string): UsbHostCompletion {
  // Keep this helper here (instead of in webusb_backend.ts) so message senders can
  // construct protocol-compliant completions even when WebUSB is unavailable.
  switch (kind) {
    case "controlIn":
    case "bulkIn":
      return { kind, id, status: "error", message } satisfies UsbHostCompletion;
    case "controlOut":
    case "bulkOut":
      return { kind, id, status: "error", message } satisfies UsbHostCompletion;
    default: {
      const neverKind: never = kind;
      throw new Error(`Unknown USB action kind: ${String(neverKind)}`);
    }
  }
}

import type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";

export type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";

export type UsbActionMessage = { type: "usb.action"; action: UsbHostAction };
export type UsbCompletionMessage = { type: "usb.completion"; completion: UsbHostCompletion };
export type UsbSelectDeviceMessage = { type: "usb.selectDevice"; filters?: USBDeviceFilter[] };
export type UsbSelectedMessage = {
  type: "usb.selected";
  ok: boolean;
  error?: string;
  info?: { vendorId: number; productId: number; productName?: string };
};

export type UsbProxyMessage = UsbActionMessage | UsbCompletionMessage | UsbSelectDeviceMessage | UsbSelectedMessage;

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
      return isUsbSetupPacket(value.setup) && value.data instanceof Uint8Array;
    case "bulkIn":
      return isUint8(value.endpoint) && isUint32(value.length);
    case "bulkOut":
      return isUint8(value.endpoint) && value.data instanceof Uint8Array;
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
      if (value.status === "success") return value.data instanceof Uint8Array;
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

export function isUsbSelectDeviceMessage(value: unknown): value is UsbSelectDeviceMessage {
  if (!isRecord(value) || value.type !== "usb.selectDevice") return false;
  if (value.filters === undefined) return true;
  return Array.isArray(value.filters);
}

export function isUsbSelectedMessage(value: unknown): value is UsbSelectedMessage {
  if (!isRecord(value) || value.type !== "usb.selected") return false;
  if (typeof value.ok !== "boolean") return false;
  if (value.ok) {
    if (value.info === undefined) return false;
    if (!isRecord(value.info)) return false;
    return isUint16(value.info.vendorId) && isUint16(value.info.productId);
  }
  if (value.error !== undefined && typeof value.error !== "string") return false;
  return true;
}

export function isUsbProxyMessage(value: unknown): value is UsbProxyMessage {
  return (
    isUsbActionMessage(value) || isUsbCompletionMessage(value) || isUsbSelectDeviceMessage(value) || isUsbSelectedMessage(value)
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

export interface UsbSetupPacket {
  bmRequestType: number;
  bRequest: number;
  wValue: number;
  wIndex: number;
  wLength: number;
}

export type UsbHostAction =
  | {
      kind: "controlIn";
      id: number;
      setup: UsbSetupPacket;
    }
  | {
      kind: "controlOut";
      id: number;
      setup: UsbSetupPacket;
      data: Uint8Array;
    }
  | {
      kind: "bulkIn";
      id: number;
      ep: number;
      length: number;
    }
  | {
      kind: "bulkOut";
      id: number;
      ep: number;
      data: Uint8Array;
    };

export type UsbHostCompletion =
  | {
      kind: "okIn";
      id: number;
      data: Uint8Array;
    }
  | {
      kind: "okOut";
      id: number;
      bytesWritten: number;
    }
  | {
      kind: "stall";
      id: number;
    }
  | {
      kind: "error";
      id: number;
      error: string;
    };

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

export function isUsbSetupPacket(value: unknown): value is UsbSetupPacket {
  if (!isRecord(value)) return false;
  return (
    isFiniteNumber(value.bmRequestType) &&
    isFiniteNumber(value.bRequest) &&
    isFiniteNumber(value.wValue) &&
    isFiniteNumber(value.wIndex) &&
    isFiniteNumber(value.wLength)
  );
}

export function isUsbHostAction(value: unknown): value is UsbHostAction {
  if (!isRecord(value)) return false;
  if (!isFiniteNumber(value.id) || typeof value.kind !== "string") return false;

  switch (value.kind) {
    case "controlIn":
      return isUsbSetupPacket(value.setup);
    case "controlOut":
      return isUsbSetupPacket(value.setup) && value.data instanceof Uint8Array;
    case "bulkIn":
      return isFiniteNumber(value.ep) && isFiniteNumber(value.length);
    case "bulkOut":
      return isFiniteNumber(value.ep) && value.data instanceof Uint8Array;
    default:
      return false;
  }
}

export function isUsbHostCompletion(value: unknown): value is UsbHostCompletion {
  if (!isRecord(value)) return false;
  if (!isFiniteNumber(value.id) || typeof value.kind !== "string") return false;

  switch (value.kind) {
    case "okIn":
      return value.data instanceof Uint8Array;
    case "okOut":
      return isFiniteNumber(value.bytesWritten);
    case "stall":
      return true;
    case "error":
      return typeof value.error === "string";
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
    return isFiniteNumber(value.info.vendorId) && isFiniteNumber(value.info.productId);
  }
  if (value.error !== undefined && typeof value.error !== "string") return false;
  return true;
}

export function isUsbProxyMessage(value: unknown): value is UsbProxyMessage {
  return (
    isUsbActionMessage(value) || isUsbCompletionMessage(value) || isUsbSelectDeviceMessage(value) || isUsbSelectedMessage(value)
  );
}

export function usbErrorCompletion(id: number, error: string): UsbHostCompletion {
  return { kind: "error", id, error };
}


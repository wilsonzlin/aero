// WebUSB backend for USB passthrough.
//
// This module provides a small, JS-friendly executor for the Rust-side
// `UsbHostAction` / `UsbHostCompletion` contract.

export interface SetupPacket {
  bmRequestType: number;
  bRequest: number;
  wValue: number;
  wIndex: number;
  wLength: number;
}

export type UsbHostAction =
  | { kind: "controlIn"; id: number; setup: SetupPacket }
  | { kind: "controlOut"; id: number; setup: SetupPacket; data: Uint8Array }
  | { kind: "bulkIn"; id: number; endpoint: number; length: number }
  | { kind: "bulkOut"; id: number; endpoint: number; data: Uint8Array };

export type UsbHostCompletion =
  | { kind: "controlIn"; id: number; status: "success"; data: Uint8Array }
  | { kind: "controlIn"; id: number; status: "stall" }
  | { kind: "controlIn"; id: number; status: "error"; message: string }
  | { kind: "controlOut"; id: number; status: "success"; bytesWritten: number }
  | { kind: "controlOut"; id: number; status: "stall" }
  | { kind: "controlOut"; id: number; status: "error"; message: string }
  | { kind: "bulkIn"; id: number; status: "success"; data: Uint8Array }
  | { kind: "bulkIn"; id: number; status: "stall" }
  | { kind: "bulkIn"; id: number; status: "error"; message: string }
  | { kind: "bulkOut"; id: number; status: "success"; bytesWritten: number }
  | { kind: "bulkOut"; id: number; status: "stall" }
  | { kind: "bulkOut"; id: number; status: "error"; message: string };

export function isWebUsbSupported(): boolean {
  return typeof (globalThis as any).navigator !== "undefined" && !!(globalThis as any).navigator?.usb;
}

function assertWebUsbSupported(): void {
  if (isWebUsbSupported()) return;
  throw new Error(
    [
      "WebUSB is not supported in this environment (navigator.usb is unavailable).",
      "Use a Chromium-based browser over HTTPS (or localhost) and ensure WebUSB is enabled.",
    ].join("\n"),
  );
}

function formatThrownError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

export type BmRequestDirection = "hostToDevice" | "deviceToHost";

export interface ParsedBmRequestType {
  direction: BmRequestDirection;
  requestType: USBRequestType;
  recipient: USBRecipient;
}

export function parseBmRequestType(bmRequestType: number): ParsedBmRequestType {
  const value = bmRequestType & 0xff;

  const direction: BmRequestDirection = (value & 0x80) !== 0 ? "deviceToHost" : "hostToDevice";

  const requestTypeBits = (value >> 5) & 0x03;
  let requestType: USBRequestType;
  switch (requestTypeBits) {
    case 0:
      requestType = "standard";
      break;
    case 1:
      requestType = "class";
      break;
    case 2:
      requestType = "vendor";
      break;
    default:
      throw new Error(`Unsupported bmRequestType request type bits: ${requestTypeBits}`);
  }

  const recipientBits = value & 0x1f;
  let recipient: USBRecipient;
  switch (recipientBits) {
    case 0:
      recipient = "device";
      break;
    case 1:
      recipient = "interface";
      break;
    case 2:
      recipient = "endpoint";
      break;
    case 3:
      recipient = "other";
      break;
    default:
      throw new Error(`Unsupported bmRequestType recipient bits: ${recipientBits}`);
  }

  return { direction, requestType, recipient };
}

export function validateControlTransferDirection(
  kind: "controlIn" | "controlOut",
  bmRequestType: number,
): { ok: true } | { ok: false; message: string } {
  const dir = (bmRequestType & 0x80) !== 0 ? "deviceToHost" : "hostToDevice";
  const expected: BmRequestDirection = kind === "controlIn" ? "deviceToHost" : "hostToDevice";

  if (dir !== expected) {
    return {
      ok: false,
      message: `Invalid bmRequestType direction for ${kind}: expected ${expected}, got ${dir}`,
    };
  }

  return { ok: true };
}

export function dataViewToUint8Array(view: DataView): Uint8Array {
  const src = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array {
  // TypeScript's `BufferSource` type excludes `SharedArrayBuffer` in some lib.dom
  // versions, even though Chromium accepts it for WebUSB calls. Keep the backend
  // strict-friendly by copying when the buffer is shared.
  if (bytes.buffer instanceof ArrayBuffer) return bytes;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

function setupPacketToWebUsbParameters(setup: SetupPacket): USBControlTransferParameters {
  const parsed = parseBmRequestType(setup.bmRequestType);
  return {
    requestType: parsed.requestType,
    recipient: parsed.recipient,
    request: setup.bRequest & 0xff,
    value: setup.wValue & 0xffff,
    index: setup.wIndex & 0xffff,
  };
}

function endpointAddressToEndpointNumber(endpoint: number): number {
  // WebUSB wants the endpoint number (1-15). Rust-side code tends to use the USB
  // endpoint address, where bit 7 is direction and bits 0-3 are the endpoint number.
  return endpoint & 0x0f;
}

function errorCompletion(kind: UsbHostAction["kind"], id: number, message: string): UsbHostCompletion {
  switch (kind) {
    case "controlIn":
      return { kind, id, status: "error", message };
    case "controlOut":
      return { kind, id, status: "error", message };
    case "bulkIn":
      return { kind, id, status: "error", message };
    case "bulkOut":
      return { kind, id, status: "error", message };
  }
}

export class WebUsbBackend {
  private readonly device: USBDevice;
  private claimedConfigurationValue: number | null = null;
  private readonly claimedInterfaces = new Set<number>();

  constructor(device: USBDevice) {
    assertWebUsbSupported();
    this.device = device;
  }

  async ensureOpenAndClaimed(): Promise<void> {
    assertWebUsbSupported();

    if (!this.device.opened) {
      try {
        await this.device.open();
      } catch (err) {
        throw new Error(`Failed to open USB device: ${formatThrownError(err)}`);
      }
    }

    if (!this.device.configuration) {
      const configs = this.device.configurations;
      if (!configs || configs.length === 0) {
        throw new Error("USB device has no configurations to select");
      }
      try {
        await this.device.selectConfiguration(configs[0].configurationValue);
      } catch (err) {
        throw new Error(`Failed to select USB configuration: ${formatThrownError(err)}`);
      }
    }

    const configuration = this.device.configuration;
    if (!configuration) {
      throw new Error("USB device has no active configuration after selection");
    }

    if (this.claimedConfigurationValue !== configuration.configurationValue) {
      this.claimedInterfaces.clear();
      this.claimedConfigurationValue = configuration.configurationValue;
    }

    for (const iface of configuration.interfaces) {
      const ifaceNum = iface.interfaceNumber;
      if (this.claimedInterfaces.has(ifaceNum)) continue;
      try {
        await this.device.claimInterface(ifaceNum);
      } catch (err) {
        throw new Error(`Failed to claim USB interface ${ifaceNum}: ${formatThrownError(err)}`);
      }
      this.claimedInterfaces.add(ifaceNum);
    }
  }

  async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
    assertWebUsbSupported();

    try {
      await this.ensureOpenAndClaimed();
    } catch (err) {
      return errorCompletion(action.kind, action.id, formatThrownError(err));
    }

    try {
      switch (action.kind) {
        case "controlIn": {
          const directionCheck = validateControlTransferDirection("controlIn", action.setup.bmRequestType);
          if (!directionCheck.ok) {
            return { kind: "controlIn", id: action.id, status: "error", message: directionCheck.message };
          }

          const params = setupPacketToWebUsbParameters(action.setup);
          const result = await this.device.controlTransferIn(params, action.setup.wLength & 0xffff);
          if (result.status === "ok") {
            const data = result.data ? dataViewToUint8Array(result.data) : new Uint8Array();
            return { kind: "controlIn", id: action.id, status: "success", data };
          }
          if (result.status === "stall") {
            return { kind: "controlIn", id: action.id, status: "stall" };
          }
          return {
            kind: "controlIn",
            id: action.id,
            status: "error",
            message: `WebUSB controlTransferIn returned status: ${result.status}`,
          };
        }
        case "controlOut": {
          const directionCheck = validateControlTransferDirection("controlOut", action.setup.bmRequestType);
          if (!directionCheck.ok) {
            return { kind: "controlOut", id: action.id, status: "error", message: directionCheck.message };
          }

          const params = setupPacketToWebUsbParameters(action.setup);
          const result = await this.device.controlTransferOut(params, ensureArrayBufferBacked(action.data));
          if (result.status === "ok") {
            return { kind: "controlOut", id: action.id, status: "success", bytesWritten: result.bytesWritten };
          }
          if (result.status === "stall") {
            return { kind: "controlOut", id: action.id, status: "stall" };
          }
          return {
            kind: "controlOut",
            id: action.id,
            status: "error",
            message: `WebUSB controlTransferOut returned status: ${result.status}`,
          };
        }
        case "bulkIn": {
          const ep = endpointAddressToEndpointNumber(action.endpoint);
          const result = await this.device.transferIn(ep, action.length);
          if (result.status === "ok") {
            const data = result.data ? dataViewToUint8Array(result.data) : new Uint8Array();
            return { kind: "bulkIn", id: action.id, status: "success", data };
          }
          if (result.status === "stall") {
            return { kind: "bulkIn", id: action.id, status: "stall" };
          }
          return {
            kind: "bulkIn",
            id: action.id,
            status: "error",
            message: `WebUSB transferIn returned status: ${result.status}`,
          };
        }
        case "bulkOut": {
          const ep = endpointAddressToEndpointNumber(action.endpoint);
          const result = await this.device.transferOut(ep, ensureArrayBufferBacked(action.data));
          if (result.status === "ok") {
            return { kind: "bulkOut", id: action.id, status: "success", bytesWritten: result.bytesWritten };
          }
          if (result.status === "stall") {
            return { kind: "bulkOut", id: action.id, status: "stall" };
          }
          return {
            kind: "bulkOut",
            id: action.id,
            status: "error",
            message: `WebUSB transferOut returned status: ${result.status}`,
          };
        }
      }
    } catch (err) {
      return errorCompletion(action.kind, action.id, formatThrownError(err));
    }
  }
}

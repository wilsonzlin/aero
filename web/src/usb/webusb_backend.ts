// WebUSB backend for USB passthrough.
//
// This module provides a small, JS-friendly executor for the Rust-side
// `UsbHostAction` / `UsbHostCompletion` contract.

import { formatWebUsbError } from "../platform/webusb_troubleshooting";
import { isWebUsbProtectedInterfaceClass } from "../platform/webusb";

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
  return formatWebUsbError(err);
}

function wrapWithCause(message: string, cause: unknown): Error {
  const error = new Error(message);
  // Not all runtimes support the `ErrorOptions` constructor parameter, but
  // attaching `cause` is still useful for debugging and for our WebUSB
  // troubleshooting helper, which can walk `Error.cause` chains.
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (error as any).cause = cause;
  } catch {
    // ignore
  }
  return error;
}

function interfaceIsWebUsbProtected(iface: Pick<USBInterface, "alternates">): boolean {
  const alternates = iface.alternates ?? [];
  if (alternates.length === 0) return false;
  return alternates.every((alt) => isWebUsbProtectedInterfaceClass(alt.interfaceClass));
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

const GET_DESCRIPTOR = 0x06;
const DESCRIPTOR_TYPE_CONFIGURATION = 0x02;
const DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION = 0x07;
const BM_REQUEST_TYPE_DEVICE_TO_HOST_STANDARD_DEVICE = 0x80;

function shouldTranslateConfigurationDescriptor(setup: SetupPacket): boolean {
  return (
    setup.bRequest === GET_DESCRIPTOR &&
    (setup.bmRequestType & 0xff) === BM_REQUEST_TYPE_DEVICE_TO_HOST_STANDARD_DEVICE &&
    ((setup.wValue >> 8) & 0xff) === DESCRIPTOR_TYPE_CONFIGURATION
  );
}

function rewriteOtherSpeedConfigAsConfig(bytes: Uint8Array): void {
  // OTHER_SPEED_CONFIGURATION and CONFIGURATION share the same layout; only the top-level
  // bDescriptorType differs. Rewriting just that byte is enough for a full-speed guest.
  if (bytes.length >= 2) {
    bytes[1] = DESCRIPTOR_TYPE_CONFIGURATION;
  }
}

export type WebUsbControlInResult =
  | { status: "ok"; data: Uint8Array }
  | { status: "stall" }
  | { status: "babble" };

export async function executeWebUsbControlIn(
  device: Pick<USBDevice, "controlTransferIn">,
  setup: SetupPacket,
): Promise<WebUsbControlInResult> {
  const length = setup.wLength & 0xffff;

  if (shouldTranslateConfigurationDescriptor(setup)) {
    const descriptorIndex = setup.wValue & 0x00ff;
    const otherSpeedSetup: SetupPacket = {
      ...setup,
      wValue: (DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION << 8) | descriptorIndex,
    };

    try {
      const otherSpeedParams = setupPacketToWebUsbParameters(otherSpeedSetup);
      const otherSpeedResult = await device.controlTransferIn(otherSpeedParams, length);
      if (otherSpeedResult.status === "ok" && otherSpeedResult.data) {
        const data = dataViewToUint8Array(otherSpeedResult.data);
        rewriteOtherSpeedConfigAsConfig(data);
        return { status: "ok", data };
      }
    } catch {
      // Fall back to CONFIGURATION (0x02) if OTHER_SPEED_CONFIGURATION is rejected/unsupported.
    }
  }

  const params = setupPacketToWebUsbParameters(setup);
  const result = await device.controlTransferIn(params, length);
  if (result.status === "ok") {
    const data = result.data ? dataViewToUint8Array(result.data) : new Uint8Array();
    return { status: "ok", data };
  }

  return { status: result.status };
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
        throw wrapWithCause("Failed to open USB device", err);
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
        throw wrapWithCause("Failed to select USB configuration", err);
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

    // Chromium blocks "protected" interface classes (HID, Mass Storage, etc.).
    // Some composite devices still appear in the chooser because they have at
    // least one non-protected interface, but attempting to claim the protected
    // interfaces will fail.
    //
    // For the passthrough backend, we try to claim as many interfaces as we can,
    // but do not fail the whole device if some interfaces cannot be claimed.
    // Instead, we only throw if *none* of the interfaces can be claimed.
    let claimedAny = false;
    let firstClaimErr: unknown = null;
    for (const iface of configuration.interfaces) {
      const ifaceNum = iface.interfaceNumber;
      if (this.claimedInterfaces.has(ifaceNum)) {
        claimedAny = true;
        continue;
      }
      if (iface.claimed) {
        this.claimedInterfaces.add(ifaceNum);
        claimedAny = true;
        continue;
      }
      if (interfaceIsWebUsbProtected(iface)) {
        // Skip interfaces that are likely blocked by Chromium's protected interface class list.
        continue;
      }
      try {
        await this.device.claimInterface(ifaceNum);
      } catch (err) {
        firstClaimErr ??= err;
        console.warn(`Failed to claim USB interface ${ifaceNum}: ${formatThrownError(err)}`);
        continue;
      }
      this.claimedInterfaces.add(ifaceNum);
      claimedAny = true;
    }

    if (!claimedAny && firstClaimErr) {
      throw wrapWithCause("Failed to claim any USB interface", firstClaimErr);
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

          const result = await executeWebUsbControlIn(this.device, action.setup);
          if (result.status === "ok") {
            return { kind: "controlIn", id: action.id, status: "success", data: result.data };
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

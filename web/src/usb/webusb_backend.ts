// WebUSB backend for USB passthrough.
//
// This module provides a small, JS-friendly executor for the Rust-side
// `UsbHostAction` / `UsbHostCompletion` contract.

import { formatWebUsbError } from "../platform/webusb_troubleshooting";
import { isWebUsbProtectedInterfaceClass } from "../platform/webusb";

import type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";
import { hex16, hex8 } from "./usb_hex";

export type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";

export function isWebUsbSupported(): boolean {
  if (typeof navigator === "undefined") return false;
  return !!(navigator as Navigator & { usb?: USB }).usb;
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
    (error as Error & { cause?: unknown }).cause = cause;
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

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // TypeScript's `BufferSource` type excludes `SharedArrayBuffer` in some lib.dom
  // versions, even though Chromium accepts it for WebUSB calls. Keep the backend
  // strict-friendly by copying when the buffer is shared.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
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
const CLEAR_FEATURE = 0x01;
const SET_CONFIGURATION = 0x09;
const SET_INTERFACE = 0x0b;
const DESCRIPTOR_TYPE_CONFIGURATION = 0x02;
const DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION = 0x07;
const BM_REQUEST_TYPE_DEVICE_TO_HOST_STANDARD_DEVICE = 0x80;
const BM_REQUEST_TYPE_HOST_TO_DEVICE_STANDARD_DEVICE = 0x00;
const BM_REQUEST_TYPE_HOST_TO_DEVICE_STANDARD_INTERFACE = 0x01;
const BM_REQUEST_TYPE_HOST_TO_DEVICE_STANDARD_ENDPOINT = 0x02;
const FEATURE_SELECTOR_ENDPOINT_HALT = 0x00;

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

export type WebUsbControlInOptions = {
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
  translateOtherSpeedConfig?: boolean;
};

export async function executeWebUsbControlIn(
  device: Pick<USBDevice, "controlTransferIn">,
  setup: SetupPacket,
  options: WebUsbControlInOptions = {},
): Promise<WebUsbControlInResult> {
  const length = setup.wLength & 0xffff;

  const translateOtherSpeedConfig = options.translateOtherSpeedConfig ?? true;
  if (translateOtherSpeedConfig && shouldTranslateConfigurationDescriptor(setup)) {
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
  private readonly translateOtherSpeedConfig: boolean;

  constructor(device: USBDevice, options: { translateOtherSpeedConfig?: boolean } = {}) {
    assertWebUsbSupported();
    this.device = device;
    this.translateOtherSpeedConfig = options.translateOtherSpeedConfig ?? true;
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
    let attemptedAnyClaim = false;
    let firstClaimErr: unknown = null;
    const protectedInterfaces: number[] = [];
    for (const iface of configuration.interfaces) {
      const ifaceNum = iface.interfaceNumber;
      if (iface.claimed) {
        this.claimedInterfaces.add(ifaceNum);
        claimedAny = true;
        continue;
      }
      // Our local cache may be stale if the device was closed/reopened (claims are
      // dropped). Only treat cached entries as claimed when the device reports it.
      if (this.claimedInterfaces.has(ifaceNum)) {
        this.claimedInterfaces.delete(ifaceNum);
      }
      if (interfaceIsWebUsbProtected(iface)) {
        // Skip interfaces that are likely blocked by Chromium's protected interface class list.
        protectedInterfaces.push(ifaceNum);
        continue;
      }
      try {
        attemptedAnyClaim = true;
        await this.device.claimInterface(ifaceNum);
      } catch (err) {
        firstClaimErr ??= err;
        console.warn(`Failed to claim USB interface ${ifaceNum}: ${formatThrownError(err)}`);
        continue;
      }
      this.claimedInterfaces.add(ifaceNum);
      claimedAny = true;
    }

    if (!claimedAny) {
      if (firstClaimErr) {
        throw wrapWithCause("Failed to claim any USB interface", firstClaimErr);
      }

      // If we never attempted a claim, it usually means every interface was skipped
      // as "protected" by Chromium's WebUSB restrictions (HID, Mass Storage, ...).
      // Surface that clearly so downstream bulk transfers don't fail with confusing
      // errors while the backend looks "ready".
      if (
        !attemptedAnyClaim &&
        configuration.interfaces.length > 0 &&
        protectedInterfaces.length === configuration.interfaces.length
      ) {
        const vid = (this.device as unknown as { vendorId?: number }).vendorId;
        const pid = (this.device as unknown as { productId?: number }).productId;
        const vidPid = vid !== undefined && pid !== undefined ? `${hex16(vid)}:${hex16(pid)}` : "unknown VID:PID";
        const ifaceList = protectedInterfaces.join(", ");
        throw new Error(
          `No claimable USB interfaces: all interfaces are protected by Chromium WebUSB restrictions (device ${vidPid}, protected interfaces: ${ifaceList})`,
        );
      }

      throw new Error(
        [
          "No claimable interface was found on this USB device.",
          "The device is likely blocked by Chromium's protected interface class list (e.g. HID, Mass Storage), so WebUSB cannot claim any interfaces for bulk/interrupt transfers.",
        ].join("\n"),
      );
    }
  }

  async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
    assertWebUsbSupported();

    // Bulk transfers use USB endpoint *addresses*, while WebUSB wants the endpoint number.
    // Validate the address before opening/claiming the device so malformed actions can't
    // trigger nonsensical WebUSB calls.
    if (action.kind === "bulkIn") {
      const endpoint = action.endpoint;
      const endpointNumber = endpoint & 0x0f;
      const valid = (endpoint & 0x80) !== 0 && (endpoint & 0x70) === 0 && endpointNumber !== 0;
      if (!valid) {
        return errorCompletion(
          action.kind,
          action.id,
          `Invalid bulkIn endpoint address ${hex8(endpoint)} (expected IN endpoint address with reserved bits clear and endpoint number 1-15)`,
        );
      }
    } else if (action.kind === "bulkOut") {
      const endpoint = action.endpoint;
      const endpointNumber = endpoint & 0x0f;
      const valid = (endpoint & 0x80) === 0 && (endpoint & 0x70) === 0 && endpointNumber !== 0;
      if (!valid) {
        return errorCompletion(
          action.kind,
          action.id,
          `Invalid bulkOut endpoint address ${hex8(endpoint)} (expected OUT endpoint address with reserved bits clear and endpoint number 1-15)`,
        );
      }
    }

    if (action.kind === "controlOut") {
      const setup = action.setup;
      const emptyData = action.data.byteLength === 0;

      if (
        (setup.bmRequestType & 0xff) === BM_REQUEST_TYPE_HOST_TO_DEVICE_STANDARD_DEVICE &&
        (setup.bRequest & 0xff) === SET_CONFIGURATION &&
        (setup.wIndex & 0xffff) === 0 &&
        (setup.wLength & 0xffff) === 0 &&
        emptyData
      ) {
        if (!this.device.opened) {
          try {
            await this.device.open();
          } catch (err) {
            return errorCompletion(action.kind, action.id, formatThrownError(wrapWithCause("Failed to open USB device", err)));
          }
        }

        try {
          const configValue = setup.wValue & 0xff;

          const existingConfiguration = this.device.configuration;
          if (existingConfiguration) {
            for (const iface of existingConfiguration.interfaces) {
              if (!iface.claimed) continue;
              try {
                await this.device.releaseInterface(iface.interfaceNumber);
              } catch (err) {
                throw wrapWithCause(
                  `Failed to release USB interface ${iface.interfaceNumber} before selecting configuration ${configValue}`,
                  err,
                );
              }
            }
          }

          try {
            await this.device.selectConfiguration(configValue);
          } catch (err) {
            throw wrapWithCause(`Failed to select USB configuration ${configValue}`, err);
          }
          // Selecting a configuration resets all claims; keep our local cache coherent.
          this.claimedInterfaces.clear();
          this.claimedConfigurationValue = this.device.configuration?.configurationValue ?? configValue;
          return { kind: "controlOut", id: action.id, status: "success", bytesWritten: 0 };
        } catch (err) {
          return errorCompletion(action.kind, action.id, formatThrownError(err));
        }
      }
    }

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

          const result = await executeWebUsbControlIn(this.device, action.setup, {
            translateOtherSpeedConfig: this.translateOtherSpeedConfig,
          });
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

          const setup = action.setup;
          const emptyData = action.data.byteLength === 0;

          if (
            (setup.bmRequestType & 0xff) === BM_REQUEST_TYPE_HOST_TO_DEVICE_STANDARD_INTERFACE &&
            (setup.bRequest & 0xff) === SET_INTERFACE &&
            (setup.wLength & 0xffff) === 0 &&
            emptyData
          ) {
            const interfaceNumber = setup.wIndex & 0xff;
            const altSetting = setup.wValue & 0xff;

            const configuration = this.device.configuration;
            if (!configuration) {
              throw new Error("USB device has no active configuration for SET_INTERFACE");
            }

            const iface = configuration.interfaces.find((entry) => entry.interfaceNumber === interfaceNumber);
            const ifaceClaimed = iface?.claimed ?? false;
            if (!ifaceClaimed) {
              try {
                await this.device.claimInterface(interfaceNumber);
              } catch (err) {
                throw wrapWithCause(`Failed to claim USB interface ${interfaceNumber} for SET_INTERFACE`, err);
              }
              this.claimedInterfaces.add(interfaceNumber);
            }

            try {
              await this.device.selectAlternateInterface(interfaceNumber, altSetting);
            } catch (err) {
              throw wrapWithCause(
                `Failed to select alternate setting ${altSetting} for USB interface ${interfaceNumber}`,
                err,
              );
            }

            return { kind: "controlOut", id: action.id, status: "success", bytesWritten: 0 };
          }

          if (
            (setup.bmRequestType & 0xff) === BM_REQUEST_TYPE_HOST_TO_DEVICE_STANDARD_ENDPOINT &&
            (setup.bRequest & 0xff) === CLEAR_FEATURE &&
            (setup.wValue & 0xffff) === FEATURE_SELECTOR_ENDPOINT_HALT &&
            (setup.wLength & 0xffff) === 0 &&
            emptyData
          ) {
            const endpointAddress = setup.wIndex & 0xff;
            const endpointNumber = endpointAddressToEndpointNumber(endpointAddress);
            try {
              const direction: USBDirection = (endpointAddress & 0x80) !== 0 ? "in" : "out";
              await this.device.clearHalt(direction, endpointNumber);
            } catch (err) {
              throw wrapWithCause(
                `Failed to clear HALT for endpoint ${hex8(endpointAddress)} (ep ${endpointNumber})`,
                err,
              );
            }
            return { kind: "controlOut", id: action.id, status: "success", bytesWritten: 0 };
          }

          const params = setupPacketToWebUsbParameters(action.setup);
          const hasOutData = (action.setup.wLength & 0xffff) !== 0 || action.data.byteLength !== 0;
          const result = hasOutData
            ? await this.device.controlTransferOut(params, ensureArrayBufferBacked(action.data))
            : await this.device.controlTransferOut(params);
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
          const endpointNumber = action.endpoint & 0x0f;
          const result = await this.device.transferIn(endpointNumber, action.length);
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
          const endpointNumber = action.endpoint & 0x0f;
          const result = await this.device.transferOut(endpointNumber, ensureArrayBufferBacked(action.data));
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

import { usbErrorCompletion, type UsbHostAction, type UsbHostCompletion, type UsbSetupPacket } from "./usb_proxy_protocol";

function parseRequestType(bmRequestType: number): USBRequestType {
  // Bits 6..5
  const typeBits = (bmRequestType >>> 5) & 0b11;
  switch (typeBits) {
    case 0:
      return "standard";
    case 1:
      return "class";
    case 2:
      return "vendor";
    default:
      // Reserved. Treat as vendor to avoid throwing during dev/demo usage.
      return "vendor";
  }
}

function parseRecipient(bmRequestType: number): USBRecipient {
  // Bits 4..0
  const rec = bmRequestType & 0b1_1111;
  switch (rec) {
    case 0:
      return "device";
    case 1:
      return "interface";
    case 2:
      return "endpoint";
    case 3:
      return "other";
    default:
      return "other";
  }
}

function toControlTransferParameters(setup: UsbSetupPacket): USBControlTransferParameters {
  return {
    requestType: parseRequestType(setup.bmRequestType),
    recipient: parseRecipient(setup.bmRequestType),
    request: setup.bRequest & 0xff,
    value: setup.wValue & 0xffff,
    index: setup.wIndex & 0xffff,
  };
}

function inResultToCompletion(id: number, res: USBInTransferResult): UsbHostCompletion {
  if (res.status === "stall") return { kind: "stall", id };
  if (res.status !== "ok") return usbErrorCompletion(id, `transfer status: ${res.status ?? "unknown"}`);
  const view = res.data;
  const bytes = view ? new Uint8Array(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength)) : new Uint8Array();
  return { kind: "okIn", id, data: bytes };
}

function outResultToCompletion(id: number, res: USBOutTransferResult): UsbHostCompletion {
  if (res.status === "stall") return { kind: "stall", id };
  if (res.status !== "ok") return usbErrorCompletion(id, `transfer status: ${res.status ?? "unknown"}`);
  return { kind: "okOut", id, bytesWritten: res.bytesWritten ?? 0 };
}

export class WebUsbBackend {
  constructor(private readonly device: USBDevice) {}

  async ensureOpenAndClaimed(): Promise<void> {
    if (!this.device.opened) {
      await this.device.open();
    }

    if (!this.device.configuration) {
      const configValue =
        this.device.configurations?.length && this.device.configurations[0]
          ? this.device.configurations[0].configurationValue
          : 1;
      await this.device.selectConfiguration(configValue);
    }

    const config = this.device.configuration;
    if (!config) return;

    // For passthrough usage we want access to any endpoints on the device.
    // Claim every interface we can; ignore failures so a partially-claimable
    // device can still be used for control transfers.
    for (const iface of config.interfaces) {
      try {
        await this.device.claimInterface(iface.interfaceNumber);
        const altSetting = iface.alternates?.[0]?.alternateSetting ?? 0;
        await this.device.selectAlternateInterface(iface.interfaceNumber, altSetting);
      } catch (err) {
        console.warn("WebUSB claimInterface failed", err);
      }
    }
  }

  async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
    try {
      switch (action.kind) {
        case "controlIn": {
          const params = toControlTransferParameters(action.setup);
          const res = await this.device.controlTransferIn(params, action.setup.wLength & 0xffff);
          return inResultToCompletion(action.id, res);
        }
        case "controlOut": {
          const params = toControlTransferParameters(action.setup);
          const res = await this.device.controlTransferOut(params, action.data);
          return outResultToCompletion(action.id, res);
        }
        case "bulkIn": {
          const res = await this.device.transferIn(action.ep, action.length & 0xffff);
          return inResultToCompletion(action.id, res);
        }
        case "bulkOut": {
          const res = await this.device.transferOut(action.ep, action.data);
          return outResultToCompletion(action.id, res);
        }
        default: {
          const neverAction: never = action;
          return usbErrorCompletion(action.id, `unknown action: ${String((neverAction as { kind?: unknown }).kind)}`);
        }
      }
    } catch (err) {
      return usbErrorCompletion(action.id, err instanceof Error ? err.message : String(err));
    }
  }
}


import type { PlatformFeatureReport } from "../platform/features";
import { WebUsbBackend, type SetupPacket } from "./webusb_backend";

type ParsedDeviceDescriptor = {
  bLength?: number;
  bDescriptorType?: number;
  idVendor?: number;
  idProduct?: number;
};

function hex16(value: number): string {
  return `0x${value.toString(16).padStart(4, "0")}`;
}

function hex8(value: number): string {
  return `0x${value.toString(16).padStart(2, "0")}`;
}

function formatHexBytes(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(" ");
}

function parseDeviceDescriptor(bytes: Uint8Array): ParsedDeviceDescriptor {
  const out: ParsedDeviceDescriptor = {};
  if (bytes.byteLength >= 1) out.bLength = bytes[0];
  if (bytes.byteLength >= 2) out.bDescriptorType = bytes[1];
  if (bytes.byteLength >= 10) out.idVendor = bytes[8] | (bytes[9] << 8);
  if (bytes.byteLength >= 12) out.idProduct = bytes[10] | (bytes[11] << 8);
  return out;
}

function formatUsbError(err: unknown): string {
  if (err instanceof DOMException) {
    // `message` is sometimes empty; surface the name regardless.
    return err.message ? `${err.name}: ${err.message}` : err.name;
  }
  if (err instanceof Error) return err.message;
  return String(err);
}

function appendHintForError(message: string, err: unknown): string {
  if (!(err instanceof DOMException)) return message;

  // Map common WebUSB errors into more actionable hints.
  if (err.name === "NotFoundError") {
    return `${message}\n\nHint: The permission prompt was likely dismissed, or the device exposes only protected interfaces (e.g. HID) which WebUSB refuses to grant.`;
  }
  if (err.name === "SecurityError") {
    return `${message}\n\nHint: WebUSB requires a secure context (HTTPS) and is only supported by some browsers (typically Chromium-based).`;
  }
  if (err.name === "NetworkError") {
    return `${message}\n\nHint: The device may be disconnected or already in use by another application/driver.`;
  }

  return message;
}

export function renderWebUsbPanel(report: PlatformFeatureReport): HTMLElement {
  const panel = document.createElement("div");
  panel.className = "panel";

  const title = document.createElement("h2");
  title.textContent = "WebUSB";

  const note = document.createElement("div");
  note.className = "hint";
  note.textContent =
    "WebUSB device permission requires a user gesture (button click) and may fail on unsupported browsers or for devices with protected interfaces.";

  const actions = document.createElement("div");
  actions.className = "row";

  const requestButton = document.createElement("button");
  requestButton.type = "button";
  requestButton.textContent = "Request USB device";

  const openButton = document.createElement("button");
  openButton.type = "button";
  openButton.textContent = "Open + read device descriptor";
  openButton.disabled = true;

  actions.append(requestButton, openButton);

  const status = document.createElement("pre");
  status.className = "mono";

  const output = document.createElement("pre");
  output.className = "mono";

  const error = document.createElement("pre");
  error.className = "mono error";

  panel.append(title, note, actions, status, output, error);

  let selected: USBDevice | null = null;
  let nextRequestId = 1;

  const refreshStatus = () => {
    if (!report.webusb) {
      status.textContent = "WebUSB: missing (navigator.usb is not available in this browser/context)";
      requestButton.disabled = true;
      openButton.disabled = true;
      return;
    }

    if (!selected) {
      status.textContent = "WebUSB: supported. No device selected.";
      openButton.disabled = true;
      return;
    }

    status.textContent = `Selected device: vendorId=${hex16(selected.vendorId)} productId=${hex16(selected.productId)} opened=${
      selected.opened ? "true" : "false"
    }`;
    openButton.disabled = false;
  };

  refreshStatus();

  requestButton.onclick = async () => {
    error.textContent = "";
    output.textContent = "";

    if (!report.webusb) {
      error.textContent = "WebUSB is not supported in this browser/context.";
      return;
    }

    // Disable while the permission prompt is open to avoid overlapping calls.
    requestButton.disabled = true;
    openButton.disabled = true;
    status.textContent = "Requesting device permission…";

    try {
      const usb = navigator.usb;
      if (!usb) throw new Error("navigator.usb is unavailable");

      // Minimal filter list: `{}` matches any device (subject to browser restrictions).
      selected = await usb.requestDevice({ filters: [{}] });
      status.textContent = "Device selected.";
    } catch (err) {
      const msg = appendHintForError(formatUsbError(err), err);
      error.textContent = msg;
      console.error(err);
      selected = null;
    } finally {
      requestButton.disabled = false;
      refreshStatus();
    }
  };

  openButton.onclick = async () => {
    error.textContent = "";
    output.textContent = "";

    const device = selected;
    if (!device) {
      error.textContent = "No device selected. Click “Request USB device” first.";
      refreshStatus();
      return;
    }

    requestButton.disabled = true;
    openButton.disabled = true;
    status.textContent = "Opening device…";

    const backend = new WebUsbBackend(device);
    try {
      await backend.ensureOpenAndClaimed();
      status.textContent = "Issuing GET_DESCRIPTOR(Device)…";

      const setup: SetupPacket = {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0100,
        wIndex: 0x0000,
        wLength: 18,
      };

      const completion = await backend.execute({ kind: "controlIn", id: nextRequestId++, setup });
      if (completion.kind !== "controlIn") {
        throw new Error(`Unexpected completion kind: ${completion.kind}`);
      }

      const header = [
        `Device properties: vendorId=${hex16(device.vendorId)} productId=${hex16(device.productId)}`,
        `Control transfer: bmRequestType=${hex8(0x80)} bRequest=${hex8(0x06)} wValue=${hex16(0x0100)} wIndex=${hex16(0)} wLength=18`,
        `Result: status=${completion.status} bytes=${completion.status === "success" ? completion.data.byteLength : 0}`,
      ].join("\n");

      // Always show the header, even if the transfer fails (stall/error).
      output.textContent = header;

      if (completion.status === "stall") {
        throw new Error("GET_DESCRIPTOR(Device) stalled");
      }
      if (completion.status === "error") {
        throw new Error(completion.message);
      }

      const parsed = parseDeviceDescriptor(completion.data);
      const parsedLines = [
        `Parsed (best-effort): bLength=${parsed.bLength ?? "?"} bDescriptorType=${parsed.bDescriptorType ?? "?"}`,
        `Parsed (best-effort): idVendor=${parsed.idVendor === undefined ? "?" : hex16(parsed.idVendor)} idProduct=${
          parsed.idProduct === undefined ? "?" : hex16(parsed.idProduct)
        }`,
      ].join("\n");

      output.textContent = `${header}\n\nDescriptor bytes:\n${formatHexBytes(completion.data)}\n\n${parsedLines}`;
      status.textContent = "OK.";
    } catch (err) {
      const msg = appendHintForError(formatUsbError(err), err);
      error.textContent = msg;
      status.textContent = "Failed.";
      console.error(err);
    } finally {
      requestButton.disabled = false;
      refreshStatus();
    }
  };

  return panel;
}

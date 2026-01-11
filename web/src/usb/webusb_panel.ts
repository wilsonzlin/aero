import type { PlatformFeatureReport } from "../platform/features";
import { explainWebUsbError, formatWebUsbError } from "../platform/webusb_troubleshooting";
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

function summarizeUsbDevice(device: USBDevice): Record<string, unknown> {
  return {
    vendorId: hex16(device.vendorId),
    productId: hex16(device.productId),
    opened: device.opened,
    productName: device.productName ?? null,
    manufacturerName: device.manufacturerName ?? null,
    serialNumber: device.serialNumber ?? null,
  };
}

function formatErrorWithHints(err: unknown): string {
  const formatted = formatWebUsbError(err);
  const explained = explainWebUsbError(err);
  const parts: string[] = [explained.title];
  if (explained.details) parts.push(explained.details);
  if (formatted) parts.push(formatted);
  if (explained.hints.length) {
    parts.push(`Hints:\n${explained.hints.map((h) => `- ${h}`).join("\n")}`);
  }
  return parts.join("\n\n");
}

async function runWebUsbProbeWorker(msg: unknown, timeoutMs = 10_000): Promise<unknown> {
  const worker = new Worker(new URL("./webusb_probe_worker.ts", import.meta.url), { type: "module" });

  return await new Promise((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      worker.terminate();
      reject(new Error(`WebUSB probe worker timed out after ${timeoutMs}ms`));
    }, timeoutMs);

    const cleanup = () => {
      window.clearTimeout(timeout);
      worker.terminate();
    };

    worker.addEventListener("message", (ev) => {
      cleanup();
      resolve(ev.data);
    });
    worker.addEventListener("messageerror", () => {
      cleanup();
      reject(new Error("WebUSB probe worker message deserialization failed"));
    });
    worker.addEventListener("error", (ev) => {
      cleanup();
      reject(new Error(ev instanceof ErrorEvent ? ev.message : String(ev)));
    });

    try {
      worker.postMessage(msg);
    } catch (err) {
      cleanup();
      reject(err);
    }
  });
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

  const requestButton = document.createElement("button");
  requestButton.type = "button";
  requestButton.textContent = "Request USB device";

  const openButton = document.createElement("button");
  openButton.type = "button";
  openButton.textContent = "Open + read device descriptor";
  openButton.disabled = true;

  const listButton = document.createElement("button");
  listButton.type = "button";
  listButton.textContent = "List permitted devices (getDevices)";

  const workerProbeButton = document.createElement("button");
  workerProbeButton.type = "button";
  workerProbeButton.textContent = "Probe worker WebUSB";

  const cloneButton = document.createElement("button");
  cloneButton.type = "button";
  cloneButton.textContent = "Send selected device to worker";

  const actions = document.createElement("div");
  actions.className = "row";
  actions.append(requestButton, openButton, listButton);

  const workerActions = document.createElement("div");
  workerActions.className = "row";
  workerActions.append(workerProbeButton, cloneButton);

  const status = document.createElement("pre");
  status.className = "mono";

  const output = document.createElement("pre");
  output.className = "mono";

  const error = document.createElement("pre");
  error.className = "mono error";

  panel.append(title, note, actions, workerActions, status, output, error);

  let selected: USBDevice | null = null;
  let nextRequestId = 1;

  const refreshStatus = () => {
    if (!report.webusb) {
      status.textContent = "WebUSB: missing (navigator.usb is not available in this browser/context)";
      requestButton.disabled = true;
      openButton.disabled = true;
      return;
    }

    const userActivation = navigator.userActivation;
    const secure = (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true;
    const envInfo =
      `isSecureContext=${secure}\n` +
      `crossOriginIsolated=${report.crossOriginIsolated}\n` +
      `userActivation.isActive=${userActivation?.isActive ?? "n/a"}\n` +
      `userActivation.hasBeenActive=${userActivation?.hasBeenActive ?? "n/a"}\n`;

    if (!selected) {
      status.textContent = `WebUSB: supported. No device selected.\n\n${envInfo}`;
      openButton.disabled = true;
      return;
    }

    status.textContent = `Selected device: ${JSON.stringify(summarizeUsbDevice(selected))}\n\n${envInfo}`;
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
      output.textContent = JSON.stringify({ selected: summarizeUsbDevice(selected) }, null, 2);
    } catch (err) {
      error.textContent = formatErrorWithHints(err);
      console.error(err);
      selected = null;
    } finally {
      requestButton.disabled = false;
      refreshStatus();
    }
  };

  listButton.onclick = async () => {
    error.textContent = "";
    output.textContent = "";

    if (!report.webusb) {
      error.textContent = "WebUSB is not supported in this browser/context.";
      return;
    }

    try {
      const usb = navigator.usb;
      if (!usb) throw new Error("navigator.usb is unavailable");
      if (typeof usb.getDevices !== "function") throw new Error("navigator.usb.getDevices() is unavailable");

      const devices = await usb.getDevices();
      output.textContent = JSON.stringify(
        {
          count: devices.length,
          devices: devices.map((d) => summarizeUsbDevice(d)),
        },
        null,
        2,
      );
    } catch (err) {
      error.textContent = formatErrorWithHints(err);
      console.error(err);
    } finally {
      refreshStatus();
    }
  };

  workerProbeButton.onclick = async () => {
    error.textContent = "";
    output.textContent = "";

    try {
      const resp = await runWebUsbProbeWorker({ type: "probe" });
      output.textContent = JSON.stringify(resp, null, 2);
    } catch (err) {
      error.textContent = formatErrorWithHints(err);
      console.error(err);
    }
  };

  cloneButton.onclick = async () => {
    error.textContent = "";
    output.textContent = "";

    if (!selected) {
      error.textContent = "No device selected. Click “Request USB device” first.";
      refreshStatus();
      return;
    }

    try {
      const resp = await runWebUsbProbeWorker({ type: "clone-test", device: selected });
      output.textContent = JSON.stringify(resp, null, 2);
    } catch (err) {
      error.textContent = formatErrorWithHints(err);
      console.error(err);
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
      error.textContent = formatErrorWithHints(err);
      status.textContent = "Failed.";
      console.error(err);
    } finally {
      requestButton.disabled = false;
      refreshStatus();
    }
  };

  return panel;
}

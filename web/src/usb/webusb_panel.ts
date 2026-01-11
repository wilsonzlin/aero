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

async function requestUsbDevice(usb: USB): Promise<{ device: USBDevice; filterNote: string }> {
  // Chromium versions differ on whether `filters: []` and/or `filters: [{}]` are allowed.
  // Try a few reasonable fallbacks so the smoke test works in more environments.
  const attempts: Array<{ note: string; options: USBDeviceRequestOptions }> = [
    { note: "filters: [] (broadest, if allowed by this Chromium build)", options: { filters: [] } },
    { note: "filters: [{}] (match-all, if allowed by this Chromium build)", options: { filters: [{}] } },
    {
      note: "filters: [{classCode: 0x00}, {classCode: 0xff}] (fallback broad device-class match)",
      options: { filters: [{ classCode: 0x00 }, { classCode: 0xff }] },
    },
    { note: "filters: [{classCode: 0xff}] (vendor-specific only fallback)", options: { filters: [{ classCode: 0xff }] } },
  ];

  let lastErr: unknown = null;
  for (const attempt of attempts) {
    try {
      const device = await usb.requestDevice(attempt.options);
      return { device, filterNote: attempt.note };
    } catch (err) {
      lastErr = err;

      // User canceled the chooser (or there were no matching devices).
      if (err instanceof DOMException && err.name === "NotFoundError") {
        throw err;
      }

      // If the browser rejected the filter shape, try the next fallback.
      if (err instanceof TypeError) continue;
      if (err instanceof DOMException && err.name === "TypeError") continue;

      throw err;
    }
  }

  throw lastErr ?? new Error("USB requestDevice failed");
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
  note.append(
    "WebUSB device permission requires a user gesture (button click) and may fail on unsupported browsers or for devices with protected interfaces.",
    document.createElement("br"),
    "For deeper inspection of protected interface classes and claimability, see ",
    Object.assign(document.createElement("a"), {
      href: "/webusb_diagnostics.html",
      target: "_blank",
      rel: "noopener",
      textContent: "/webusb_diagnostics.html",
    }),
    ".",
  );

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

  const errorTitle = document.createElement("div");
  errorTitle.className = "bad";

  const errorDetails = document.createElement("div");
  errorDetails.className = "hint";

  const errorRaw = document.createElement("pre");
  errorRaw.className = "mono";

  const errorHints = document.createElement("ul");

  panel.append(title, note, actions, workerActions, status, output, errorTitle, errorDetails, errorRaw, errorHints);

  let selected: USBDevice | null = null;
  let nextRequestId = 1;

  const clearError = () => {
    errorTitle.textContent = "";
    errorDetails.textContent = "";
    errorRaw.textContent = "";
    errorHints.replaceChildren();
  };

  const showMessage = (message: string) => {
    clearError();
    errorTitle.textContent = message;
  };

  const showError = (err: unknown) => {
    const explained = explainWebUsbError(err);
    errorTitle.textContent = explained.title;
    errorDetails.textContent = explained.details ?? "";
    errorRaw.textContent = formatWebUsbError(err);
    errorHints.replaceChildren(
      ...explained.hints.map((hint) => {
        const li = document.createElement("li");
        li.textContent = hint;
        return li;
      }),
    );
  };

  const refreshStatus = () => {
    if (!report.webusb) {
      status.textContent = "WebUSB: missing (navigator.usb is not available in this browser/context)";
      requestButton.disabled = true;
      openButton.disabled = true;
      listButton.disabled = true;
      cloneButton.disabled = true;
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
      listButton.disabled = false;
      cloneButton.disabled = true;
      return;
    }

    status.textContent = `Selected device: ${JSON.stringify(summarizeUsbDevice(selected))}\n\n${envInfo}`;
    openButton.disabled = false;
    listButton.disabled = false;
    cloneButton.disabled = false;
  };

  refreshStatus();

  requestButton.onclick = async () => {
    clearError();
    output.textContent = "";

    if (!report.webusb) {
      showMessage("WebUSB is not supported in this browser/context.");
      return;
    }

    // Disable while the permission prompt is open to avoid overlapping calls.
    requestButton.disabled = true;
    openButton.disabled = true;
    status.textContent = "Requesting device permission…";

    try {
      const usb = navigator.usb;
      if (!usb) throw new Error("navigator.usb is unavailable");

      const { device, filterNote } = await requestUsbDevice(usb);
      selected = device;
      status.textContent = "Device selected.";
      output.textContent = JSON.stringify({ selected: summarizeUsbDevice(device), filterNote }, null, 2);
    } catch (err) {
      showError(err);
      console.error(err);
      selected = null;
    } finally {
      requestButton.disabled = false;
      refreshStatus();
    }
  };

  listButton.onclick = async () => {
    clearError();
    output.textContent = "";

    if (!report.webusb) {
      showMessage("WebUSB is not supported in this browser/context.");
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
      showError(err);
      console.error(err);
    } finally {
      refreshStatus();
    }
  };

  workerProbeButton.onclick = async () => {
    clearError();
    output.textContent = "";

    try {
      const resp = await runWebUsbProbeWorker({ type: "probe" });
      output.textContent = JSON.stringify(resp, null, 2);
    } catch (err) {
      showError(err);
      console.error(err);
    }
  };

  cloneButton.onclick = async () => {
    clearError();
    output.textContent = "";

    if (!selected) {
      showMessage("No device selected. Click “Request USB device” first.");
      refreshStatus();
      return;
    }

    try {
      const resp = await runWebUsbProbeWorker({ type: "clone-test", device: selected });
      output.textContent = JSON.stringify(resp, null, 2);
    } catch (err) {
      showError(err);
      console.error(err);
    }
  };

  openButton.onclick = async () => {
    clearError();
    output.textContent = "";

    const device = selected;
    if (!device) {
      showMessage("No device selected. Click “Request USB device” first.");
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
      showError(err);
      status.textContent = "Failed.";
      console.error(err);
    } finally {
      requestButton.disabled = false;
      refreshStatus();
    }
  };

  return panel;
}

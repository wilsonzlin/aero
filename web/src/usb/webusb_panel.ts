import type { PlatformFeatureReport } from "../platform/features";
import { explainWebUsbError, formatWebUsbError } from "../platform/webusb_troubleshooting";
import { formatOneLineError } from "../text";
import { unrefBestEffort } from "../unrefSafe";
import { WebUsbBackend, type SetupPacket } from "./webusb_backend";
import { formatHexBytes, hex16, hex8 } from "./usb_hex";

type ForgettableUsbDevice = USBDevice & { forget: () => Promise<void> };

function canForgetUsbDevice(device: USBDevice): device is ForgettableUsbDevice {
  return typeof (device as unknown as { forget?: unknown }).forget === "function";
}

type ParsedDeviceDescriptor = {
  bLength?: number;
  bDescriptorType?: number;
  idVendor?: number;
  idProduct?: number;
};

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

function describeUsbDevice(device: USBDevice): string {
  const vidPid = `${hex16(device.vendorId)}:${hex16(device.productId)}`;
  const parts = [vidPid];
  if (device.manufacturerName) parts.push(device.manufacturerName);
  if (device.productName) parts.push(device.productName);
  if (device.serialNumber) parts.push(`sn=${device.serialNumber}`);
  if (device.opened) parts.push("(opened)");
  return parts.join(" ");
}

function parseUsbId(text: string): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const normalized = trimmed.toLowerCase().startsWith("0x") ? trimmed.slice(2) : trimmed;
  const value = Number.parseInt(normalized, 16);
  if (!Number.isFinite(value)) return null;
  if (value < 0 || value > 0xffff) return null;
  return value;
}

async function requestUsbDevice(
  usb: USB,
  opts: { vendorId: number | null; productId: number | null } = { vendorId: null, productId: null },
): Promise<{ device: USBDevice; filterNote: string }> {
  if (opts.productId !== null && opts.vendorId === null) {
    throw new Error("productId filter requires vendorId.");
  }

  // Chromium versions differ on whether `filters: []` and/or `filters: [{}]` are allowed.
  // Try a few reasonable fallbacks so the smoke test works in more environments.
  const attempts: Array<{ note: string; options: USBDeviceRequestOptions }> = [];

  if (opts.vendorId !== null) {
    const filter: USBDeviceFilter = { vendorId: opts.vendorId };
    if (opts.productId !== null) filter.productId = opts.productId;
    attempts.push({ note: `filters: [${JSON.stringify(filter)}] (user-specified)`, options: { filters: [filter] } });
  } else {
    attempts.push({ note: "filters: [] (broadest, if allowed by this Chromium build)", options: { filters: [] } });
    attempts.push({ note: "filters: [{}] (match-all, if allowed by this Chromium build)", options: { filters: [{}] } });
    attempts.push({
      note: "filters: [{classCode: 0x00}, {classCode: 0xff}] (fallback broad device-class match)",
      options: { filters: [{ classCode: 0x00 }, { classCode: 0xff }] },
    });
    attempts.push({
      note: "filters: [{classCode: 0xff}] (vendor-specific only fallback)",
      options: { filters: [{ classCode: 0xff }] },
    });
  }

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
    unrefBestEffort(timeout);

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
      const message = formatOneLineError(ev instanceof ErrorEvent ? ev.message : ev, 512, "WebUSB probe worker error");
      reject(new Error(message));
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

  const siteSettingsHref = (() => {
    try {
      return `chrome://settings/content/siteDetails?site=${encodeURIComponent(location.origin)}`;
    } catch {
      return "chrome://settings/content/siteDetails";
    }
  })();
  const siteSettingsLink = Object.assign(document.createElement("a"), {
    href: siteSettingsHref,
    target: "_blank",
    rel: "noopener",
    textContent: "site settings",
  });

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
    document.createElement("br"),
    "To revoke access, use “Forget permission” (if available) or remove this site's USB permissions in your browser's ",
    siteSettingsLink,
    ".",
  );

  const requestButton = document.createElement("button");
  requestButton.type = "button";
  requestButton.textContent = "Request USB device";

  const vendorIdInput = document.createElement("input");
  vendorIdInput.type = "text";
  vendorIdInput.placeholder = "VID (0x1234)";
  vendorIdInput.className = "mono";
  vendorIdInput.style.width = "140px";

  const productIdInput = document.createElement("input");
  productIdInput.type = "text";
  productIdInput.placeholder = "PID (0x5678)";
  productIdInput.className = "mono";
  productIdInput.style.width = "140px";

  const vidLabel = document.createElement("span");
  vidLabel.className = "mono";
  vidLabel.textContent = "VID:";

  const pidLabel = document.createElement("span");
  pidLabel.className = "mono";
  pidLabel.textContent = "PID:";

  const openButton = document.createElement("button");
  openButton.type = "button";
  openButton.textContent = "Open + read device descriptor";
  openButton.disabled = true;

  const listButton = document.createElement("button");
  listButton.type = "button";
  listButton.textContent = "List permitted devices (getDevices)";

  const forgetButton = document.createElement("button");
  forgetButton.type = "button";
  forgetButton.textContent = "Forget permission";
  forgetButton.hidden = true;
  forgetButton.disabled = true;

  const workerProbeButton = document.createElement("button");
  workerProbeButton.type = "button";
  workerProbeButton.textContent = "Probe worker WebUSB";

  const cloneButton = document.createElement("button");
  cloneButton.type = "button";
  cloneButton.textContent = "Send selected device to worker";

  const actions = document.createElement("div");
  actions.className = "row";
  actions.append(vidLabel, vendorIdInput, pidLabel, productIdInput, requestButton, openButton, listButton, forgetButton);

  const workerActions = document.createElement("div");
  workerActions.className = "row";
  workerActions.append(workerProbeButton, cloneButton);

  const status = document.createElement("pre");
  status.className = "mono";

  const output = document.createElement("pre");
  output.className = "mono";

  const permittedTitle = document.createElement("div");
  permittedTitle.className = "hint";
  permittedTitle.textContent = "Permitted devices (navigator.usb.getDevices()):";
  permittedTitle.hidden = true;

  const permittedList = document.createElement("ul");
  permittedList.hidden = true;

  const errorTitle = document.createElement("div");
  errorTitle.className = "bad";

  const errorDetails = document.createElement("div");
  errorDetails.className = "hint";

  const errorRaw = document.createElement("pre");
  errorRaw.className = "mono";

  const errorHints = document.createElement("ul");

  panel.append(
    title,
    note,
    actions,
    workerActions,
    status,
    output,
    permittedTitle,
    permittedList,
    errorTitle,
    errorDetails,
    errorRaw,
    errorHints,
  );

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
    const secure = (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true;
    const hasUsb = typeof navigator !== "undefined" && "usb" in navigator && !!(navigator as Navigator & { usb?: unknown }).usb;
    const userActivation = typeof navigator !== "undefined" ? navigator.userActivation : undefined;
    const ppUsb = (() => {
      if (typeof document === "undefined") return null;
      const doc = document as unknown as {
        permissionsPolicy?: { allowsFeature?: (feature: string) => boolean };
        featurePolicy?: { allowsFeature?: (feature: string) => boolean };
      };
      const policy = doc.permissionsPolicy ?? doc.featurePolicy;
      if (!policy || typeof policy.allowsFeature !== "function") return null;
      try {
        return policy.allowsFeature("usb");
      } catch {
        return null;
      }
    })();
    const ppText = ppUsb === null ? "unknown" : ppUsb ? "yes" : "no";
    const envInfo =
      `isSecureContext=${secure}\n` +
      `crossOriginIsolated=${report.crossOriginIsolated}\n` +
      `permissionsPolicy.usb=${ppText}\n` +
      `userActivation.isActive=${userActivation?.isActive ?? "n/a"}\n` +
      `userActivation.hasBeenActive=${userActivation?.hasBeenActive ?? "n/a"}\n`;

    if (!report.webusb) {
      const reason = !secure
        ? "requires a secure context (https:// or localhost)"
        : !hasUsb
          ? "navigator.usb is not available (unsupported browser or blocked by policy)"
          : "unavailable";
      status.textContent = `WebUSB: missing (${reason})\n\n${envInfo}`;
      requestButton.disabled = true;
      openButton.disabled = true;
      listButton.disabled = true;
      cloneButton.disabled = true;
      forgetButton.disabled = true;
      forgetButton.hidden = true;
      vendorIdInput.disabled = true;
      productIdInput.disabled = true;
      return;
    }

    vendorIdInput.disabled = false;
    productIdInput.disabled = false;

    if (!selected) {
      status.textContent = `WebUSB: supported. No device selected.\n\n${envInfo}`;
      openButton.disabled = true;
      listButton.disabled = false;
      cloneButton.disabled = true;
      forgetButton.disabled = true;
      forgetButton.hidden = true;
      return;
    }

    status.textContent = `Selected device: ${JSON.stringify(summarizeUsbDevice(selected))}\n\n${envInfo}`;
    openButton.disabled = false;
    listButton.disabled = false;
    cloneButton.disabled = false;

    // `USBDevice.forget()` is currently Chromium-specific; hide the action when absent.
    const canForget = canForgetUsbDevice(selected);
    forgetButton.hidden = !canForget;
    forgetButton.disabled = !canForget;
  };

  refreshStatus();

  forgetButton.onclick = async () => {
    clearError();
    output.textContent = "";

    const device = selected;
    if (!device) {
      showMessage("No device selected. Click “Request USB device” first.");
      refreshStatus();
      return;
    }

    if (!canForgetUsbDevice(device)) {
      showMessage("USBDevice.forget() is unavailable in this browser.");
      refreshStatus();
      return;
    }

    const shouldRefreshPermittedList = !permittedTitle.hidden;
    requestButton.disabled = true;
    openButton.disabled = true;
    listButton.disabled = true;
    cloneButton.disabled = true;
    forgetButton.disabled = true;
    status.textContent = "Forgetting device permission…";

    let forgot = false;
    try {
      if (device.opened) {
        try {
          await device.close();
        } catch (err) {
          console.warn("WebUSB device.close() before forget() failed", err);
        }
      }

      await device.forget();
      selected = null;
      status.textContent = "Device permission revoked.";
      forgot = true;
    } catch (err) {
      showError(err);
      console.error(err);
    } finally {
      requestButton.disabled = false;
      refreshStatus();
      if (forgot && shouldRefreshPermittedList) await refreshPermittedDevices();
    }
  };

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

      const vendorIdText = vendorIdInput.value.trim();
      const productIdText = productIdInput.value.trim();
      const vendorId = parseUsbId(vendorIdText);
      const productId = parseUsbId(productIdText);
      if (vendorIdText && vendorId === null) {
        throw new Error("Invalid vendorId. Expected hex like 0x1234.");
      }
      if (productIdText && productId === null) {
        throw new Error("Invalid productId. Expected hex like 0x5678.");
      }

      const { device, filterNote } = await requestUsbDevice(usb, { vendorId, productId });
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

  const refreshPermittedDevices = async (): Promise<void> => {
    clearError();
    output.textContent = "";
    permittedTitle.hidden = true;
    permittedList.hidden = true;
    permittedList.replaceChildren();

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

      permittedTitle.hidden = false;
      permittedList.hidden = false;
      permittedList.replaceChildren(
        ...(devices.length
          ? devices.map((device) => {
              const li = document.createElement("li");
              li.className = "row";
              const label = document.createElement("span");
              label.className = "mono";
              label.textContent = describeUsbDevice(device);

              const select = document.createElement("button");
              select.type = "button";
              select.textContent = "Select";
              select.onclick = () => {
                selected = device;
                clearError();
                output.textContent = JSON.stringify({ selected: summarizeUsbDevice(device), source: "getDevices()" }, null, 2);
                refreshStatus();
              };

              if (canForgetUsbDevice(device)) {
                const forget = document.createElement("button");
                forget.type = "button";
                forget.textContent = "Forget";
                forget.onclick = async () => {
                  clearError();
                  output.textContent = "";

                  requestButton.disabled = true;
                  openButton.disabled = true;
                  listButton.disabled = true;
                  cloneButton.disabled = true;
                  forgetButton.disabled = true;
                  status.textContent = "Forgetting device permission…";

                  let ok = false;
                  try {
                    if (device.opened) {
                      try {
                        await device.close();
                      } catch (err) {
                        console.warn("WebUSB device.close() before forget() failed", err);
                      }
                    }

                    await device.forget();
                    ok = true;

                    if (selected === device) {
                      selected = null;
                    }
                    status.textContent = "Device permission revoked.";
                  } catch (err) {
                    showError(err);
                    console.error(err);
                  } finally {
                    requestButton.disabled = false;
                    refreshStatus();
                    if (ok) {
                      await refreshPermittedDevices();
                    }
                  }
                };

                li.append(label, select, forget);
              } else {
                li.append(label, select);
              }
              return li;
            })
          : [
              Object.assign(document.createElement("li"), {
                textContent: "No permitted devices. Use “Request USB device” to grant access.",
              }),
            ]),
      );
    } catch (err) {
      showError(err);
      console.error(err);
    } finally {
      refreshStatus();
    }
  };
  listButton.onclick = refreshPermittedDevices;

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

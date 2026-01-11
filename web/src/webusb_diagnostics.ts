import "./style.css";

import {
  classifyWebUsbDevice,
  isWebUsbProtectedInterfaceClass,
  type WebUsbDeviceClassification,
} from "./platform/webusb";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<Node | string | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) continue;
    if (key === "class") {
      node.className = String(value);
    } else if (key === "text") {
      node.textContent = String(value);
    } else if (key.startsWith("on") && typeof value === "function") {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (node as any)[key.toLowerCase()] = value;
    } else {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    node.append(child instanceof Node ? child : document.createTextNode(child));
  }
  return node;
}

function fmtHex(value: number, width = 2): string {
  const clamped = Number.isFinite(value) ? value >>> 0 : 0;
  return `0x${clamped.toString(16).padStart(width, "0")}`;
}

function fmtVidPid(device: USBDevice): string {
  return `${fmtHex(device.vendorId, 4)}:${fmtHex(device.productId, 4)}`;
}

function fmtError(err: unknown): string {
  if (err instanceof DOMException) {
    return `${err.name}: ${err.message}`;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return String(err);
}

function getUsbApi(): USB | null {
  // Some environments type `navigator.usb` as `USB` even when it doesn't exist.
  // Use a runtime check.
  const maybe = (navigator as unknown as { usb?: unknown }).usb;
  return maybe && typeof maybe === "object" ? (maybe as USB) : null;
}

async function requestUsbDevice(usb: USB): Promise<{ device: USBDevice; filterNote: string }> {
  const attempts: Array<{ note: string; options: USBDeviceRequestOptions }> = [
    { note: "filters: [] (broadest, if allowed by this Chromium build)", options: { filters: [] } },
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
      if (err instanceof DOMException && err.name === "NotFoundError") {
        // User canceled the chooser (or there were no matching devices).
        throw err;
      }
      // If the filters were rejected, try the next fallback.
      if (err instanceof TypeError) continue;
      if (err instanceof DOMException && err.name === "TypeError") continue;
      throw err;
    }
  }

  throw lastErr ?? new Error("USB requestDevice failed");
}

function renderPrereqError(title: string, body: string): HTMLElement {
  return el(
    "div",
    { class: "panel missing" },
    el("h2", { text: title }),
    el("div", { class: "error", text: body }),
  );
}

function renderDeviceTree(device: USBDevice, classification: WebUsbDeviceClassification): HTMLElement {
  const claimableWarning = !classification.isClaimable
    ? el(
        "div",
        { class: "warning" },
        "This device appears to expose only protected interface classes. It may not appear in the WebUSB chooser, and ",
        "Chromium will reject attempts to claim its interfaces.",
      )
    : null;

  const configNodes: HTMLElement[] = [];
  for (const cfg of device.configurations ?? []) {
    const cfgInfo = classification.configurations.find((c) => c.configurationValue === cfg.configurationValue);
    const cfgClaimable = cfgInfo?.isClaimable ?? false;

    const cfgDetails = el("details", { open: "" });
    cfgDetails.append(
      el(
        "summary",
        {},
        el("span", { class: "mono", text: `Configuration ${cfg.configurationValue}` }),
        " ",
        el("span", { class: cfgClaimable ? "ok" : "bad", text: cfgClaimable ? "(claimable)" : "(no claimable interfaces)" }),
      ),
    );

    const ifaceList = el("div", {});
    for (const iface of cfg.interfaces) {
      const ifaceInfo = cfgInfo?.interfaces.find((i) => i.interfaceNumber === iface.interfaceNumber);
      const ifaceClaimable = ifaceInfo?.isClaimable ?? false;

      const ifaceDetails = el("details", {});
      ifaceDetails.append(
        el(
          "summary",
          {},
          el("span", { class: "mono", text: `Interface ${iface.interfaceNumber}` }),
          " ",
          el("span", { class: ifaceClaimable ? "ok" : "bad", text: ifaceClaimable ? "(claimable)" : "(protected)" }),
          iface.claimed ? el("span", { class: "mono", text: " claimed" }) : null,
        ),
      );

      const altList = el("div", {});
      for (const alt of iface.alternates) {
        const altInfo = ifaceInfo?.alternates.find((a) => a.alternateSetting === alt.alternateSetting);
        const isProtected = altInfo?.isProtected ?? isWebUsbProtectedInterfaceClass(alt.interfaceClass);
        const badge = el("span", { class: isProtected ? "bad" : "ok", text: isProtected ? "protected" : "claimable" });

        const header = el(
          "div",
          { class: "row" },
          el("span", { class: "mono", text: `Alt ${alt.alternateSetting}` }),
          badge,
          altInfo?.className ? el("span", { class: "muted", text: `(${altInfo.className})` }) : null,
          el(
            "span",
            { class: "mono" },
            `class=${fmtHex(alt.interfaceClass)} subclass=${fmtHex(alt.interfaceSubclass)} protocol=${fmtHex(
              alt.interfaceProtocol,
            )}`,
          ),
          isProtected && altInfo?.reason ? el("span", { class: "muted", text: altInfo.reason }) : null,
        );

        const endpointList = el("ul");
        for (const ep of alt.endpoints) {
          endpointList.append(
            el(
              "li",
              { class: "mono" },
              `${ep.direction.toUpperCase()} ep${ep.endpointNumber} ${ep.type} packetSize=${ep.packetSize}`,
            ),
          );
        }

        altList.append(header, endpointList);
      }

      ifaceDetails.append(altList);
      ifaceList.append(ifaceDetails);
    }

    cfgDetails.append(ifaceList);
    configNodes.push(cfgDetails);
  }

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Selected device" }),
    el(
      "div",
      { class: "row" },
      el("span", { class: "mono", text: fmtVidPid(device) }),
      el("span", { class: "mono", text: device.productName ?? "" }),
      el("span", { class: "muted", text: device.opened ? "(opened)" : "(not opened)" }),
    ),
    claimableWarning,
    el(
      "table",
      {},
      el(
        "tbody",
        {},
        el("tr", {}, el("th", { text: "manufacturerName" }), el("td", { text: device.manufacturerName ?? "(unavailable)" })),
        el("tr", {}, el("th", { text: "productName" }), el("td", { text: device.productName ?? "(unavailable)" })),
        el("tr", {}, el("th", { text: "serialNumber" }), el("td", { text: device.serialNumber ?? "(unavailable)" })),
        el(
          "tr",
          {},
          el("th", { text: "configuration" }),
          el("td", { text: device.configuration ? String(device.configuration.configurationValue) : "(none selected)" }),
        ),
      ),
    ),
    el("h3", { text: "Configurations / interfaces / endpoints" }),
    ...configNodes,
  );
}

function findFirstClaimCandidate(classification: WebUsbDeviceClassification): { configValue: number; interfaceNumber: number } | null {
  for (const cfg of classification.configurations) {
    for (const iface of cfg.interfaces) {
      if (iface.isClaimable) {
        return { configValue: cfg.configurationValue, interfaceNumber: iface.interfaceNumber };
      }
    }
  }
  return null;
}

async function tryOpenAndClaim(device: USBDevice, classification: WebUsbDeviceClassification): Promise<string> {
  const candidate = findFirstClaimCandidate(classification);
  if (!candidate) {
    throw new Error("No claimable interfaces found (all interfaces appear to be WebUSB-protected).");
  }

  await device.open();
  if (!device.configuration || device.configuration.configurationValue !== candidate.configValue) {
    await device.selectConfiguration(candidate.configValue);
  }

  await device.claimInterface(candidate.interfaceNumber);
  return `Success: opened + claimed interface ${candidate.interfaceNumber} (configuration ${candidate.configValue}).`;
}

function renderWhyDevicesDontShow(): HTMLElement {
  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Why a device might not show up" }),
    el(
      "ul",
      {},
      el("li", {}, "This page must be served from a secure context (HTTPS or http://localhost)."),
      el("li", {}, "WebUSB is Chromium-only in practice; Firefox/Safari typically do not expose navigator.usb."),
      el(
        "li",
        {},
        "The chooser only shows devices with at least one interface that is not in Chromium's protected class list ",
        "(e.g. HID, Mass Storage, Audio, Video).",
      ),
      el(
        "li",
        {},
        "Even if an interface is not protected, open/claim can still fail if the OS has an exclusive driver binding.",
      ),
    ),
    el(
      "div",
      { class: "warning" },
      "Note: requestDevice() filtering behavior varies by browser/version. This page tries a broad request first and falls back to device-class filters when required.",
    ),
  );
}

function main(): void {
  const app = document.getElementById("app");
  if (!app) throw new Error("Missing #app element");

  app.append(el("h1", { text: "WebUSB diagnostics / enumeration" }));

  if (!globalThis.isSecureContext) {
    app.append(
      renderPrereqError(
        "Secure context required",
        "window.isSecureContext is false. WebUSB is only available on HTTPS (or http://localhost).",
      ),
    );
    return;
  }

  const usb = getUsbApi();
  if (!usb) {
    app.append(
      renderPrereqError(
        "WebUSB unavailable",
        "navigator.usb is missing. WebUSB is generally only available in Chromium-based browsers (Chrome/Edge) and requires a secure context.",
      ),
    );
    return;
  }

  const log = el("pre", { class: "mono", text: "" });
  const deviceHost = el("div", {});

  let selectedDevice: USBDevice | null = null;
  let selectedClass: WebUsbDeviceClassification | null = null;
  let lastFilterNote: string | null = null;

  const renderSelected = (): void => {
    if (!selectedDevice || !selectedClass) {
      deviceHost.replaceChildren();
      return;
    }
    const children: Node[] = [];
    if (lastFilterNote) {
      children.push(
        el("div", { class: "panel" }, el("div", { class: "mono", text: `requestDevice() used: ${lastFilterNote}` })),
      );
    }
    children.push(renderDeviceTree(selectedDevice, selectedClass));
    deviceHost.replaceChildren(...children);
  };

  const requestBtn = el("button", { text: "Request USB device" }) as HTMLButtonElement;
  const claimBtn = el("button", { text: "Try open + claim (first claimable interface)", disabled: "true" }) as HTMLButtonElement;

  requestBtn.onclick = () => {
    void (async () => {
      log.textContent = "";
      try {
        const { device, filterNote } = await requestUsbDevice(usb);
        selectedDevice = device;
        selectedClass = classifyWebUsbDevice(device);
        lastFilterNote = filterNote;
        claimBtn.disabled = false;

        renderSelected();
      } catch (err) {
        const msg = fmtError(err);
        log.textContent = msg;
        console.error(err);
      }
    })();
  };

  claimBtn.onclick = () => {
    void (async () => {
      log.textContent = "";
      if (!selectedDevice || !selectedClass) {
        log.textContent = "No device selected.";
        return;
      }
      try {
        const result = await tryOpenAndClaim(selectedDevice, selectedClass);
        log.textContent = result;
        // Re-render so `opened`/`claimed` state updates.
        selectedClass = classifyWebUsbDevice(selectedDevice);
        renderSelected();
      } catch (err) {
        const msg = fmtError(err);
        log.textContent = msg;
        console.error(err);
      }
    })();
  };

  app.append(
    renderWhyDevicesDontShow(),
    el(
      "div",
      { class: "panel" },
      el("h2", { text: "Actions" }),
      el("div", { class: "row actions" }, requestBtn, claimBtn),
      log,
    ),
    deviceHost,
  );
}

main();

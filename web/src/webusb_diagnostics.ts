import "./style.css";

import {
  classifyWebUsbDevice,
  isWebUsbProtectedInterfaceClass,
  type WebUsbDeviceClassification,
} from "./platform/webusb";
import { explainWebUsbError, formatWebUsbError } from "./platform/webusb_troubleshooting";

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
  return formatWebUsbError(err);
}

function deviceSummaryLabel(device: USBDevice): string {
  const name = device.productName || device.manufacturerName || "USB device";
  return `${fmtVidPid(device)} ${name}`;
}

function getUsbApi(): USB | null {
  // Some environments type `navigator.usb` as `USB` even when it doesn't exist.
  // Use a runtime check.
  const maybe = (navigator as unknown as { usb?: unknown }).usb;
  return maybe && typeof maybe === "object" ? (maybe as USB) : null;
}

function permissionsPolicyAllowsUsb(): boolean | null {
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
}

async function requestUsbDevice(usb: USB): Promise<{ device: USBDevice; filterNote: string }> {
  const attempts: Array<{ note: string; options: USBDeviceRequestOptions }> = [
    // Chromium currently requires a non-empty filter list; `{}` matches any device
    // that is eligible for WebUSB (subject to the protected interface class rules).
    { note: "filters: [{}] (broadest; matches any eligible device)", options: { filters: [{}] } },
    // Some Chromium builds historically accepted an empty filter list; keep this
    // as a fallback since the page is explicitly diagnostic.
    { note: "filters: [] (empty list; accepted by some Chromium builds)", options: { filters: [] } },
    {
      note: "filters: [{classCode: 0xff}] (vendor-specific only fallback)",
      options: { filters: [{ classCode: 0xff }] },
    },
    {
      // Rare fallback: some stacks require at least one filter and reject empty filter objects.
      // This is intentionally broad, but still helps catch some composite/vendored devices.
      note: "filters: [{classCode: 0x00}, {classCode: 0xff}] (broad device-class match fallback)",
      options: { filters: [{ classCode: 0x00 }, { classCode: 0xff }] },
    },
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

function renderWhyDevicesDontShow(ppUsb: boolean | null): HTMLElement {
  const ppText = ppUsb === null ? "unknown" : ppUsb ? "yes" : "no";
  const ppClass = ppUsb === null ? "muted" : ppUsb ? "ok" : "bad";

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Why a device might not show up" }),
    el(
      "ul",
      {},
      el(
        "li",
        {},
        "Permissions-Policy allows usb: ",
        el("span", { class: ppClass, text: ppText }),
        ppUsb === false ? " (check response header policy + iframe allow=\"usb\")" : null,
      ),
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
      el(
        "li",
        {},
        "On Windows, WebUSB often requires the device interface to be bound to WinUSB (development: Zadig; production: Microsoft OS 2.0 descriptors / WCID).",
      ),
    ),
    el(
      "div",
      { class: "warning" },
      "Note: requestDevice() filtering behavior varies by browser/version. This page tries a broad request first and falls back to device-class filters when required.",
    ),
  );
}

function snapshotDeviceDescriptors(device: USBDevice): unknown {
  return {
    vendorId: device.vendorId,
    productId: device.productId,
    productName: device.productName ?? null,
    manufacturerName: device.manufacturerName ?? null,
    serialNumber: device.serialNumber ?? null,
    opened: device.opened,
    selectedConfigurationValue: device.configuration?.configurationValue ?? null,
    configurations: (device.configurations ?? []).map((cfg) => ({
      configurationValue: cfg.configurationValue,
      interfaces: (cfg.interfaces ?? []).map((iface) => ({
        interfaceNumber: iface.interfaceNumber,
        claimed: iface.claimed,
        alternates: (iface.alternates ?? []).map((alt) => ({
          alternateSetting: alt.alternateSetting,
          interfaceClass: alt.interfaceClass,
          interfaceSubclass: alt.interfaceSubclass,
          interfaceProtocol: alt.interfaceProtocol,
          endpoints: (alt.endpoints ?? []).map((ep) => ({
            endpointNumber: ep.endpointNumber,
            direction: ep.direction,
            type: ep.type,
            packetSize: ep.packetSize,
          })),
        })),
      })),
    })),
  };
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

  const ppUsb = permissionsPolicyAllowsUsb();
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

  const status = el("pre", { class: "mono", text: "" });
  const errorTitle = el("div", { class: "bad", text: "" });
  const errorDetails = el("div", { class: "hint", text: "" });
  const errorRaw = el("pre", { class: "mono", text: "" });
  const errorHints = el("ul");
  const eventLog = el("pre", { class: "mono", text: "" });
  const deviceHost = el("div", {});
  const knownDevicesHost = el("div", {});

  let selectedDevice: USBDevice | null = null;
  let selectedClass: WebUsbDeviceClassification | null = null;
  let lastFilterNote: string | null = null;

  function appendEvent(line: string): void {
    eventLog.textContent = `${eventLog.textContent ?? ""}${line}\n`;
    eventLog.scrollTop = eventLog.scrollHeight;
  }

  function clearError(): void {
    errorTitle.textContent = "";
    errorDetails.textContent = "";
    errorRaw.textContent = "";
    errorHints.replaceChildren();
  }

  function showError(err: unknown): void {
    const explained = explainWebUsbError(err);
    errorTitle.textContent = explained.title;
    errorDetails.textContent = explained.details ?? "";
    errorRaw.textContent = fmtError(err);
    errorHints.replaceChildren(...explained.hints.map((h) => el("li", { text: h })));
  }

  async function refreshKnownDevices(): Promise<void> {
    if (typeof usb.getDevices !== "function") {
      knownDevicesHost.replaceChildren(
        el("div", { class: "muted", text: "navigator.usb.getDevices() is unavailable in this browser." }),
      );
      return;
    }

    const devices = await usb.getDevices();
    if (devices.length === 0) {
      knownDevicesHost.replaceChildren(el("div", { class: "muted", text: "No previously granted WebUSB devices found." }));
      return;
    }

    const select = el("select") as HTMLSelectElement;
    for (const dev of devices) {
      const option = el("option", { value: fmtVidPid(dev), text: deviceSummaryLabel(dev) }) as HTMLOptionElement;
      select.append(option);
    }

    const useBtn = el("button", { text: "Use selected device" }) as HTMLButtonElement;
    useBtn.onclick = () => {
      clearError();
      const idx = select.selectedIndex;
      if (idx < 0 || idx >= devices.length) return;
      selectedDevice = devices[idx];
      selectedClass = classifyWebUsbDevice(selectedDevice);
      lastFilterNote = "getDevices() (previously granted permission)";
      claimBtn.disabled = false;
      copyBtn.disabled = false;
      renderSelected();
    };

    knownDevicesHost.replaceChildren(el("div", { class: "row" }, select, useBtn));
  }

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
  const copyBtn = el("button", { text: "Copy JSON summary", disabled: "true" }) as HTMLButtonElement;
  const refreshKnownBtn = el("button", { text: "Refresh granted devices" }) as HTMLButtonElement;

  copyBtn.onclick = () => {
    void (async () => {
      status.textContent = "";
      clearError();
      if (!selectedDevice || !selectedClass) {
        status.textContent = "No device selected.";
        return;
      }

      const payload = {
        generatedAt: new Date().toISOString(),
        filterNote: lastFilterNote,
        device: snapshotDeviceDescriptors(selectedDevice),
        classification: selectedClass,
      };
      const text = JSON.stringify(payload, null, 2);

      try {
        if (!navigator.clipboard?.writeText) {
          throw new Error("Clipboard API unavailable (navigator.clipboard.writeText).");
        }
        await navigator.clipboard.writeText(text);
        status.textContent = "Copied JSON summary to clipboard.";
      } catch (err) {
        showError(err);
      }
    })();
  };

  refreshKnownBtn.onclick = () => {
    void (async () => {
      try {
        await refreshKnownDevices();
      } catch (err) {
        showError(err);
      }
    })();
  };

  requestBtn.onclick = () => {
    void (async () => {
      status.textContent = "";
      clearError();
      try {
        const { device, filterNote } = await requestUsbDevice(usb);
        selectedDevice = device;
        selectedClass = classifyWebUsbDevice(device);
        lastFilterNote = filterNote;
        claimBtn.disabled = false;
        copyBtn.disabled = false;

        renderSelected();
      } catch (err) {
        showError(err);
        console.error(err);
      }
    })();
  };

  claimBtn.onclick = () => {
    void (async () => {
      status.textContent = "";
      clearError();
      if (!selectedDevice || !selectedClass) {
        status.textContent = "No device selected.";
        return;
      }
      try {
        const result = await tryOpenAndClaim(selectedDevice, selectedClass);
        status.textContent = result;
        // Re-render so `opened`/`claimed` state updates.
        selectedClass = classifyWebUsbDevice(selectedDevice);
        renderSelected();
      } catch (err) {
        showError(err);
        console.error(err);
      }
    })();
  };

  usb.addEventListener("connect", (event) => {
    const dev = (event as USBConnectionEvent).device;
    appendEvent(`connect: ${deviceSummaryLabel(dev)}`);
    void refreshKnownDevices();
  });
  usb.addEventListener("disconnect", (event) => {
    const dev = (event as USBConnectionEvent).device;
    appendEvent(`disconnect: ${deviceSummaryLabel(dev)}`);
    void refreshKnownDevices();
  });

  appendEvent("listening for navigator.usb connect/disconnect events…");
  void refreshKnownDevices();

  app.append(
    renderWhyDevicesDontShow(ppUsb),
    el(
      "div",
      { class: "panel" },
      el("h2", { text: "Actions" }),
      el("div", { class: "row actions" }, requestBtn, claimBtn, copyBtn),
      status,
      errorTitle,
      errorDetails,
      errorRaw,
      errorHints,
    ),
    el(
      "div",
      { class: "panel" },
      el("h2", { text: "Previously granted devices" }),
      el("div", { class: "row" }, refreshKnownBtn),
      knownDevicesHost,
    ),
    el("div", { class: "panel" }, el("h2", { text: "USB connect/disconnect log" }), eventLog),
    deviceHost,
  );
}

main();

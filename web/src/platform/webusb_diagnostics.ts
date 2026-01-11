import { explainWebUsbError, formatWebUsbError } from "./webusb_troubleshooting";

type UsbLike = Pick<USB, "getDevices" | "requestDevice" | "addEventListener" | "removeEventListener">;

export type WebUsbDiagnosticsState = {
  supported: boolean;
  knownDevices: USBDevice[];
  selectedDevice: USBDevice | null;
};

function getNavigatorUsb(): UsbLike | null {
  if (typeof navigator === "undefined") return null;
  const maybe = (navigator as Navigator & { usb?: unknown }).usb;
  if (!maybe) return null;
  return maybe as UsbLike;
}

function hex4(n: number): string {
  return n.toString(16).padStart(4, "0");
}

function describeDevice(device: USBDevice): string {
  const id = `${hex4(device.vendorId)}:${hex4(device.productId)}`;
  const name = device.productName || device.manufacturerName || `device (${id})`;
  return `${name} [${id}]`;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<HTMLElement | string | null | undefined>
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

function interfaceLabel(iface: USBInterface): string {
  const alt = iface.alternates?.[0];
  if (!alt) return `#${iface.interfaceNumber}`;
  const cls = alt.interfaceClass.toString(16).padStart(2, "0");
  const sub = alt.interfaceSubclass.toString(16).padStart(2, "0");
  const proto = alt.interfaceProtocol.toString(16).padStart(2, "0");
  return `#${iface.interfaceNumber} (class 0x${cls} / sub 0x${sub} / proto 0x${proto})`;
}

export function mountWebUsbDiagnosticsPanel(host: HTMLElement): () => void {
  const usb = getNavigatorUsb();
  let selectedDevice: USBDevice | null = null;
  let knownDevices: USBDevice[] = [];

  const hint = el(
    "div",
    { class: "mono" },
    "WebUSB permissions persist per-origin. To revoke access, use your browser's site settings and remove USB device permissions for this site.",
  );

  const status = el("pre", { text: "" });

  const errorTitle = el("div", { class: "bad", text: "" });
  const errorDetails = el("div", { class: "hint", text: "" });
  const errorRaw = el("pre", { class: "mono", text: "" });
  const errorHints = el("ul");

  const requestButton = el("button", { text: "Request device…" }) as HTMLButtonElement;
  const refreshButton = el("button", { text: "Refresh known devices" }) as HTMLButtonElement;

  const openButton = el("button", { text: "Open" }) as HTMLButtonElement;
  const closeButton = el("button", { text: "Close" }) as HTMLButtonElement;

  const interfaceSelect = el("select") as HTMLSelectElement;
  const claimButton = el("button", { text: "Claim interface" }) as HTMLButtonElement;

  const knownList = el("ul");
  const selectedInfo = el("div", { class: "mono", text: "selected: (none)" });

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
    errorRaw.textContent = formatWebUsbError(err);
    errorHints.replaceChildren(...explained.hints.map((h) => el("li", { text: h })));
  }

  async function refreshKnownDevices(): Promise<void> {
    if (!usb) {
      knownDevices = [];
      render();
      return;
    }
    try {
      knownDevices = await usb.getDevices();
    } catch (err) {
      // Some environments expose `navigator.usb` but deny access via policy.
      console.warn("WebUSB getDevices() failed", err);
      knownDevices = [];
    }
    render();
  }

  function updateSelectedUi(): void {
    const dev = selectedDevice;
    if (!dev) {
      selectedInfo.textContent = "selected: (none)";
      interfaceSelect.replaceChildren(el("option", { value: "", text: "(no device selected)" }));
      return;
    }

    selectedInfo.textContent = `selected: ${describeDevice(dev)} opened=${dev.opened ? "yes" : "no"}`;

    const config = dev.configuration;
    if (!config) {
      interfaceSelect.replaceChildren(el("option", { value: "", text: "(no configuration selected)" }));
      return;
    }

    const ifaces = config.interfaces ?? [];
    interfaceSelect.replaceChildren(
      ...(ifaces.length
        ? ifaces.map((iface) => el("option", { value: String(iface.interfaceNumber), text: interfaceLabel(iface) }))
        : [el("option", { value: "", text: "(no interfaces in configuration)" })]),
    );
  }

  function render(): void {
    requestButton.disabled = !usb;
    refreshButton.disabled = !usb;

    openButton.disabled = !selectedDevice || !usb;
    closeButton.disabled = !selectedDevice || !usb;
    claimButton.disabled = !selectedDevice || !usb;
    interfaceSelect.disabled = !selectedDevice || !usb;

    if (!usb) {
      knownList.replaceChildren(el("li", { text: "WebUSB is not available in this browser/context." }));
      updateSelectedUi();
      return;
    }

    knownList.replaceChildren(
      ...(knownDevices.length
        ? knownDevices.map((device) =>
            el(
              "li",
              {},
              el("span", { text: describeDevice(device) }),
              el("button", {
                text: "Select",
                onclick: () => {
                  clearError();
                  status.textContent = "";
                  selectedDevice = device;
                  updateSelectedUi();
                },
              }),
            ),
          )
        : [el("li", { text: "No known devices. Use “Request device…” to grant access." })]),
    );

    updateSelectedUi();
  }

  requestButton.onclick = async () => {
    clearError();
    status.textContent = "";
    if (!usb) return;

    try {
      // Some Chromium versions reject `filters: []` outright. Using a single empty
      // filter is a best-effort "match all" request for diagnostics purposes.
      selectedDevice = await usb.requestDevice({ filters: [{} as USBDeviceFilter] });
      status.textContent = `Selected ${describeDevice(selectedDevice)}.`;
      await refreshKnownDevices();
    } catch (err) {
      showError(err);
    } finally {
      updateSelectedUi();
    }
  };

  refreshButton.onclick = async () => {
    clearError();
    status.textContent = "";
    await refreshKnownDevices();
  };

  openButton.onclick = async () => {
    clearError();
    status.textContent = "";
    const dev = selectedDevice;
    if (!dev) return;

    try {
      await dev.open();
      status.textContent = "Device opened.";
    } catch (err) {
      showError(err);
    } finally {
      updateSelectedUi();
    }
  };

  closeButton.onclick = async () => {
    clearError();
    status.textContent = "";
    const dev = selectedDevice;
    if (!dev) return;

    try {
      await dev.close();
      status.textContent = "Device closed.";
    } catch (err) {
      showError(err);
    } finally {
      updateSelectedUi();
    }
  };

  claimButton.onclick = async () => {
    clearError();
    status.textContent = "";
    const dev = selectedDevice;
    if (!dev) return;

    // 1) Open
    if (!dev.opened) {
      try {
        await dev.open();
      } catch (err) {
        showError(err);
        updateSelectedUi();
        return;
      }
    }

    // 2) Select configuration (best-effort)
    if (!dev.configuration) {
      try {
        const cfg = dev.configurations?.[0]?.configurationValue ?? 1;
        await dev.selectConfiguration(cfg);
      } catch (err) {
        showError(err);
        updateSelectedUi();
        return;
      }
    }

    // 3) Claim
    updateSelectedUi();
    const ifaceNumber = Number(interfaceSelect.value);
    if (!Number.isFinite(ifaceNumber)) {
      status.textContent = "Select an interface first.";
      return;
    }

    try {
      await dev.claimInterface(ifaceNumber);
      status.textContent = `Claimed interface ${ifaceNumber}.`;
    } catch (err) {
      showError(err);
    }
  };

  const onConnect = () => void refreshKnownDevices();
  const onDisconnect = () => void refreshKnownDevices();

  if (usb) {
    usb.addEventListener("connect", onConnect);
    usb.addEventListener("disconnect", onDisconnect);
  }

  host.replaceChildren(
    el("h3", { text: "WebUSB diagnostics" }),
    hint,
    el("div", { class: "row" }, requestButton, refreshButton),
    el("h4", { text: "Known devices" }),
    knownList,
    el("h4", { text: "Selected device" }),
    selectedInfo,
    el("div", { class: "row" }, openButton, closeButton, interfaceSelect, claimButton),
    status,
    errorTitle,
    errorDetails,
    errorRaw,
    errorHints,
  );

  render();
  void refreshKnownDevices();

  return () => {
    if (usb) {
      usb.removeEventListener("connect", onConnect);
      usb.removeEventListener("disconnect", onDisconnect);
    }
  };
}

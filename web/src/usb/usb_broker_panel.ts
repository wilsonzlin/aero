import { explainWebUsbError, formatWebUsbError } from "../platform/webusb_troubleshooting";
import type { UsbBroker } from "./usb_broker";
import { hex16 } from "./usb_hex";
import { isUsbGuestWebUsbStatusMessage, isUsbSelectedMessage, type UsbGuestWebUsbSnapshot } from "./usb_proxy_protocol";
import { unrefBestEffort } from "../unrefSafe";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<HTMLElement | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) continue;
    if (key === "class") {
      node.className = String(value);
    } else if (key === "text") {
      node.textContent = String(value);
    } else if (key.startsWith("on") && typeof value === "function") {
      (node as unknown as Record<string, unknown>)[key.toLowerCase()] = value;
    } else {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children) {
    if (!child) continue;
    node.append(child);
  }
  return node;
}

function describeUsbDevice(device: USBDevice): string {
  const parts = [`${hex16(device.vendorId)}:${hex16(device.productId)}`];
  if (device.manufacturerName) parts.push(device.manufacturerName);
  if (device.productName) parts.push(device.productName);
  if (device.serialNumber) parts.push(`sn=${device.serialNumber}`);
  return parts.join(" ");
}

type SelectedInfo = { vendorId: number; productId: number; productName?: string };

export function renderWebUsbBrokerPanel(broker: UsbBroker): HTMLElement {
  const supported = typeof navigator !== "undefined" && "usb" in navigator && !!(navigator as Navigator & { usb?: unknown }).usb;

  const knownList = el("ul");
  const status = el("pre", { text: "" });
  const guestStatus = el("pre", { text: "Guest-visible passthrough: (waiting for I/O worker…)" });

  const errorTitle = el("div", { class: "bad", text: "" });
  const errorDetails = el("div", { class: "hint", text: "" });
  const errorRaw = el("pre", { class: "mono", text: "" });
  const errorHints = el("ul");

  const clearError = (): void => {
    errorTitle.textContent = "";
    errorDetails.textContent = "";
    errorRaw.textContent = "";
    errorHints.replaceChildren();
  };

  const showError = (err: unknown): void => {
    const explained = explainWebUsbError(err);
    errorTitle.textContent = explained.title;
    errorDetails.textContent = explained.details ?? "";
    errorRaw.textContent = formatWebUsbError(err);
    errorHints.replaceChildren(...explained.hints.map((h) => el("li", { text: h })));
  };

  const detachButton = el("button", {
    text: "Detach",
    disabled: "true",
    onclick: async () => {
      clearError();
      try {
        await broker.detachSelectedDevice("WebUSB device detached.");
      } catch (err) {
        showError(err);
      }
    },
  }) as HTMLButtonElement;

  const forgetButton = el("button", {
    text: "Forget selected device",
    disabled: "true",
    onclick: async () => {
      clearError();
      try {
        await broker.forgetSelectedDevice();
        await refreshKnownDevices();
      } catch (err) {
        showError(err);
      } finally {
        updateForgetButton();
      }
    },
  }) as HTMLButtonElement;
  forgetButton.hidden = true;

  let selected: SelectedInfo | null = null;
  let guestSnapshot: UsbGuestWebUsbSnapshot | null = null;

  const updateGuestStatus = (): void => {
    const snap = guestSnapshot;
    if (!snap) {
      guestStatus.textContent = "Guest-visible passthrough: (waiting for I/O worker…)";
      return;
    }

    const root = snap.controllerKind ? `${snap.controllerKind} rootPort=${snap.rootPort}` : `rootPort=${snap.rootPort}`;
    const state = !snap.available
      ? "unavailable"
      : snap.attached
        ? "attached"
        : snap.blocked
          ? "blocked"
          : "detached";
    const lines = [`Guest-visible passthrough: ${state} (${root})`];
    if (snap.lastError) lines.push(`lastError: ${snap.lastError}`);
    guestStatus.textContent = lines.join("\n");
  };

  const updateForgetButton = (): void => {
    if (!supported || !selected) {
      forgetButton.hidden = true;
      forgetButton.disabled = true;
      return;
    }

    const canForget = broker.canForgetSelectedDevice();
    forgetButton.hidden = !canForget;
    forgetButton.disabled = !canForget;
  };

  const setStatus = (info: SelectedInfo | null): void => {
    selected = info;
    if (!info) {
      status.textContent = "Selected device: (none)";
      detachButton.disabled = true;
      updateForgetButton();
      return;
    }
    status.textContent = `Selected device: ${info.productName ? `${info.productName} ` : ""}(vid=${hex16(info.vendorId)} pid=${hex16(info.productId)})`;
    detachButton.disabled = false;
    updateForgetButton();
  };

  const renderKnownDevices = (devices: USBDevice[]): void => {
    knownList.replaceChildren(
      ...(devices.length
        ? devices.map((device) =>
            el(
              "li",
              { class: "row" },
              el("span", { class: "mono", text: describeUsbDevice(device) }),
              el("button", {
                text: "Attach",
                disabled: supported ? undefined : "true",
                onclick: async () => {
                  clearError();
                  try {
                    await broker.attachKnownDevice(device);
                  } catch (err) {
                    showError(err);
                  }
                },
              }),
            ),
          )
        : [el("li", { text: "No known devices. Use “Select WebUSB device…” to grant access." })]),
    );
  };

  let refreshPending = false;
  const refreshKnownDevices = async (): Promise<void> => {
    if (!supported) {
      renderKnownDevices([]);
      return;
    }
    if (refreshPending) return;
    refreshPending = true;
    try {
      const devices = await broker.getKnownDevices();
      renderKnownDevices(devices);
    } catch (err) {
      renderKnownDevices([]);
      showError(err);
    } finally {
      refreshPending = false;
    }
  };

  const selectButton = el("button", {
    text: "Select WebUSB device…",
    disabled: supported ? undefined : "true",
    onclick: async () => {
      clearError();
      try {
        await broker.requestDevice();
        await refreshKnownDevices();
      } catch (err) {
        showError(err);
      }
    },
  }) as HTMLButtonElement;

  const refreshButton = el("button", {
    text: "Refresh known devices",
    disabled: supported ? undefined : "true",
    onclick: () => {
      clearError();
      void refreshKnownDevices();
    },
  }) as HTMLButtonElement;

  const controllerModeSelect = el(
    "select",
    {
      onchange: () => {
        try {
          const value = controllerModeSelect.value;
          if (value === "uhci" || value === "ehci") {
            broker.setGuestControllerMode(value);
          }
        } catch {
          // ignore
        }
      },
    },
    el("option", { value: "uhci", text: "UHCI (full-speed)" }),
    el("option", { value: "ehci", text: "EHCI (high-speed)" }),
  ) as HTMLSelectElement;
  try {
    controllerModeSelect.value = broker.getGuestControllerMode();
  } catch {
    controllerModeSelect.value = "uhci";
  }

  setStatus(null);

  if (!supported) {
    errorTitle.textContent = "WebUSB is not available in this browser context.";
    renderKnownDevices([]);
  } else {
    if (typeof MessageChannel !== "undefined") {
      // Keep the panel in sync with broker selection/disconnect events by attaching a MessagePort.
      const channel = new MessageChannel();
      broker.attachWorkerPort(channel.port1, { attachRings: false });
      channel.port2.addEventListener("message", (ev: MessageEvent<unknown>) => {
        const data = ev.data;
        if (isUsbSelectedMessage(data)) {
          if (data.ok) {
            setStatus(data.info);
            clearError();
          } else {
            setStatus(null);
            if (data.error) {
              showError(data.error);
            }
          }
          return;
        }

        if (isUsbGuestWebUsbStatusMessage(data)) {
          guestSnapshot = data.snapshot;
          updateGuestStatus();
        }
      });
      channel.port2.start();
      // Node's MessagePort keeps the event loop alive once started. Unit tests run in
      // the `node` environment; unref to avoid leaking handles.
      try {
        unrefBestEffort(channel.port1);
        unrefBestEffort(channel.port2);
      } catch {
        // ignore
      }
    }

    broker.subscribeToDeviceChanges(() => {
      void refreshKnownDevices();
    });

    void refreshKnownDevices();
  }

  const origin = (globalThis as unknown as { location?: { origin?: unknown } }).location?.origin;
  const encodedOrigin = typeof origin === "string" ? encodeURIComponent(origin) : "";
  const siteSettingsLink = el("a", {
    href: `chrome://settings/content/siteDetails?site=${encodedOrigin}`,
    target: "_blank",
    rel: "noopener",
    text: "site settings",
  });
  const hint = el(
    "div",
    { class: "mono" },
    el("span", {
      text:
        "WebUSB permissions persist per-origin; devices granted via the chooser will appear under “Known devices”. " +
        "Some Chromium builds support revoking permissions via “Forget selected device”; otherwise, use your browser's ",
    }),
    siteSettingsLink,
    el("span", {
      text: ". The main thread owns WebUSB; workers send usb.action messages and receive usb.completion replies.",
    }),
  );

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "WebUSB passthrough broker" }),
    hint,
    el("div", { class: "row" }, el("span", { text: "Guest controller:" }), controllerModeSelect),
    el("div", { class: "row" }, selectButton, refreshButton),
    el("div", { class: "row" }, detachButton, forgetButton),
    status,
    guestStatus,
    el("h4", { text: "Known devices (navigator.usb.getDevices())" }),
    knownList,
    errorTitle,
    errorDetails,
    errorRaw,
    errorHints,
  );
}

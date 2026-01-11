import { fnv1a32Hex } from "../utils/fnv1a";
import type { GuestUsbPort, HidPassthroughMessage } from "./hid_passthrough_protocol";

export interface HidPassthroughTarget {
  postMessage(message: HidPassthroughMessage): void;
}

export const UHCI_ROOT_PORTS: readonly GuestUsbPort[] = [0, 1];

const NO_FREE_PORTS_MESSAGE = `No free guest USB ports (${UHCI_ROOT_PORTS.length} total). Detach an existing device first.`;

export function getNoFreeGuestUsbPortsMessage(): string {
  return NO_FREE_PORTS_MESSAGE;
}

export type WebHidPassthroughAttachment = {
  device: HIDDevice;
  deviceId: string;
  guestPort: GuestUsbPort;
};

export type WebHidPassthroughState = {
  supported: boolean;
  knownDevices: HIDDevice[];
  attachedDevices: WebHidPassthroughAttachment[];
};

export type WebHidPassthroughListener = (state: WebHidPassthroughState) => void;

type HidLike = Pick<HID, "getDevices" | "requestDevice" | "addEventListener" | "removeEventListener">;

function getNavigatorHid(): HidLike | null {
  if (typeof navigator === "undefined") return null;
  const maybe = (navigator as Navigator & { hid?: unknown }).hid;
  if (!maybe) return null;
  return maybe as HidLike;
}

function describeDevice(device: HIDDevice): string {
  const name = device.productName || `device (${device.vendorId.toString(16)}:${device.productId.toString(16)})`;
  return `${name} [${device.vendorId.toString(16).padStart(4, "0")}:${device.productId.toString(16).padStart(4, "0")}]`;
}

const NOOP_TARGET: HidPassthroughTarget = { postMessage: () => {} };

export class WebHidPassthroughManager {
  readonly #hid: HidLike | null;
  readonly #target: HidPassthroughTarget;

  #knownDevices: HIDDevice[] = [];
  #attachedDevices: WebHidPassthroughAttachment[] = [];
  readonly #listeners = new Set<WebHidPassthroughListener>();

  readonly #devicePorts = new Map<string, GuestUsbPort>();
  readonly #usedPorts = new Set<GuestUsbPort>();

  readonly #deviceIds = new WeakMap<HIDDevice, string>();
  #nextDeviceOrdinal = 1;

  readonly #onConnect: ((event: Event) => void) | null;
  readonly #onDisconnect: ((event: Event) => void) | null;

  constructor(options: { hid?: HidLike | null; target?: HidPassthroughTarget } = {}) {
    this.#hid = options.hid ?? getNavigatorHid();
    this.#target = options.target ?? NOOP_TARGET;

    if (this.#hid) {
      this.#onConnect = () => {
        void this.refreshKnownDevices();
      };

      this.#onDisconnect = (event: Event) => {
        void this.#handleDisconnect(event);
      };

      this.#hid.addEventListener("connect", this.#onConnect);
      this.#hid.addEventListener("disconnect", this.#onDisconnect);
    } else {
      this.#onConnect = null;
      this.#onDisconnect = null;
    }
  }

  destroy(): void {
    if (this.#hid && this.#onConnect && this.#onDisconnect) {
      this.#hid.removeEventListener("connect", this.#onConnect);
      this.#hid.removeEventListener("disconnect", this.#onDisconnect);
    }
    this.#listeners.clear();
  }

  getState(): WebHidPassthroughState {
    return {
      supported: !!this.#hid,
      knownDevices: this.#knownDevices,
      attachedDevices: this.#attachedDevices,
    };
  }

  subscribe(listener: WebHidPassthroughListener): () => void {
    this.#listeners.add(listener);
    listener(this.getState());
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async refreshKnownDevices(): Promise<void> {
    if (!this.#hid) {
      this.#knownDevices = [];
      this.#emit();
      return;
    }

    try {
      this.#knownDevices = await this.#hid.getDevices();
    } catch (err) {
      // Browsers may throw when WebHID is disabled by policy/flags. Treat this as
      // "supported but unavailable" rather than crashing the UI.
      console.warn("WebHID getDevices() failed", err);
      this.#knownDevices = [];
    }

    this.#emit();
  }

  async requestAndAttachDevice(filters: HIDDeviceFilter[] = []): Promise<void> {
    if (!this.#hid) {
      throw new Error("WebHID is unavailable in this browser.");
    }

    const devices = await this.#hid.requestDevice({ filters });
    for (const device of devices) {
      await this.attachKnownDevice(device);
    }

    // `requestDevice()` also grants permissions; refresh so the "known devices"
    // list stays in sync across browsers.
    await this.refreshKnownDevices();
  }

  async attachKnownDevice(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIdFor(device);
    if (this.#devicePorts.has(deviceId)) {
      return;
    }

    const port = this.#allocatePort();
    if (port === undefined) {
      throw new Error(NO_FREE_PORTS_MESSAGE);
    }

    await device.open();

    try {
      this.#target.postMessage({ type: "hid:attach", deviceId, guestPort: port });
    } catch (err) {
      try {
        await device.close();
      } catch {
        // Ignore close failures when attach fails.
      }
      throw err;
    }

    this.#devicePorts.set(deviceId, port);
    this.#usedPorts.add(port);
    this.#attachedDevices = [...this.#attachedDevices, { device, deviceId, guestPort: port }].sort(
      (a, b) => a.guestPort - b.guestPort,
    );
    this.#emit();
  }

  async detachDevice(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIds.get(device);
    if (!deviceId) return;

    const port = this.#devicePorts.get(deviceId);
    if (port === undefined) return;

    let detachError: unknown | null = null;
    try {
      this.#target.postMessage({ type: "hid:detach", deviceId, guestPort: port });
    } catch (err) {
      detachError = err;
    }

    this.#devicePorts.delete(deviceId);
    this.#usedPorts.delete(port);
    this.#attachedDevices = this.#attachedDevices.filter((d) => d.deviceId !== deviceId);
    this.#emit();

    try {
      await device.close();
    } catch (err) {
      console.warn("WebHID device.close() failed", err);
    }

    if (detachError) {
      throw detachError;
    }
  }

  #emit(): void {
    const state = this.getState();
    for (const listener of this.#listeners) listener(state);
  }

  #allocatePort(): GuestUsbPort | undefined {
    for (const port of UHCI_ROOT_PORTS) {
      if (!this.#usedPorts.has(port)) return port;
    }
    return undefined;
  }

  #deviceIdFor(device: HIDDevice): string {
    const existing = this.#deviceIds.get(device);
    if (existing) return existing;

    const base = `${device.vendorId}:${device.productId}:${device.productName ?? ""}`;
    const hash = fnv1a32Hex(new TextEncoder().encode(base));
    const id = `${hash}-${this.#nextDeviceOrdinal++}`;
    this.#deviceIds.set(device, id);
    return id;
  }

  async #handleDisconnect(event: Event): Promise<void> {
    const dev = (event as unknown as HIDConnectionEvent).device;
    if (dev) {
      try {
        await this.detachDevice(dev);
      } catch {
        // Ignore detach failures on disconnect.
      }
    }
    await this.refreshKnownDevices();
  }
}

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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (node as any)[key.toLowerCase()] = value;
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

/**
 * Minimal passthrough UI for debugging / manual testing.
 *
 * Note: Unit tests run in the `node` environment. The DOM interactions are
 * intentionally simple so tests can stub out `document.createElement`.
 */
export function mountWebHidPassthroughPanel(host: HTMLElement, manager: WebHidPassthroughManager): () => void {
  const portHint = el("div", {
    class: "mono",
    text:
      "Guest UHCI root hub currently exposes only 2 ports (0 and 1). " +
      "Virtual hub / additional controller support is planned for more devices.",
  });

  const permissionHint = el("div", {
    class: "mono",
    text: "WebHID permissions persist per-origin. To revoke access, use your browser's site settings and remove HID device permissions for this site.",
  });

  const error = el("pre", { text: "" });

  const requestButton = el("button", {
    text: "Request device…",
    onclick: async () => {
      error.textContent = "";
      try {
        await manager.requestAndAttachDevice([]);
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  const knownList = el("ul");
  const attachedList = el("ul");

  function render(state: WebHidPassthroughState): void {
    requestButton.disabled = !state.supported;

    if (!state.supported) {
      knownList.replaceChildren(el("li", { text: "WebHID is not available in this browser/context." }));
      attachedList.replaceChildren(el("li", { text: "No devices attached." }));
      return;
    }

    const attachedSet = new Set(state.attachedDevices.map((d) => d.device));
    const known = state.knownDevices.filter((d) => !attachedSet.has(d));

    knownList.replaceChildren(
      ...(known.length
        ? known.map((device) =>
            el(
              "li",
              {},
              el("span", { text: describeDevice(device) }),
              el("button", {
                text: "Attach",
                onclick: async () => {
                  error.textContent = "";
                  try {
                    await manager.attachKnownDevice(device);
                  } catch (err) {
                    error.textContent = err instanceof Error ? err.message : String(err);
                  }
                },
              }),
            ),
          )
        : [el("li", { text: "No known devices. Use “Request device…” to grant access." })]),
    );

    attachedList.replaceChildren(
      ...(state.attachedDevices.length
        ? state.attachedDevices.map((attachment) =>
            el(
              "li",
              {},
              el("span", { class: "mono", text: `port=${attachment.guestPort}` }),
              el("span", { text: ` ${describeDevice(attachment.device)}` }),
              el("button", {
                text: "Detach",
                onclick: async () => {
                  error.textContent = "";
                  try {
                    await manager.detachDevice(attachment.device);
                  } catch (err) {
                    error.textContent = err instanceof Error ? err.message : String(err);
                  }
                },
              }),
            ),
          )
        : [el("li", { text: "No devices attached." })]),
    );
  }

  const unsubscribe = manager.subscribe(render);

  host.replaceChildren(
    el("h3", { text: "WebHID passthrough (USB HID → guest UHCI)" }),
    portHint,
    permissionHint,
    el("div", { class: "row" }, requestButton),
    el("h4", { text: "Known devices" }),
    knownList,
    el("h4", { text: "Attached devices" }),
    attachedList,
    error,
  );

  void manager.refreshKnownDevices();

  return () => {
    unsubscribe();
    manager.destroy();
  };
}


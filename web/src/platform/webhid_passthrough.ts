export type WebHidPassthroughState = {
  supported: boolean;
  knownDevices: HIDDevice[];
  attachedDevices: HIDDevice[];
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

export class WebHidPassthroughManager {
  readonly #hid: HidLike | null;
  #knownDevices: HIDDevice[] = [];
  #attachedDevices: HIDDevice[] = [];
  readonly #listeners = new Set<WebHidPassthroughListener>();

  readonly #onConnect: ((event: Event) => void) | null;
  readonly #onDisconnect: ((event: Event) => void) | null;

  constructor(options: { hid?: HidLike | null } = {}) {
    this.#hid = options.hid ?? getNavigatorHid();

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
    await device.open();
    if (!this.#attachedDevices.includes(device)) {
      this.#attachedDevices = [...this.#attachedDevices, device];
      this.#emit();
    }
  }

  async detachDevice(device: HIDDevice): Promise<void> {
    if (!this.#attachedDevices.includes(device)) return;

    this.#attachedDevices = this.#attachedDevices.filter((d) => d !== device);
    this.#emit();

    try {
      await device.close();
    } catch (err) {
      console.warn("WebHID device.close() failed", err);
    }
  }

  #emit(): void {
    const state = this.getState();
    for (const listener of this.#listeners) listener(state);
  }

  async #handleDisconnect(event: Event): Promise<void> {
    const dev = (event as unknown as HIDConnectionEvent).device;
    if (dev) {
      await this.detachDevice(dev);
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
  const hint = el("div", {
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

    const attachedSet = new Set(state.attachedDevices);
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
        ? state.attachedDevices.map((device) =>
            el(
              "li",
              {},
              el("span", { text: describeDevice(device) }),
              el("button", {
                text: "Detach",
                onclick: async () => {
                  error.textContent = "";
                  try {
                    await manager.detachDevice(device);
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
    el("h3", { text: "WebHID passthrough" }),
    hint,
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


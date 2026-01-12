import { afterEach, describe, expect, it, vi } from "vitest";

import { UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT, UHCI_SYNTHETIC_HID_HUB_PORT_COUNT } from "../usb/uhci_external_hub";
import { isGuestUsbPath, type HidPassthroughMessage } from "./hid_passthrough_protocol";
import { getNoFreeGuestUsbPortsMessage, mountWebHidPassthroughPanel, WebHidPassthroughManager } from "./webhid_passthrough";

const originalNavigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
const originalDocumentDescriptor = Object.getOwnPropertyDescriptor(globalThis, "document");

function stubNavigator(value: unknown): void {
  Object.defineProperty(globalThis, "navigator", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

function stubDocument(value: unknown): void {
  Object.defineProperty(globalThis, "document", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

afterEach(() => {
  if (originalNavigatorDescriptor) {
    Object.defineProperty(globalThis, "navigator", originalNavigatorDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "navigator");
  }

  if (originalDocumentDescriptor) {
    Object.defineProperty(globalThis, "document", originalDocumentDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "document");
  }
});

type FakeListener = (event: unknown) => void;

class FakeElement {
  readonly tagName: string;
  className = "";
  disabled = false;
  private _text = "";
  readonly children: FakeElement[] = [];
  readonly attributes: Record<string, string> = {};
  onclick?: () => unknown;

  constructor(tagName: string) {
    this.tagName = tagName.toUpperCase();
  }

  setAttribute(name: string, value: string): void {
    this.attributes[name] = value;
  }

  append(child: FakeElement): void {
    this.children.push(child);
  }

  replaceChildren(...children: FakeElement[]): void {
    this.children.length = 0;
    this.children.push(...children.filter(Boolean));
  }

  set textContent(value: string | null) {
    this._text = value ?? "";
    this.children.length = 0;
  }

  get textContent(): string {
    return [this._text, ...this.children.map((c) => c.textContent)].join("");
  }
}

class FakeDocument {
  createElement(tag: string): FakeElement {
    return new FakeElement(tag);
  }
}

function findAll(root: FakeElement, predicate: (el: FakeElement) => boolean): FakeElement[] {
  const out: FakeElement[] = [];
  const walk = (node: FakeElement): void => {
    if (predicate(node)) out.push(node);
    for (const child of node.children) walk(child);
  };
  walk(root);
  return out;
}

class FakeHid {
  readonly getDevices: () => Promise<HIDDevice[]>;
  readonly requestDevice: () => Promise<HIDDevice[]>;
  readonly #listeners = new Map<string, Set<FakeListener>>();

  constructor(options: { getDevices: () => Promise<HIDDevice[]>; requestDevice: () => Promise<HIDDevice[]> }) {
    this.getDevices = options.getDevices;
    this.requestDevice = options.requestDevice;
  }

  addEventListener(type: string, cb: FakeListener): void {
    let set = this.#listeners.get(type);
    if (!set) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(cb);
  }

  removeEventListener(type: string, cb: FakeListener): void {
    this.#listeners.get(type)?.delete(cb);
  }

  dispatch(type: string, event: unknown): void {
    for (const cb of this.#listeners.get(type) ?? []) cb(event);
  }
}

describe("WebHidPassthroughManager UI (mocked WebHID)", () => {
  it("shows previously granted devices returned from getDevices()", async () => {
    const device = {
      productName: "Example Gamepad",
      vendorId: 0x1234,
      productId: 0xabcd,
      collections: [] as unknown as HIDCollectionInfo[],
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const spans = findAll(host, (el) => el.tagName === "SPAN" && el.textContent.includes("Example Gamepad"));
    expect(spans.length).toBe(1);
  });

  it("shows Forget only for devices that expose device.forget()", async () => {
    const forgettable = {
      productName: "Forgettable",
      vendorId: 0x0001,
      productId: 0x0002,
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
      forget: vi.fn(async () => {}),
    } as unknown as HIDDevice;
    const normal = {
      productName: "Normal",
      vendorId: 0x0003,
      productId: 0x0004,
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [forgettable, normal]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const forgetButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Forget");
    expect(forgetButtons).toHaveLength(1);
  });

  it("always includes a site settings link for permission revocation guidance", async () => {
    const device = {
      productName: "Normal",
      vendorId: 0x0003,
      productId: 0x0004,
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const siteLinks = findAll(host, (el) => el.tagName === "A" && el.textContent === "site settings");
    expect(siteLinks).toHaveLength(1);
    expect(siteLinks[0].attributes.href).toContain("chrome://settings/content/siteDetails");
  });

  it("attaches known devices without calling requestDevice()", async () => {
    let opened = false;
    const device = {
      productName: "Granted Device",
      vendorId: 0x0001,
      productId: 0x0002,
      collections: [] as unknown as HIDCollectionInfo[],
      get opened() {
        return opened;
      },
      open: vi.fn(async () => {
        opened = true;
      }),
      close: vi.fn(async () => {
        opened = false;
      }),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const attachButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Attach");
    expect(attachButtons.length).toBe(1);

    await (attachButtons[0].onclick as () => Promise<void>)();

    expect((hid.requestDevice as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
    expect((device.open as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(1);

    const attached = manager.getState().attachedDevices;
    expect(attached).toHaveLength(1);
    expect(attached[0]?.device).toBe(device);
    expect(attached[0]?.guestPath).toEqual([0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT]);
  });

  it("detaches an attached device before calling device.forget()", async () => {
    const callOrder: string[] = [];
    const device = {
      productName: "Detach+Forget",
      vendorId: 0x0001,
      productId: 0x0002,
      collections: [] as unknown as HIDCollectionInfo[],
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
      forget: vi.fn(async () => {
        callOrder.push("forget");
      }),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager({
      target: {
        postMessage: (msg: HidPassthroughMessage) => {
          if (msg.type === "hid:detach") {
            callOrder.push("detach");
            throw new Error("vm detach failed");
          }
        },
      },
    });
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();
    const attachButton = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Attach")[0];
    expect(attachButton).toBeTruthy();
    await (attachButton.onclick as () => Promise<void>)();

    const forgetButton = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Forget")[0];
    expect(forgetButton).toBeTruthy();

    await (forgetButton.onclick as () => Promise<void>)();

    expect(callOrder).toEqual(["detach", "forget"]);
  });

  it("surfaces forget errors without breaking the panel UI", async () => {
    const device = {
      productName: "BrokenForget",
      vendorId: 0x0001,
      productId: 0x0002,
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
      forget: vi.fn(async () => {
        throw new Error("boom");
      }),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const forgetButton = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Forget")[0];
    expect(forgetButton).toBeTruthy();
    await (forgetButton.onclick as () => Promise<void>)();

    const errors = findAll(host, (el) => el.tagName === "PRE");
    expect(errors.some((el) => el.textContent.includes("Forget failed: boom"))).toBe(true);

    // Still renders the device row and actions.
    const attachButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Attach");
    expect(attachButtons).toHaveLength(1);
  });
});

class TestTarget {
  readonly messages: HidPassthroughMessage[] = [];

  postMessage(message: HidPassthroughMessage): void {
    this.messages.push(message);
  }
}

function makeDevice(vendorId: number, productId: number, productName: string): HIDDevice {
  return {
    vendorId,
    productId,
    productName,
    collections: [] as unknown as HIDCollectionInfo[],
    open: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  } as unknown as HIDDevice;
}

describe("Guest USB path validator", () => {
  it("accepts non-empty paths with root ports 0/1 and hub ports 1..=255", () => {
    expect(isGuestUsbPath([0])).toBe(true);
    expect(isGuestUsbPath([1])).toBe(true);
    expect(isGuestUsbPath([0, 1])).toBe(true);
    expect(isGuestUsbPath([0, 255])).toBe(true);
    expect(isGuestUsbPath([])).toBe(false);
    expect(isGuestUsbPath([2])).toBe(false);
    expect(isGuestUsbPath([0, 0])).toBe(false);
    expect(isGuestUsbPath([0, 256])).toBe(false);
    expect(isGuestUsbPath([0, 1.5])).toBe(false);
  });
});

describe("WebHID guest path allocation (external hub on root port 0)", () => {
  it("assigns hub-backed paths when attaching three devices", async () => {
    const target = new TestTarget();
    const externalHubPortCount = UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + 3;
    const manager = new WebHidPassthroughManager({ hid: null, target, externalHubPortCount });

    const devA = makeDevice(1, 1, "A");
    const devB = makeDevice(2, 2, "B");
    const devC = makeDevice(3, 3, "C");

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);
    await manager.attachKnownDevice(devC);

    expect(target.messages).toHaveLength(4);
    expect(target.messages[0]).toMatchObject({ type: "hid:attachHub", guestPath: [0], portCount: externalHubPortCount });
    expect(target.messages[1]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT] });
    expect(target.messages[2]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 1] });
    expect(target.messages[3]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 2] });

    expect(manager.getState().attachedDevices.map((d) => d.guestPath)).toEqual([
      [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT],
      [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 1],
      [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 2],
    ]);

    // Sanity: allocations must never use reserved hub ports.
    for (const path of manager.getState().attachedDevices.map((d) => d.guestPath)) {
      expect(path[1]).toBeGreaterThanOrEqual(UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT);
    }
  });

  it("frees hub ports on detach and reuses the lowest free hub port", async () => {
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target, externalHubPortCount: UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + 3 });

    const devA = makeDevice(1, 1, "A");
    const devB = makeDevice(2, 2, "B");
    const devC = makeDevice(3, 3, "C");
    const devD = makeDevice(4, 4, "D");

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);
    await manager.attachKnownDevice(devC);
    await manager.detachDevice(devB);
    await manager.attachKnownDevice(devD);

    expect(target.messages).toHaveLength(6);
    expect(target.messages[4]).toMatchObject({ type: "hid:detach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 1] });
    expect(target.messages[5]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 1] });
  });

  it("can resync already-attached devices after an I/O worker restart", async () => {
    const target = new TestTarget();
    const externalHubPortCount = UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + 3;
    const manager = new WebHidPassthroughManager({ hid: null, target, externalHubPortCount });

    const devA = makeDevice(1, 1, "A");
    await manager.attachKnownDevice(devA);

    const firstAttach = target.messages.find((m) => m.type === "hid:attach") as any;
    expect(firstAttach).toBeTruthy();
    expect(typeof firstAttach.numericDeviceId).toBe("number");
    const numericDeviceId = firstAttach.numericDeviceId;

    target.messages.length = 0;
    await manager.resyncAttachedDevices();

    expect(target.messages).toHaveLength(2);
    expect(target.messages[0]).toMatchObject({ type: "hid:attachHub", guestPath: [0], portCount: externalHubPortCount });
    expect(target.messages[1]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT], numericDeviceId });
  });

  it("errors once the external hub is full (root port 1 reserved)", async () => {
    const target = new TestTarget();
    const externalHubPortCount = UHCI_SYNTHETIC_HID_HUB_PORT_COUNT + 2;
    const manager = new WebHidPassthroughManager({ hid: null, target, externalHubPortCount });

    const devA = makeDevice(1, 1, "A");
    const devB = makeDevice(2, 2, "B");
    const devC = makeDevice(3, 3, "C");

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);
    await expect(manager.attachKnownDevice(devC)).rejects.toThrow(
      getNoFreeGuestUsbPortsMessage({ externalHubPortCount, reservedExternalHubPorts: UHCI_SYNTHETIC_HID_HUB_PORT_COUNT }),
    );

    expect(target.messages).toHaveLength(3);
    expect(target.messages[0]).toMatchObject({ type: "hid:attachHub", guestPath: [0], portCount: externalHubPortCount });
    expect(target.messages[1]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT] });
    expect(target.messages[2]).toMatchObject({ type: "hid:attach", guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT + 1] });

    expect(manager.getState().attachedDevices).toHaveLength(2);
  });

  it("forgets known devices without calling requestDevice()", async () => {
    const device = {
      productName: "Forgettable Device",
      vendorId: 0x0003,
      productId: 0x0004,
      opened: false,
      open: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget: vi.fn(async () => {}),
    } as unknown as HIDDevice;

    const hid = new FakeHid({
      getDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const forgetButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Forget");
    expect(forgetButtons.length).toBe(1);

    await (forgetButtons[0].onclick as () => Promise<void>)();

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect((device as any).forget).toHaveBeenCalledTimes(1);
    expect((hid.requestDevice as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
  });

  it("detaches before forgetting attached devices", async () => {
    let opened = false;
    const callOrder: string[] = [];
    const device = {
      productName: "Forgettable Attached Device",
      vendorId: 0x0010,
      productId: 0x0020,
      collections: [] as unknown as HIDCollectionInfo[],
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      get opened() {
        return opened;
      },
      open: vi.fn(async () => {
        opened = true;
        callOrder.push("open");
      }),
      close: vi.fn(async () => {
        opened = false;
        callOrder.push("close");
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget: vi.fn(async () => {
        callOrder.push("forget");
      }),
    } as unknown as HIDDevice;

    // Note: `mountWebHidPassthroughPanel` triggers an initial refreshKnownDevices()
    // in the background, so keep this mock stable across calls.
    const getDevices = vi.fn(async () => [device]);

    const hid = new FakeHid({
      getDevices,
      requestDevice: vi.fn(async () => []),
    });
    stubNavigator({ hid } as any);
    stubDocument(new FakeDocument());

    const host = (document as any).createElement("div") as FakeElement;
    const manager = new WebHidPassthroughManager();
    mountWebHidPassthroughPanel(host as any, manager);

    await manager.refreshKnownDevices();

    const attachButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Attach");
    expect(attachButtons.length).toBe(1);
    await (attachButtons[0].onclick as () => Promise<void>)();

    const forgetButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Forget");
    expect(forgetButtons.length).toBe(1);
    await (forgetButtons[0].onclick as () => Promise<void>)();

    expect(callOrder).toEqual(["open", "close", "forget"]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect((device as any).forget).toHaveBeenCalledTimes(1);
    expect((device.close as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(1);
    expect((hid.requestDevice as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
  });
});

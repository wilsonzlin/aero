import { afterEach, describe, expect, it, vi } from "vitest";

import type { HidPassthroughMessage } from "./hid_passthrough_protocol";
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

    const spans = findAll(host, (el) => el.tagName === "SPAN" && el.textContent.includes("Example Gamepad"));
    expect(spans.length).toBe(1);
  });

  it("attaches known devices without calling requestDevice()", async () => {
    let opened = false;
    const device = {
      productName: "Granted Device",
      vendorId: 0x0001,
      productId: 0x0002,
      get opened() {
        return opened;
      },
      open: vi.fn(async () => {
        opened = true;
      }),
      close: vi.fn(async () => {
        opened = false;
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

    const attachButtons = findAll(host, (el) => el.tagName === "BUTTON" && el.textContent === "Attach");
    expect(attachButtons.length).toBe(1);

    await (attachButtons[0].onclick as () => Promise<void>)();

    expect((hid.requestDevice as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
    expect((device.open as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(1);

    const attached = manager.getState().attachedDevices;
    expect(attached).toHaveLength(1);
    expect(attached[0]?.device).toBe(device);
    expect(attached[0]?.guestPort).toBe(0);
  });
});

class TestTarget {
  readonly messages: HidPassthroughMessage[] = [];

  postMessage(message: HidPassthroughMessage): void {
    this.messages.push(message);
  }
}

describe("WebHID guest port allocation (UHCI 2-port root)", () => {
  it("assigns ports 0 and 1 when attaching two devices", async () => {
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    const devA = { vendorId: 1, productId: 1, productName: "A", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;
    const devB = { vendorId: 2, productId: 2, productName: "B", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);

    expect(target.messages).toHaveLength(2);
    expect(target.messages[0]).toMatchObject({ type: "hid:attach", guestPort: 0 });
    expect(target.messages[1]).toMatchObject({ type: "hid:attach", guestPort: 1 });

    expect(manager.getState().attachedDevices.map((d) => d.guestPort)).toEqual([0, 1]);
  });

  it("rejects a third attach and does not post a worker message", async () => {
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    const devA = { vendorId: 1, productId: 1, productName: "A", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;
    const devB = { vendorId: 2, productId: 2, productName: "B", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;
    const devC = { vendorId: 3, productId: 3, productName: "C", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);
    await expect(manager.attachKnownDevice(devC)).rejects.toThrow(getNoFreeGuestUsbPortsMessage());

    expect(target.messages).toHaveLength(2);
    expect(manager.getState().attachedDevices).toHaveLength(2);
  });

  it("frees ports on detach and reuses the lowest free port", async () => {
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    const devA = { vendorId: 1, productId: 1, productName: "A", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;
    const devB = { vendorId: 2, productId: 2, productName: "B", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;
    const devC = { vendorId: 3, productId: 3, productName: "C", open: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as HIDDevice;

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);
    await manager.detachDevice(devA);

    await manager.attachKnownDevice(devC);

    expect(target.messages).toHaveLength(4);
    expect(target.messages[2]).toMatchObject({ type: "hid:detach", guestPort: 0 });
    expect(target.messages[3]).toMatchObject({ type: "hid:attach", guestPort: 0 });
  });
});


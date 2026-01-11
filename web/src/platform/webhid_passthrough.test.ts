import { afterEach, describe, expect, it, vi } from "vitest";
import { mountWebHidPassthroughPanel, WebHidPassthroughManager } from "./webhid_passthrough";

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
    expect(manager.getState().attachedDevices).toEqual([device]);
  });
});


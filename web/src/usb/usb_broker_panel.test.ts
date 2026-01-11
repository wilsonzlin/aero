import { afterEach, describe, expect, it, vi } from "vitest";

import { renderWebUsbBrokerPanel } from "./usb_broker_panel";

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

describe("usb broker panel UI", () => {
  it("calls attachKnownDevice when clicking Attach for a known device", async () => {
    const device = { vendorId: 0x1234, productId: 0x5678, productName: "Demo" } as unknown as USBDevice;

    const broker = {
      attachKnownDevice: vi.fn(async () => ({ vendorId: device.vendorId, productId: device.productId, productName: device.productName })),
      detachSelectedDevice: vi.fn(async () => {}),
      getKnownDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => ({ vendorId: device.vendorId, productId: device.productId, productName: device.productName })),
      attachWorkerPort: vi.fn(() => {}),
      subscribeToDeviceChanges: vi.fn(() => () => {}),
    } as any;

    stubNavigator({ usb: {} } as any);
    stubDocument(new FakeDocument());

    const panel = renderWebUsbBrokerPanel(broker);
    await new Promise((r) => setTimeout(r, 0));

    const attachButtons = findAll(panel as any, (el) => el.tagName === "BUTTON" && el.textContent === "Attach");
    expect(attachButtons).toHaveLength(1);

    await (attachButtons[0].onclick as () => Promise<void>)();

    expect(broker.attachKnownDevice).toHaveBeenCalledTimes(1);
    expect(broker.attachKnownDevice).toHaveBeenCalledWith(device);
  });
});


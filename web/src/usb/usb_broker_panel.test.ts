import { afterEach, describe, expect, it, vi } from "vitest";

import { renderWebUsbBrokerPanel } from "./usb_broker_panel";
import type { UsbBroker } from "./usb_broker";

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
    Reflect.deleteProperty(globalThis, "navigator");
  }

  if (originalDocumentDescriptor) {
    Object.defineProperty(globalThis, "document", originalDocumentDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "document");
  }
});

class FakeElement {
  readonly tagName: string;
  className = "";
  disabled = false;
  hidden = false;
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
  it("includes a site settings link in the hint", () => {
    const broker = {
      attachKnownDevice: vi.fn(async () => ({ vendorId: 0, productId: 0 })),
      detachSelectedDevice: vi.fn(async () => {}),
      getKnownDevices: vi.fn(async () => []),
      requestDevice: vi.fn(async () => ({ vendorId: 0, productId: 0 })),
      attachWorkerPort: vi.fn(() => {}),
      subscribeToDeviceChanges: vi.fn(() => () => {}),
    };

    stubNavigator({ usb: {} });
    stubDocument(new FakeDocument());

    const panel = renderWebUsbBrokerPanel(broker as unknown as UsbBroker);
    const links = findAll(panel as unknown as FakeElement, (el) => el.tagName === "A" && el.textContent === "site settings");
    expect(links).toHaveLength(1);
    expect(links[0].attributes.href).toContain("chrome://settings/content/siteDetails");
  });

  it("attaches a UI message port without allocating USB proxy rings", () => {
    const broker = {
      attachKnownDevice: vi.fn(async () => ({ vendorId: 0, productId: 0 })),
      detachSelectedDevice: vi.fn(async () => {}),
      getKnownDevices: vi.fn(async () => []),
      requestDevice: vi.fn(async () => ({ vendorId: 0, productId: 0 })),
      attachWorkerPort: vi.fn(() => {}),
      subscribeToDeviceChanges: vi.fn(() => () => {}),
    };

    stubNavigator({ usb: {} });
    stubDocument(new FakeDocument());

    renderWebUsbBrokerPanel(broker as unknown as UsbBroker);

    if (typeof MessageChannel === "undefined") return;
    expect(broker.attachWorkerPort).toHaveBeenCalledWith(expect.anything(), { attachRings: false });
  });

  it("calls attachKnownDevice when clicking Attach for a known device", async () => {
    const device = { vendorId: 0x1234, productId: 0x5678, productName: "Demo" } as unknown as USBDevice;

    const broker = {
      attachKnownDevice: vi.fn(async () => ({ vendorId: device.vendorId, productId: device.productId, productName: device.productName })),
      detachSelectedDevice: vi.fn(async () => {}),
      getKnownDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => ({ vendorId: device.vendorId, productId: device.productId, productName: device.productName })),
      attachWorkerPort: vi.fn(() => {}),
      subscribeToDeviceChanges: vi.fn(() => () => {}),
    };

    stubNavigator({ usb: {} });
    stubDocument(new FakeDocument());

    const panel = renderWebUsbBrokerPanel(broker as unknown as UsbBroker);
    await new Promise((r) => setTimeout(r, 0));

    const attachButtons = findAll(panel as unknown as FakeElement, (el) => el.tagName === "BUTTON" && el.textContent === "Attach");
    expect(attachButtons).toHaveLength(1);

    await (attachButtons[0].onclick as () => Promise<void>)();

    expect(broker.attachKnownDevice).toHaveBeenCalledTimes(1);
    expect(broker.attachKnownDevice).toHaveBeenCalledWith(device);
  });

  it("shows Forget selected device when supported and calls forgetSelectedDevice()", async () => {
    const device = { vendorId: 0x1234, productId: 0x5678, productName: "Demo" } as unknown as USBDevice;
    let attachedPort: MessagePort | null = null;

    const broker = {
      attachKnownDevice: vi.fn(async () => ({ vendorId: device.vendorId, productId: device.productId, productName: device.productName })),
      detachSelectedDevice: vi.fn(async () => {}),
      getKnownDevices: vi.fn(async () => [device]),
      requestDevice: vi.fn(async () => ({ vendorId: device.vendorId, productId: device.productId, productName: device.productName })),
      attachWorkerPort: vi.fn((port: MessagePort) => {
        attachedPort = port;
      }),
      subscribeToDeviceChanges: vi.fn(() => () => {}),
      canForgetSelectedDevice: vi.fn(() => true),
      forgetSelectedDevice: vi.fn(async () => {
        // Simulate the broker broadcasting a deselection to the attached MessagePort.
        attachedPort?.postMessage({ type: "usb.selected", ok: false });
      }),
    };

    stubNavigator({ usb: {} });
    stubDocument(new FakeDocument());

    const panel = renderWebUsbBrokerPanel(broker as unknown as UsbBroker);
    await new Promise((r) => setTimeout(r, 0));
    expect(attachedPort).not.toBeNull();
    const port = attachedPort as unknown as MessagePort;

    // Simulate broker selection notification.
    port.postMessage({
      type: "usb.selected",
      ok: true,
      info: { vendorId: device.vendorId, productId: device.productId, productName: device.productName },
    });
    await new Promise((r) => setTimeout(r, 0));

    const forgetButtons = findAll(
      panel as unknown as FakeElement,
      (el) => el.tagName === "BUTTON" && el.textContent === "Forget selected device",
    );
    expect(forgetButtons).toHaveLength(1);
    expect(forgetButtons[0].hidden).toBe(false);

    await (forgetButtons[0].onclick as () => Promise<void>)();

    expect(broker.forgetSelectedDevice).toHaveBeenCalledTimes(1);
    await new Promise((r) => setTimeout(r, 0));
    expect(forgetButtons[0].hidden).toBe(true);
  });
});

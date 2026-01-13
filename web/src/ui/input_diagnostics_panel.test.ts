import { afterEach, describe, expect, it } from "vitest";

import { mountInputDiagnosticsPanel, type InputDiagnosticsSnapshot } from "./input_diagnostics_panel";

const originalDocumentDescriptor = Object.getOwnPropertyDescriptor(globalThis, "document");

function stubDocument(value: unknown): void {
  Object.defineProperty(globalThis, "document", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

afterEach(() => {
  if (originalDocumentDescriptor) {
    Object.defineProperty(globalThis, "document", originalDocumentDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "document");
  }
});

class FakeElement {
  readonly tagName: string;
  className = "";
  private _text = "";
  readonly children: FakeElement[] = [];
  readonly attributes: Record<string, string> = {};

  constructor(tagName: string) {
    this.tagName = tagName.toUpperCase();
  }

  setAttribute(name: string, value: string): void {
    this.attributes[name] = value;
  }

  append(...children: FakeElement[]): void {
    this.children.push(...children.filter(Boolean));
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

describe("mountInputDiagnosticsPanel", () => {
  it("renders and updates when fed snapshots", () => {
    stubDocument(new FakeDocument());

    const host = document.createElement("div") as unknown as HTMLElement;
    const panel = mountInputDiagnosticsPanel(host);

    const snap1: InputDiagnosticsSnapshot = {
      keyboardBackend: "ps2",
      mouseBackend: "usb",
      virtioKeyboardDriverOk: false,
      virtioMouseDriverOk: true,
      syntheticUsbKeyboardConfigured: true,
      syntheticUsbMouseConfigured: false,
      mouseButtonsMask: 0x1f,
      pressedKeyboardHidUsageCount: 2,
    };
    panel.setSnapshot(snap1);
    expect((host as any).textContent).toContain("keyboard_backend=ps2");
    expect((host as any).textContent).toContain("mouse_backend=usb");
    expect((host as any).textContent).toContain("virtio_mouse.driver_ok=yes");
    expect((host as any).textContent).toContain("synthetic_usb_keyboard.configured=yes");
    expect((host as any).textContent).toContain("mouse_buttons_mask=0x0000001f");
    expect((host as any).textContent).toContain("pressed_hid_usage_count=2");

    const snap2: InputDiagnosticsSnapshot = {
      ...snap1,
      keyboardBackend: "virtio",
      mouseBackend: "virtio",
      virtioKeyboardDriverOk: true,
      virtioMouseDriverOk: true,
      syntheticUsbKeyboardConfigured: false,
      syntheticUsbMouseConfigured: true,
      mouseButtonsMask: 0,
      pressedKeyboardHidUsageCount: 0,
    };
    panel.setSnapshot(snap2);
    expect((host as any).textContent).toContain("keyboard_backend=virtio");
    expect((host as any).textContent).toContain("mouse_backend=virtio");
    expect((host as any).textContent).toContain("virtio_keyboard.driver_ok=yes");
    expect((host as any).textContent).toContain("synthetic_usb_mouse.configured=yes");
    expect((host as any).textContent).toContain("mouse_buttons_mask=0x00000000");
    expect((host as any).textContent).toContain("pressed_hid_usage_count=0");
  });
});

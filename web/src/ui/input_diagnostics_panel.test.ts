import { afterEach, describe, expect, it } from "vitest";

import { mountInputDiagnosticsPanel, readInputDiagnosticsSnapshotFromStatus } from "./input_diagnostics_panel";
import { encodeInputBackendStatus } from "../input/input_backend_status";
import { STATUS_BYTES, StatusIndex } from "../runtime/shared_layout";

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

    const status = new Int32Array(new SharedArrayBuffer(STATUS_BYTES));
    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus("ps2"));
    Atomics.store(status, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus("usb"));
    Atomics.store(status, StatusIndex.IoInputVirtioKeyboardDriverOk, 0);
    Atomics.store(status, StatusIndex.IoInputVirtioMouseDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbKeyboardOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbMouseOk, 0);
    Atomics.store(status, StatusIndex.IoInputMouseButtonsHeldMask, 0x1f);
    Atomics.store(status, StatusIndex.IoInputKeyboardHeldCount, 2);
    Atomics.store(status, StatusIndex.IoInputBatchReceivedCounter, 10);
    Atomics.store(status, StatusIndex.IoInputBatchCounter, 9);
    Atomics.store(status, StatusIndex.IoInputBatchDropCounter, 1);
    Atomics.store(status, StatusIndex.IoInputEventCounter, 123);
    Atomics.store(status, StatusIndex.IoKeyboardBackendSwitchCounter, 4);
    Atomics.store(status, StatusIndex.IoMouseBackendSwitchCounter, 5);

    panel.setSnapshot(readInputDiagnosticsSnapshotFromStatus(status));
    expect((host as any).textContent).toContain("keyboard_backend=ps2");
    expect((host as any).textContent).toContain("mouse_backend=usb");
    expect((host as any).textContent).toContain("virtio_mouse.driver_ok=yes");
    expect((host as any).textContent).toContain("synthetic_usb_keyboard.configured=yes");
    expect((host as any).textContent).toContain("mouse_buttons_mask=0x0000001f");
    expect((host as any).textContent).toContain("mouse_buttons_held=left,right,middle,back,forward");
    expect((host as any).textContent).toContain("pressed_hid_usage_count=2");
    expect((host as any).textContent).toContain("io.batches_received=10");
    expect((host as any).textContent).toContain("io.batches_processed=9");
    expect((host as any).textContent).toContain("io.batches_dropped=1");
    expect((host as any).textContent).toContain("io.events_processed=123");
    expect((host as any).textContent).toContain("io.keyboard_backend_switches=4");
    expect((host as any).textContent).toContain("io.mouse_backend_switches=5");

    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus("virtio"));
    Atomics.store(status, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus("virtio"));
    Atomics.store(status, StatusIndex.IoInputVirtioKeyboardDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputVirtioMouseDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbKeyboardOk, 0);
    Atomics.store(status, StatusIndex.IoInputUsbMouseOk, 1);
    Atomics.store(status, StatusIndex.IoInputMouseButtonsHeldMask, 0);
    Atomics.store(status, StatusIndex.IoInputKeyboardHeldCount, 0);
    Atomics.store(status, StatusIndex.IoInputBatchReceivedCounter, 42);
    Atomics.store(status, StatusIndex.IoInputBatchCounter, 42);
    Atomics.store(status, StatusIndex.IoInputBatchDropCounter, 0);
    Atomics.store(status, StatusIndex.IoInputEventCounter, 999);
    Atomics.store(status, StatusIndex.IoKeyboardBackendSwitchCounter, 6);
    Atomics.store(status, StatusIndex.IoMouseBackendSwitchCounter, 7);

    panel.setSnapshot(readInputDiagnosticsSnapshotFromStatus(status));
    expect((host as any).textContent).toContain("keyboard_backend=virtio");
    expect((host as any).textContent).toContain("mouse_backend=virtio");
    expect((host as any).textContent).toContain("virtio_keyboard.driver_ok=yes");
    expect((host as any).textContent).toContain("synthetic_usb_mouse.configured=yes");
    expect((host as any).textContent).toContain("mouse_buttons_mask=0x00000000");
    expect((host as any).textContent).toContain("mouse_buttons_held=(none)");
    expect((host as any).textContent).toContain("pressed_hid_usage_count=0");
    expect((host as any).textContent).toContain("io.batches_received=42");
    expect((host as any).textContent).toContain("io.batches_processed=42");
    expect((host as any).textContent).toContain("io.batches_dropped=0");
    expect((host as any).textContent).toContain("io.events_processed=999");
    expect((host as any).textContent).toContain("io.keyboard_backend_switches=6");
    expect((host as any).textContent).toContain("io.mouse_backend_switches=7");
  });
});

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
    Reflect.deleteProperty(globalThis, "document");
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

    const host = document.createElement("div");
    const panel = mountInputDiagnosticsPanel(host);
    const hostText = (): string => host.textContent ?? "";

    const status = new Int32Array(new SharedArrayBuffer(STATUS_BYTES));
    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus("ps2"));
    Atomics.store(status, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus("usb"));
    Atomics.store(status, StatusIndex.IoInputVirtioKeyboardDriverOk, 0);
    Atomics.store(status, StatusIndex.IoInputVirtioMouseDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbKeyboardOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbMouseOk, 0);
    Atomics.store(status, StatusIndex.IoInputKeyboardLedsUsb, 0x03);
    Atomics.store(status, StatusIndex.IoInputKeyboardLedsVirtio, 0);
    Atomics.store(status, StatusIndex.IoInputKeyboardLedsPs2, 0x04);
    Atomics.store(status, StatusIndex.IoInputMouseButtonsHeldMask, 0x1f);
    Atomics.store(status, StatusIndex.IoInputKeyboardHeldCount, 2);
    Atomics.store(status, StatusIndex.IoInputBatchReceivedCounter, 10);
    Atomics.store(status, StatusIndex.IoInputBatchCounter, 9);
    Atomics.store(status, StatusIndex.IoInputBatchDropCounter, 1);
    Atomics.store(status, StatusIndex.IoInputEventCounter, 123);
    Atomics.store(status, StatusIndex.IoKeyboardBackendSwitchCounter, 4);
    Atomics.store(status, StatusIndex.IoMouseBackendSwitchCounter, 5);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyUs, 1500);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyEwmaUs, 2000);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyMaxUs, 2500);
    Atomics.store(status, StatusIndex.IoInputEventLatencyAvgUs, 500);
    Atomics.store(status, StatusIndex.IoInputEventLatencyEwmaUs, 600);
    Atomics.store(status, StatusIndex.IoInputEventLatencyMaxUs, 700);

    panel.setSnapshot(readInputDiagnosticsSnapshotFromStatus(status));
    expect(hostText()).toContain("keyboard_backend=ps2");
    expect(hostText()).toContain("mouse_backend=usb");
    expect(hostText()).toContain("virtio_mouse.driver_ok=yes");
    expect(hostText()).toContain("synthetic_usb_keyboard.configured=yes");
    expect(hostText()).toContain("mouse_buttons_mask=0x0000001f");
    expect(hostText()).toContain("mouse_buttons_held=left,right,middle,back,forward");
    expect(hostText()).toContain("keyboard_held_count=2");
    expect(hostText()).toContain("keyboard_leds_usb=0x00000003 num,caps");
    expect(hostText()).toContain("keyboard_leds_virtio=0x00000000 (none)");
    expect(hostText()).toContain("keyboard_leds_ps2=0x00000004 scroll");
    expect(hostText()).toContain("io.batches_received=10");
    expect(hostText()).toContain("io.batches_processed=9");
    expect(hostText()).toContain("io.batches_dropped=1");
    expect(hostText()).toContain("io.events_processed=123");
    expect(hostText()).toContain("io.keyboard_backend_switches=4");
    expect(hostText()).toContain("io.mouse_backend_switches=5");
    expect(hostText()).toContain("io.batch_send_latency_us=1500 us (1.500 ms)");
    expect(hostText()).toContain("io.batch_send_latency_ewma_us=2000 us (2.000 ms)");
    expect(hostText()).toContain("io.batch_send_latency_max_us=2500 us (2.500 ms)");
    expect(hostText()).toContain("io.event_latency_avg_us=500 us (0.500 ms)");
    expect(hostText()).toContain("io.event_latency_ewma_us=600 us (0.600 ms)");
    expect(hostText()).toContain("io.event_latency_max_us=700 us (0.700 ms)");

    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus("virtio"));
    Atomics.store(status, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus("virtio"));
    Atomics.store(status, StatusIndex.IoInputVirtioKeyboardDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputVirtioMouseDriverOk, 1);
    Atomics.store(status, StatusIndex.IoInputUsbKeyboardOk, 0);
    Atomics.store(status, StatusIndex.IoInputUsbMouseOk, 1);
    Atomics.store(status, StatusIndex.IoInputKeyboardLedsUsb, 0);
    Atomics.store(status, StatusIndex.IoInputKeyboardLedsVirtio, 0x01);
    Atomics.store(status, StatusIndex.IoInputKeyboardLedsPs2, 0);
    Atomics.store(status, StatusIndex.IoInputMouseButtonsHeldMask, 0);
    Atomics.store(status, StatusIndex.IoInputKeyboardHeldCount, 0);
    Atomics.store(status, StatusIndex.IoInputBatchReceivedCounter, 42);
    Atomics.store(status, StatusIndex.IoInputBatchCounter, 42);
    Atomics.store(status, StatusIndex.IoInputBatchDropCounter, 0);
    Atomics.store(status, StatusIndex.IoInputEventCounter, 999);
    Atomics.store(status, StatusIndex.IoKeyboardBackendSwitchCounter, 6);
    Atomics.store(status, StatusIndex.IoMouseBackendSwitchCounter, 7);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyUs, 3000);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyEwmaUs, 3500);
    Atomics.store(status, StatusIndex.IoInputBatchSendLatencyMaxUs, 4000);
    Atomics.store(status, StatusIndex.IoInputEventLatencyAvgUs, 800);
    Atomics.store(status, StatusIndex.IoInputEventLatencyEwmaUs, 900);
    Atomics.store(status, StatusIndex.IoInputEventLatencyMaxUs, 1000);

    panel.setSnapshot(readInputDiagnosticsSnapshotFromStatus(status));
    expect(hostText()).toContain("keyboard_backend=virtio");
    expect(hostText()).toContain("mouse_backend=virtio");
    expect(hostText()).toContain("virtio_keyboard.driver_ok=yes");
    expect(hostText()).toContain("synthetic_usb_mouse.configured=yes");
    expect(hostText()).toContain("mouse_buttons_mask=0x00000000");
    expect(hostText()).toContain("mouse_buttons_held=(none)");
    expect(hostText()).toContain("keyboard_held_count=0");
    expect(hostText()).toContain("keyboard_leds_usb=0x00000000 (none)");
    expect(hostText()).toContain("keyboard_leds_virtio=0x00000001 num");
    expect(hostText()).toContain("keyboard_leds_ps2=0x00000000 (none)");
    expect(hostText()).toContain("io.batches_received=42");
    expect(hostText()).toContain("io.batches_processed=42");
    expect(hostText()).toContain("io.batches_dropped=0");
    expect(hostText()).toContain("io.events_processed=999");
    expect(hostText()).toContain("io.keyboard_backend_switches=6");
    expect(hostText()).toContain("io.mouse_backend_switches=7");
    expect(hostText()).toContain("io.batch_send_latency_us=3000 us (3.000 ms)");
    expect(hostText()).toContain("io.batch_send_latency_ewma_us=3500 us (3.500 ms)");
    expect(hostText()).toContain("io.batch_send_latency_max_us=4000 us (4.000 ms)");
    expect(hostText()).toContain("io.event_latency_avg_us=800 us (0.800 ms)");
    expect(hostText()).toContain("io.event_latency_ewma_us=900 us (0.900 ms)");
    expect(hostText()).toContain("io.event_latency_max_us=1000 us (1.000 ms)");
  });
});

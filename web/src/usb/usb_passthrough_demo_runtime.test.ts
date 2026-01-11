import { describe, expect, it } from "vitest";

import type { UsbSelectedMessage, UsbHostCompletion } from "./usb_proxy_protocol";
import {
  UsbPassthroughDemoRuntime,
  isUsbPassthroughDemoResultMessage,
  isUsbPassthroughDemoRunMessage,
} from "./usb_passthrough_demo_runtime";

class FakeDemo {
  actions: unknown[] = [];
  pushed: unknown[] = [];
  nextResult: unknown = null;
  nextId = 1;

  reset(): void {
    this.actions = [];
    this.pushed = [];
    this.nextResult = null;
  }

  queue_get_device_descriptor(len: number): void {
    this.actions.push({
      kind: "controlIn",
      id: this.nextId++,
      setup: {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0100,
        wIndex: 0,
        wLength: len,
      },
    });
  }

  queue_get_config_descriptor(len: number): void {
    this.actions.push({
      kind: "controlIn",
      id: this.nextId++,
      setup: {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0200,
        wIndex: 0,
        wLength: len,
      },
    });
  }

  drain_actions(): unknown {
    const out = this.actions;
    this.actions = [];
    return out;
  }

  push_completion(completion: unknown): void {
    this.pushed.push(completion);
    this.nextResult = { status: "success", data: [0x12, 0x34] };
  }

  poll_last_result(): unknown {
    const out = this.nextResult;
    this.nextResult = null;
    return out;
  }
}

describe("UsbPassthroughDemoRuntime", () => {
  it("validates usb.demoResult messages with a strict type guard", () => {
    expect(isUsbPassthroughDemoResultMessage({ type: "usb.demoResult", result: { status: "stall" } })).toBe(true);
    expect(isUsbPassthroughDemoResultMessage({ type: "usb.demoResult", result: { status: "error", message: "x" } })).toBe(true);
    expect(isUsbPassthroughDemoResultMessage({ type: "usb.demoResult", result: { status: "success", data: Uint8Array.of(1, 2) } })).toBe(
      true,
    );

    // Reject malformed shapes.
    expect(isUsbPassthroughDemoResultMessage({ type: "usb.demoResult", result: { status: "success", data: [1, 2] } })).toBe(false);
    expect(isUsbPassthroughDemoResultMessage({ type: "usb.demoResult", result: { status: "error" } })).toBe(false);
    expect(isUsbPassthroughDemoResultMessage({ type: "usb.demoResult" })).toBe(false);
  });

  it("validates usb.demo.run messages with a strict type guard", () => {
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "deviceDescriptor" })).toBe(true);
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "configDescriptor", length: 255 })).toBe(true);
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "configDescriptor", length: 0xffff })).toBe(true);

    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "unknown" })).toBe(false);
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "deviceDescriptor", length: -1 })).toBe(false);
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "deviceDescriptor", length: 1.5 })).toBe(false);
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run", request: "deviceDescriptor", length: 0x1_0000 })).toBe(false);
    expect(isUsbPassthroughDemoRunMessage({ type: "usb.demo.run" })).toBe(false);
  });

  it("queues a control-in action when usb.selected arrives", () => {
    const demo = new FakeDemo();
    const posted: unknown[] = [];

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    const selected: UsbSelectedMessage = { type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } };
    runtime.onUsbSelected(selected);
    runtime.tick();

    expect(posted).toEqual([
      {
        type: "usb.action",
        action: {
          kind: "controlIn",
          id: 1_000_000_000,
          setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0100, wIndex: 0, wLength: 18 },
        },
      },
    ]);
  });

  it("queues a control-in action when run() is invoked", () => {
    const demo = new FakeDemo();
    const posted: unknown[] = [];

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    runtime.run("configDescriptor", 9);

    expect(posted).toEqual([
      {
        type: "usb.action",
        action: {
          kind: "controlIn",
          id: 1_000_000_000,
          setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0200, wIndex: 0, wLength: 9 },
        },
      },
    ]);
  });

  it("does not reuse proxy action IDs after reset", () => {
    const demo = new FakeDemo();
    const posted: any[] = [];

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    const selected: UsbSelectedMessage = { type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } };
    runtime.onUsbSelected(selected);
    runtime.tick();

    runtime.reset();
    runtime.onUsbSelected(selected);
    runtime.tick();

    const ids = posted.filter((m) => m.type === "usb.action").map((m) => m.action.id);
    expect(ids).toHaveLength(2);
    expect(ids[1]).toBeGreaterThan(ids[0]);
  });

  it("feeds completions into the demo object and emits result messages", () => {
    const demo = new FakeDemo();
    const posted: unknown[] = [];

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    runtime.onUsbSelected({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });
    runtime.tick();

    const completion: UsbHostCompletion = { kind: "controlIn", id: 1_000_000_000, status: "success", data: Uint8Array.of(0x12, 0x34) };
    runtime.onUsbCompletion({ type: "usb.completion", completion });

    expect(demo.pushed).toEqual([
      {
        kind: "controlIn",
        id: 1,
        status: "success",
        data: Uint8Array.of(0x12, 0x34),
      },
    ]);

    expect(posted).toContainEqual({
      type: "usb.demoResult",
      result: { status: "success", data: Uint8Array.of(0x12, 0x34) },
    });
  });
});

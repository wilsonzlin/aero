import { describe, expect, it } from "vitest";

import type { UsbSelectedMessage, UsbHostCompletion } from "./usb_proxy_protocol";
import { UsbPassthroughDemoRuntime } from "./usb_passthrough_demo_runtime";

class FakeDemo {
  actions: unknown[] = [];
  pushed: unknown[] = [];
  nextResult: unknown = null;

  reset(): void {
    this.actions = [];
    this.pushed = [];
    this.nextResult = null;
  }

  queue_get_device_descriptor(len: number): void {
    this.actions.push({
      kind: "controlIn",
      id: 1,
      setup: {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0100,
        wIndex: 0,
        wLength: len,
      },
    });
  }

  queue_get_config_descriptor(): void {
    throw new Error("not used");
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

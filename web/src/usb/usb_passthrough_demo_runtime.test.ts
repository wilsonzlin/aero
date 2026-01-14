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

class CamelCaseDemo {
  actions: unknown[] = [];
  pushed: unknown[] = [];
  nextResult: unknown = null;
  nextId = 1;

  reset(): void {
    this.actions = [];
    this.pushed = [];
    this.nextResult = null;
  }

  queueGetDeviceDescriptor(len: number): void {
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

  queueGetConfigDescriptor(len: number): void {
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

  drainActions(): unknown {
    const out = this.actions;
    this.actions = [];
    return out;
  }

  pushCompletion(completion: unknown): void {
    this.pushed.push(completion);
    this.nextResult = { status: "success", data: [0x12, 0x34] };
  }

  pollLastResult(): unknown {
    const out = this.nextResult;
    this.nextResult = null;
    return out;
  }
}

class ThrowingDemo extends FakeDemo {
  constructor(private readonly mode: "drain" | "poll") {
    super();
  }

  override drain_actions(): unknown {
    if (this.mode === "drain") throw new Error("boom");
    return super.drain_actions();
  }

  override poll_last_result(): unknown {
    if (this.mode === "poll") throw new Error("boom");
    return super.poll_last_result();
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

  it("uses descriptor-specific default lengths when run() omits length", () => {
    const demo = new FakeDemo();
    const posted: any[] = [];

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    runtime.run("deviceDescriptor");
    runtime.run("configDescriptor");

    const actions = posted.filter((m) => m.type === "usb.action").map((m) => m.action);
    expect(actions).toHaveLength(2);
    expect(actions[0]!.setup.wValue).toBe(0x0100);
    expect(actions[0]!.setup.wLength).toBe(18);
    expect(actions[1]!.setup.wValue).toBe(0x0200);
    expect(actions[1]!.setup.wLength).toBe(255);
    expect(actions[1]!.id).toBeGreaterThan(actions[0]!.id);
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

  it("accepts camelCase demo exports (backwards compatibility)", () => {
    const demo = new CamelCaseDemo();
    const posted: unknown[] = [];

    const runtime = new UsbPassthroughDemoRuntime({
      demo: demo as unknown as any,
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

  it("propagates demo API exceptions from tick()/pollResults() so callers can surface errors", () => {
    const drainDemo = new ThrowingDemo("drain");
    const drainRuntime = new UsbPassthroughDemoRuntime({
      demo: drainDemo,
      postMessage: () => undefined,
    });
    expect(() => drainRuntime.tick()).toThrow("boom");

    const pollDemo = new ThrowingDemo("poll");
    const pollRuntime = new UsbPassthroughDemoRuntime({
      demo: pollDemo,
      postMessage: () => undefined,
    });
    expect(() => pollRuntime.pollResults()).toThrow("boom");
  });

  it("throws when the demo emits an action with an invalid byte payload (so callers can reset instead of hanging)", () => {
    const demo = new FakeDemo();
    demo.actions.push({
      kind: "bulkOut",
      id: 1,
      endpoint: 0x02,
      // Invalid byte array: string entries are not allowed.
      data: ["oops"],
    });

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: () => undefined,
    });

    expect(() => runtime.tick()).toThrow(/bulkOut/i);
  });

  it("throws when the demo emits an action with an invalid id (so the caller can reset instead of hanging)", () => {
    const demo = new FakeDemo();
    demo.actions.push({
      kind: "bulkIn",
      id: -1,
      endpoint: 0x81,
      length: 8,
    });

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: () => undefined,
    });

    expect(() => runtime.tick()).toThrow(/invalid usb action id/i);
  });

  it("throws when the demo emits an action that fails UsbHostAction validation", () => {
    const demo = new FakeDemo();
    demo.actions.push({
      kind: "controlIn",
      id: 1,
      setup: {
        bmRequestType: -1,
        bRequest: 0x06,
        wValue: 0x0100,
        wIndex: 0,
        wLength: 18,
      },
    });

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: () => undefined,
    });

    expect(() => runtime.tick()).toThrow(/invalid usb host action/i);
  });

  it("treats null drain_actions as no actions (compat with Option-returning wasm bindings)", () => {
    class NullActionsDemo extends FakeDemo {
      override drain_actions(): unknown {
        return null;
      }
    }

    const demo = new NullActionsDemo();
    const posted: unknown[] = [];
    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    expect(() => runtime.tick()).not.toThrow();
    expect(posted).toEqual([]);
  });

  it("throws when drain_actions returns a non-array value", () => {
    class NonArrayActionsDemo extends FakeDemo {
      override drain_actions(): unknown {
        return { kind: "controlIn" };
      }
    }

    const runtime = new UsbPassthroughDemoRuntime({
      demo: new NonArrayActionsDemo(),
      postMessage: () => undefined,
    });

    expect(() => runtime.tick()).toThrow(/invalid actions payload/i);
  });

  it("accepts ArrayBuffer demo result payloads (serde_wasm_bindgen can produce them)", () => {
    const demo = new FakeDemo();
    demo.nextResult = { status: "success", data: Uint8Array.of(1, 2).buffer };

    const posted: any[] = [];
    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    runtime.pollResults();

    expect(posted).toHaveLength(1);
    const msg = posted[0];
    expect(msg.type).toBe("usb.demoResult");
    expect(msg.result.status).toBe("success");
    expect(msg.result.data).toBeInstanceOf(Uint8Array);
    expect(Array.from(msg.result.data)).toEqual([1, 2]);
  });

  it("copies SAB-backed Uint8Array demo result payloads to ArrayBuffer-backed bytes", () => {
    if (typeof SharedArrayBuffer === "undefined") return;
    const sab = new SharedArrayBuffer(4);
    const view = new Uint8Array(sab);
    view.set([1, 2, 3, 4]);

    const demo = new FakeDemo();
    demo.nextResult = { status: "success", data: view.subarray(1, 3) };

    const posted: any[] = [];
    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    runtime.pollResults();

    expect(posted).toHaveLength(1);
    const msg = posted[0];
    expect(msg.type).toBe("usb.demoResult");
    expect(msg.result.status).toBe("success");
    expect(msg.result.data).toBeInstanceOf(Uint8Array);
    expect(msg.result.data.buffer).toBeInstanceOf(ArrayBuffer);
    expect(Array.from(msg.result.data)).toEqual([2, 3]);
  });

  it("copies Uint8Array subviews emitted by the demo before forwarding to the broker", () => {
    const demo = new FakeDemo();
    const big = Uint8Array.of(9, 8, 7, 6, 5, 4);
    const sub = new Uint8Array(big.buffer, 2, 2);
    demo.actions.push({
      kind: "bulkOut",
      id: 1,
      endpoint: 0x02,
      data: sub,
    });

    const posted: any[] = [];
    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: (msg) => posted.push(msg),
    });

    runtime.tick();

    expect(posted).toHaveLength(1);
    const msg = posted[0];
    expect(msg.type).toBe("usb.action");
    expect(msg.action.kind).toBe("bulkOut");
    expect(msg.action.data).toBeInstanceOf(Uint8Array);
    expect(msg.action.data.buffer).toBeInstanceOf(ArrayBuffer);
    expect(Array.from(msg.action.data)).toEqual([7, 6]);
  });

  it("throws when the demo emits an invalid result payload", () => {
    const demo = new FakeDemo();
    demo.nextResult = { status: "success", data: ["oops"] };

    const runtime = new UsbPassthroughDemoRuntime({
      demo,
      postMessage: () => undefined,
    });

    expect(() => runtime.pollResults()).toThrow(/invalid result payload/i);
  });
});

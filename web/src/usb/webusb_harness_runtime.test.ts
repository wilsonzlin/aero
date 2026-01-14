import { describe, expect, it, vi } from "vitest";

import { isUsbUhciHarnessStatusMessage, WebUsbUhciHarnessRuntime } from "./webusb_harness_runtime";
import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";
import { createUsbProxyRingBuffer } from "./usb_proxy_ring";

type Listener = (ev: MessageEvent<unknown>) => void;

class FakePort {
  readonly posted: unknown[] = [];
  private readonly listeners = new Set<Listener>();

  addEventListener(type: string, listener: Listener): void {
    if (type !== "message") return;
    this.listeners.add(listener);
  }

  removeEventListener(type: string, listener: Listener): void {
    if (type !== "message") return;
    this.listeners.delete(listener);
  }

  start(): void {
    // no-op
  }

  postMessage(msg: unknown): void {
    this.posted.push(msg);
  }

  emit(msg: unknown): void {
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of this.listeners) listener(ev);
  }
}

describe("usb/WebUsbUhciHarnessRuntime", () => {
  it("ticks the harness, forwards actions to the broker, and pushes completions back into the harness", () => {
    const port = new FakePort();

    const actions: UsbHostAction[] = [
      {
        kind: "controlIn",
        id: 1,
        setup: { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 18 },
      },
      { kind: "bulkOut", id: 2, endpoint: 1, data: Uint8Array.of(1, 2, 3) },
    ];

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => actions),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: true,
    });

    runtime.start();

    // Harness should be blocked until `usb.selected ok:true`.
    runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.querySelected" }]);

    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 0x1234, productId: 0x5678 } });

    runtime.pollOnce();
    expect(harness.tick).toHaveBeenCalledTimes(1);
    const postedActions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.action") as Array<{
      type: "usb.action";
      action: UsbHostAction;
    }>;
    expect(postedActions).toHaveLength(2);
    expect(postedActions[0]?.action.kind).toBe(actions[0].kind);
    expect(postedActions[1]?.action.kind).toBe(actions[1].kind);

    const brokerId1 = postedActions[0]!.action.id;
    const brokerId2 = postedActions[1]!.action.id;

    const c1Broker: UsbHostCompletion = { kind: "controlIn", id: brokerId1, status: "success", data: Uint8Array.of(9) };
    const c2Broker: UsbHostCompletion = { kind: "bulkOut", id: brokerId2, status: "success", bytesWritten: 3 };
    port.emit({ type: "usb.completion", completion: c2Broker });
    port.emit({ type: "usb.completion", completion: c1Broker });

    expect(harness.push_completion).toHaveBeenCalledTimes(2);
    expect(harness.push_completion.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkOut", id: 2, status: "success", bytesWritten: 3 });
    expect(harness.push_completion.mock.calls[1]?.[0]).toMatchObject({ kind: "controlIn", id: 1, status: "success" });

    const snapshot = runtime.getSnapshot();
    expect(snapshot.actionsForwarded).toBe(2);
    expect(snapshot.completionsApplied).toBe(2);
    expect(snapshot.pendingCompletions).toBe(0);
    expect(snapshot.lastAction?.id).toBe(2);
    expect(snapshot.lastCompletion?.id).toBe(1);
  });

  it("accepts camelCase harness exports (backwards compatibility)", () => {
    const port = new FakePort();

    const actions: UsbHostAction[] = [
      {
        kind: "controlIn",
        id: 1,
        setup: { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 18 },
      },
    ];

    const tick = vi.fn();
    const drainActions = vi.fn(() => actions);
    const pushCompletion = vi.fn();
    const free = vi.fn();

    const harness = { tick, drainActions, pushCompletion, free };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness as unknown as { tick(): void; drain_actions(): unknown; push_completion(_c: unknown): void; free(): void },
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });

    runtime.start();
    runtime.pollOnce();

    expect(tick).toHaveBeenCalledTimes(1);
    const posted = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.action") as Array<{
      type: "usb.action";
      action: UsbHostAction;
    }>;
    expect(posted).toHaveLength(1);

    const brokerId = posted[0]!.action.id;
    port.emit({
      type: "usb.completion",
      completion: { kind: "controlIn", id: brokerId, status: "success", data: Uint8Array.of(9) } satisfies UsbHostCompletion,
    });

    expect(pushCompletion).toHaveBeenCalledTimes(1);
    expect(pushCompletion.mock.calls[0]?.[0]).toMatchObject({ kind: "controlIn", id: 1, status: "success" });

    runtime.destroy();
    expect(free).toHaveBeenCalledTimes(1);
  });

  it("captures device + config descriptor bytes from GET_DESCRIPTOR(ControlIn) pairs", () => {
    const port = new FakePort();

    const deviceAction: UsbHostAction = {
      kind: "controlIn",
      id: 1,
      setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0100, wIndex: 0, wLength: 18 },
    };
    const configAction: UsbHostAction = {
      kind: "controlIn",
      id: 2,
      setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0200, wIndex: 0, wLength: 9 },
    };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [deviceAction, configAction]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();

    const posted = port.posted as Array<{ type: string; action: UsbHostAction }>;
    expect(posted).toHaveLength(3);
    const brokerId1 = posted[1]!.action.id;
    const brokerId2 = posted[2]!.action.id;

    port.emit({
      type: "usb.completion",
      completion: {
        kind: "controlIn",
        id: brokerId1,
        status: "success",
        data: Uint8Array.of(1, 2, 3),
      } satisfies UsbHostCompletion,
    });
    port.emit({
      type: "usb.completion",
      completion: { kind: "controlIn", id: brokerId2, status: "success", data: Uint8Array.of(9, 9) } satisfies UsbHostCompletion,
    });

    const snapshot = runtime.getSnapshot();
    expect(snapshot.deviceDescriptor).toEqual(Uint8Array.of(1, 2, 3));
    expect(snapshot.configDescriptor).toEqual(Uint8Array.of(9, 9));
  });

  it("copies SAB-backed harness action payload bytes before posting usb.action", () => {
    if (typeof SharedArrayBuffer === "undefined") return;
    const port = new FakePort();

    const sab = new SharedArrayBuffer(4);
    const view = new Uint8Array(sab);
    view.set([9, 8, 7, 6]);

    const action: UsbHostAction = { kind: "bulkOut", id: 1, endpoint: 0x02, data: view.subarray(1, 3) };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();

    const postedActions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.action") as Array<{
      type: "usb.action";
      action: UsbHostAction;
    }>;
    expect(postedActions).toHaveLength(1);

    const posted = postedActions[0]!.action;
    if (posted.kind !== "bulkOut") throw new Error("unreachable");
    expect(posted.data.buffer).toBeInstanceOf(ArrayBuffer);
    expect(Array.from(posted.data)).toEqual([8, 7]);
  });

  it("copies ArrayBuffer subviews emitted by the harness before posting usb.action", () => {
    const port = new FakePort();

    const big = Uint8Array.of(0, 9, 8, 7, 6, 0);
    const sub = new Uint8Array(big.buffer, 2, 2);

    const action: UsbHostAction = { kind: "bulkOut", id: 1, endpoint: 0x02, data: sub };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();

    const postedActions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.action") as Array<{
      type: "usb.action";
      action: UsbHostAction;
    }>;
    expect(postedActions).toHaveLength(1);

    const posted = postedActions[0]!.action;
    if (posted.kind !== "bulkOut") throw new Error("unreachable");
    expect(posted.data.buffer).toBeInstanceOf(ArrayBuffer);
    // The payload should not carry the full underlying buffer (only the slice).
    expect(posted.data.buffer.byteLength).toBe(2);
    expect(Array.from(posted.data)).toEqual([8, 7]);
  });

  it("stops + resets on usb.selected ok:false", () => {
    const port = new FakePort();

    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };
    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();
    expect(port.posted).toHaveLength(2);
    const posted = port.posted[1] as { type: string; action: UsbHostAction };
    expect(posted.type).toBe("usb.action");
    expect(posted.action.kind).toBe("bulkIn");

    port.emit({ type: "usb.selected", ok: false, error: "revoked" });

    const snapshot = runtime.getSnapshot();
    expect(snapshot.enabled).toBe(false);
    expect(snapshot.blocked).toBe(true);
    expect(snapshot.pendingCompletions).toBe(0);
  });

  it("stops when the harness emits an out-of-range action id (non-u32)", () => {
    const port = new FakePort();

    const action: unknown = {
      kind: "controlIn",
      id: 0x1_0000_0000n,
      setup: { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0100, wIndex: 0, wLength: 18 },
    };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();

    const snapshot = runtime.getSnapshot();
    expect(snapshot.enabled).toBe(false);
    expect(snapshot.lastError).toMatch(/uint32/i);
  });

  it("stops when the harness emits an oversized bulkIn length", () => {
    const port = new FakePort();

    const action: unknown = {
      kind: "bulkIn",
      id: 1,
      endpoint: 0x81,
      length: 10 * 1024 * 1024,
    };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();

    const snapshot = runtime.getSnapshot();
    expect(snapshot.enabled).toBe(false);
    expect(snapshot.lastError).toMatch(/length/i);
  });

  it("stops when the harness emits a controlOut action whose payload length does not match wLength", () => {
    const port = new FakePort();

    const action: unknown = {
      kind: "controlOut",
      id: 1,
      setup: { bmRequestType: 0, bRequest: 9, wValue: 1, wIndex: 0, wLength: 1 },
      data: [1, 2],
    };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });
    runtime.start();
    runtime.pollOnce();

    const snapshot = runtime.getSnapshot();
    expect(snapshot.enabled).toBe(false);
    expect(snapshot.lastError).toMatch(/controlOut/i);
  });

  it("validates usb.harness.status messages with a strict type guard", () => {
    const msg = {
      type: "usb.harness.status",
      snapshot: {
        available: true,
        enabled: false,
        blocked: true,
        tickCount: 0,
        actionsForwarded: 0,
        completionsApplied: 0,
        pendingCompletions: 0,
        lastAction: null,
        lastCompletion: null,
        deviceDescriptor: null,
        configDescriptor: null,
        lastError: null,
      },
    };
    expect(isUsbUhciHarnessStatusMessage(msg)).toBe(true);

    // Reject malformed shapes.
    expect(isUsbUhciHarnessStatusMessage({ type: "usb.harness.status" })).toBe(false);
    expect(
      isUsbUhciHarnessStatusMessage({
        type: "usb.harness.status",
        snapshot: { ...msg.snapshot, lastAction: { kind: "bulkIn", id: 1, endpoint: 1, length: 8 } },
      }),
    ).toBe(false);
  });

  it("does not stop on usb.ringDetach and pushes error completions for pending actions", () => {
    const port = new FakePort();

    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };

    const harness = {
      tick: vi.fn(),
      drain_actions: vi.fn().mockReturnValueOnce([action]).mockReturnValue([]),
      push_completion: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbUhciHarnessRuntime({
      createHarness: () => harness,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });

    runtime.start();
    port.emit({
      type: "usb.ringAttach",
      actionRing: createUsbProxyRingBuffer(256),
      completionRing: createUsbProxyRingBuffer(256),
    });
    port.posted.length = 0;

    runtime.pollOnce();
    expect(harness.tick).toHaveBeenCalledTimes(1);
    expect(harness.push_completion).toHaveBeenCalledTimes(0);

    port.emit({ type: "usb.ringDetach", reason: "corrupt ring" });

    expect(harness.push_completion).toHaveBeenCalledTimes(1);
    expect(harness.push_completion.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkIn", id: 1, status: "error" });

    const snapshot = runtime.getSnapshot();
    expect(snapshot.enabled).toBe(true);
    expect(snapshot.pendingCompletions).toBe(0);
    expect(snapshot.lastError).toBe("corrupt ring");

    expect(port.posted.filter((m) => (m as { type?: unknown }).type === "usb.ringDetach")).toHaveLength(0);
  });
});

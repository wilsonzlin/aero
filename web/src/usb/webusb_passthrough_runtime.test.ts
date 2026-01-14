import { describe, expect, it, vi } from "vitest";

import { WebUsbPassthroughRuntime, type UsbPassthroughBridgeLike } from "./webusb_passthrough_runtime";
import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";

type Listener = (ev: MessageEvent<unknown>) => void;

class FakePort {
  readonly posted: unknown[] = [];
  readonly transfers: Array<Transferable[] | undefined> = [];
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
    // No-op; browsers require MessagePort.start() when using addEventListener.
  }

  postMessage(msg: unknown, transfer?: Transferable[]): void {
    this.posted.push(msg);
    this.transfers.push(transfer);
  }

  emit(msg: unknown): void {
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of this.listeners) {
      listener(ev);
    }
  }
}

describe("usb/WebUsbPassthroughRuntime", () => {
  it("drains actions and forwards them to the broker port as usb.action messages", async () => {
    const port = new FakePort();

    const bulkOutData = Uint8Array.of(1, 2, 3);
    const actions: UsbHostAction[] = [
      {
        kind: "controlIn",
        id: 1,
        setup: { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 18 },
      },
      { kind: "bulkOut", id: 2, endpoint: 1, data: bulkOutData },
    ];

    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 0x1234, productId: 0x5678 } });

    const p = runtime.pollOnce();

    expect(port.posted).toEqual([
      { type: "usb.ringAttachRequest" },
      { type: "usb.action", action: actions[0] },
      { type: "usb.action", action: actions[1] },
    ]);
    const bulkOut = actions[1];
    if (!bulkOut || bulkOut.kind !== "bulkOut") throw new Error("unreachable");
    expect(port.transfers).toEqual([undefined, undefined, [bulkOut.data.buffer]]);

    port.emit({
      type: "usb.completion",
      completion: { kind: "controlIn", id: 1, status: "success", data: Uint8Array.of(9) } satisfies UsbHostCompletion,
    });
    port.emit({
      type: "usb.completion",
      completion: { kind: "bulkOut", id: 2, status: "success", bytesWritten: 3 } satisfies UsbHostCompletion,
    });

    await p;
  });

  it("accepts camelCase UsbPassthroughBridge exports (backwards compatibility)", async () => {
    const port = new FakePort();

    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };
    const pushCompletion = vi.fn();
    const pendingSummary = vi.fn(() => ({ pending: 0 }));

    const bridge = {
      drainActions: vi.fn(() => [action]),
      pushCompletion,
      pendingSummary,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({
      bridge: bridge as unknown as UsbPassthroughBridgeLike,
      port: port as unknown as MessagePort,
      pollIntervalMs: 0,
    });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.action", action }]);

    const completion: UsbHostCompletion = { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(9) };
    port.emit({ type: "usb.completion", completion } satisfies { type: "usb.completion"; completion: UsbHostCompletion });

    await p;
    expect(pushCompletion).toHaveBeenCalledTimes(1);
    expect(pushCompletion).toHaveBeenCalledWith(completion);

    expect(runtime.pendingSummary()).toEqual({ pending: 0 });
    expect(pendingSummary).toHaveBeenCalledTimes(1);

    runtime.destroy();
    expect(bridge.free).toHaveBeenCalledTimes(1);
  });

  it("respects initiallyBlocked and waits for usb.selected ok:true before forwarding actions", async () => {
    const port = new FakePort();

    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({
      bridge,
      port: port as unknown as MessagePort,
      pollIntervalMs: 0,
      initiallyBlocked: true,
    });

    await runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.querySelected" }]);

    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();
    expect(port.posted).toEqual([
      { type: "usb.ringAttachRequest" },
      { type: "usb.querySelected" },
      { type: "usb.action", action },
    ]);
    port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 1, status: "stall" } satisfies UsbHostCompletion });
    await p;
  });

  it("falls back to non-transfer postMessage when the payload buffer cannot be transferred", async () => {
    class ThrowOnTransferPort extends FakePort {
      override postMessage(msg: unknown, transfer?: Transferable[]): void {
        if (transfer && transfer.length > 0) throw new Error("transfer not supported");
        super.postMessage(msg, transfer);
      }
    }

    const port = new ThrowOnTransferPort();

    const actions: UsbHostAction[] = [{ kind: "bulkOut", id: 1, endpoint: 1, data: Uint8Array.of(1, 2, 3) }];

    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.action", action: actions[0] }]);
    // Transfer list was rejected, so the runtime should retry without transferables.
    expect(port.transfers).toEqual([undefined, undefined]);

    port.emit({
      type: "usb.completion",
      completion: { kind: "bulkOut", id: 1, status: "success", bytesWritten: 3 } satisfies UsbHostCompletion,
    });

    await p;
    expect(runtime.getMetrics().lastError).toBeNull();
  });

  it("normalizes bigint ids from WASM actions before forwarding to the broker", async () => {
    const port = new FakePort();

    const rawActions = [{ kind: "bulkIn", id: 1n, endpoint: 0x81, length: 8 }];

    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => rawActions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();

    expect(port.posted).toEqual([
      { type: "usb.ringAttachRequest" },
      { type: "usb.action", action: { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 } },
    ]);

    const completion: UsbHostCompletion = { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) };
    port.emit({ type: "usb.completion", completion });

    await p;

    expect(push_completion).toHaveBeenCalledTimes(1);
    expect(push_completion.mock.calls[0]?.[0]).toBe(completion);
  });

  it("pushes usb.completion replies back into the WASM bridge (matching out of order by id)", async () => {
    const port = new FakePort();

    const actions: UsbHostAction[] = [
      { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 },
      { kind: "bulkOut", id: 2, endpoint: 2, data: Uint8Array.of(7, 7) },
    ];

    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => actions),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();

    const c2: UsbHostCompletion = { kind: "bulkOut", id: 2, status: "success", bytesWritten: 2 };
    const c1: UsbHostCompletion = { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1, 2) };

    // Emit out-of-order to ensure the runtime matches by id.
    port.emit({ type: "usb.completion", completion: c2 });
    port.emit({ type: "usb.completion", completion: c1 });

    await p;

    expect(push_completion).toHaveBeenCalledTimes(2);
    expect(push_completion.mock.calls[0]?.[0]).toBe(c2);
    expect(push_completion.mock.calls[1]?.[0]).toBe(c1);

    expect(runtime.getMetrics()).toMatchObject({ actionsForwarded: 2, completionsApplied: 2, pendingCompletions: 0 });
  });

  it("stops pumping and resets the bridge on usb.selected ok:false", async () => {
    const port = new FakePort();

    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };
    const drain_actions = vi.fn(() => [action]);
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions,
      push_completion: vi.fn(),
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.action", action }]);

    // No completion is delivered; selecting ok:false should cancel the in-flight action and reset the bridge.
    port.emit({ type: "usb.selected", ok: false, error: "device revoked" });

    await p;
    expect(reset).toHaveBeenCalledTimes(1);

    await runtime.pollOnce();
    expect(drain_actions).toHaveBeenCalledTimes(1);
  });

  it("treats null/undefined drain_actions() as \"no work\" (idle polling fast path)", async () => {
    const port = new FakePort();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => null),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
  });

  it("limits forwarded actions per pollOnce when maxActionsPerPoll is set", async () => {
    const port = new FakePort();

    const actions: UsbHostAction[] = [
      { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 },
      { kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 },
      { kind: "bulkIn", id: 3, endpoint: 0x81, length: 8 },
    ];

    const drain_actions = vi.fn(() => actions);
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions,
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({
      bridge,
      port: port as unknown as MessagePort,
      pollIntervalMs: 0,
      maxActionsPerPoll: 2,
    });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p1 = runtime.pollOnce();
    expect(port.posted).toEqual([
      { type: "usb.ringAttachRequest" },
      { type: "usb.action", action: actions[0] },
      { type: "usb.action", action: actions[1] },
    ]);
    expect(drain_actions).toHaveBeenCalledTimes(1);

    port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 1, status: "stall" } satisfies UsbHostCompletion });
    port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 2, status: "stall" } satisfies UsbHostCompletion });
    await p1;

    const p2 = runtime.pollOnce();
    expect(drain_actions).toHaveBeenCalledTimes(1);
    expect(port.posted.slice(3)).toEqual([{ type: "usb.action", action: actions[2] }]);
    port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 3, status: "stall" } satisfies UsbHostCompletion });
    await p2;
  });

  it("normalizes BigInt ids emitted by WASM into Number ids for the broker protocol", async () => {
    const port = new FakePort();

    const rawAction = { kind: "bulkIn", id: 1n, endpoint: 0x81, length: 8 };

    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    port.emit({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } });

    const p = runtime.pollOnce();

    expect(port.posted).toHaveLength(2);
    const msg = port.posted[1] as { type: string; action: { id: unknown } };
    expect(msg.type).toBe("usb.action");
    expect(msg.action.id).toBe(1);
    expect(typeof msg.action.id).toBe("number");

    port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 1, status: "stall" } satisfies UsbHostCompletion });
    await p;
  });

  it("pushes an error completion when forwarding an action to the broker throws", async () => {
    class ThrowingPort extends FakePort {
      override postMessage(_msg: unknown): void {
        throw new Error("boom");
      }
    }

    const port = new ThrowingPort();
    const action: UsbHostAction = { kind: "bulkOut", id: 7, endpoint: 2, data: Uint8Array.of(1) };

    const push_completion = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [action]),
      push_completion,
      reset: vi.fn(),
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(push_completion).toHaveBeenCalledTimes(1);
    const completion = push_completion.mock.calls[0]?.[0] as UsbHostCompletion;
    expect(completion.kind).toBe("bulkOut");
    expect(completion.id).toBe(7);
    expect(completion.status).toBe("error");
  });

  it("pushes an error completion when WASM emits an invalid action (but includes id/kind)", async () => {
    const port = new FakePort();
    const rawAction = { kind: "bulkIn", id: 3, endpoint: 0x81, length: "nope" };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
    expect(reset).not.toHaveBeenCalled();

    expect(push_completion).toHaveBeenCalledTimes(1);
    const completion = push_completion.mock.calls[0]?.[0] as UsbHostCompletion;
    expect(completion.kind).toBe("bulkIn");
    expect(completion.id).toBe(3);
    expect(completion.status).toBe("error");
  });

  it("pushes an error completion when WASM emits an action with an invalid byte array payload", async () => {
    const port = new FakePort();
    const rawAction = { kind: "bulkOut", id: 3, endpoint: 0x02, data: [1, 256] };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
    expect(reset).not.toHaveBeenCalled();

    expect(push_completion).toHaveBeenCalledTimes(1);
    const completion = push_completion.mock.calls[0]?.[0] as UsbHostCompletion;
    expect(completion.kind).toBe("bulkOut");
    expect(completion.id).toBe(3);
    expect(completion.status).toBe("error");
  });

  it("pushes an error completion when WASM emits a controlOut action whose data length does not match wLength", async () => {
    const port = new FakePort();
    const rawAction = {
      kind: "controlOut",
      id: 3,
      setup: { bmRequestType: 0, bRequest: 9, wValue: 1, wIndex: 0, wLength: 1 },
      data: [1, 2],
    };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
    expect(reset).not.toHaveBeenCalled();

    expect(push_completion).toHaveBeenCalledTimes(1);
    const completion = push_completion.mock.calls[0]?.[0] as UsbHostCompletion;
    expect(completion.kind).toBe("controlOut");
    expect(completion.id).toBe(3);
    expect(completion.status).toBe("error");
  });

  it("resets the bridge when WASM emits an invalid action without id/kind", async () => {
    const port = new FakePort();
    const rawAction = { endpoint: 1, length: 8 };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
    expect(push_completion).not.toHaveBeenCalled();
    expect(reset).toHaveBeenCalledTimes(1);
    expect(runtime.getMetrics().lastError).toMatch(/missing id/);
  });

  it("synthesizes an error completion when the broker sends an invalid usb.completion payload", async () => {
    const port = new FakePort();
    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [action]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });

    const p = runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.action", action }]);

    // `data` is an array instead of a Uint8Array, so it fails validation.
    port.emit({
      type: "usb.completion",
      completion: { kind: "bulkIn", id: 1, status: "success", data: [1, 2, 3] },
    });

    await p;

    expect(reset).not.toHaveBeenCalled();
    expect(push_completion).toHaveBeenCalledTimes(1);
    const completion = push_completion.mock.calls[0]?.[0] as UsbHostCompletion;
    expect(completion.kind).toBe("bulkIn");
    expect(completion.id).toBe(1);
    expect(completion.status).toBe("error");
  });

  it("resets the bridge when WASM emits an out-of-range (non-u32) action id", async () => {
    const port = new FakePort();
    const rawAction = { kind: "bulkIn", id: 0x1_0000_0000n, endpoint: 0x81, length: 8 };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
    expect(push_completion).not.toHaveBeenCalled();
    expect(reset).toHaveBeenCalledTimes(1);
    expect(runtime.getMetrics().lastError).toMatch(/invalid id/);
  });

  it("pushes an error completion when WASM emits a bulkIn action with an excessive length", async () => {
    const port = new FakePort();
    const rawAction = { kind: "bulkIn", id: 3, endpoint: 0x81, length: 10 * 1024 * 1024 };

    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [rawAction]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });
    await runtime.pollOnce();

    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }]);
    expect(reset).not.toHaveBeenCalled();
    expect(push_completion).toHaveBeenCalledTimes(1);
    expect(push_completion.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkIn", id: 3, status: "error" });
  });

  it("ignores usb.ringDetach when the runtime never attached rings (postMessage path remains active)", async () => {
    const port = new FakePort();

    const action: UsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };
    const push_completion = vi.fn();
    const reset = vi.fn();
    const bridge: UsbPassthroughBridgeLike = {
      drain_actions: vi.fn(() => [action]),
      push_completion,
      reset,
      free: vi.fn(),
    };

    const runtime = new WebUsbPassthroughRuntime({ bridge, port: port as unknown as MessagePort, pollIntervalMs: 0 });

    const p = runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.action", action }]);

    // Another runtime on the same port may trigger ring detach; this runtime should not reset/cancel
    // since it wasn't using rings.
    port.emit({ type: "usb.ringDetach", reason: "disabled" });
    expect(reset).not.toHaveBeenCalled();

    port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 1, status: "stall" } satisfies UsbHostCompletion });
    await p;

    expect(push_completion).toHaveBeenCalledTimes(1);
    expect(push_completion.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkIn", id: 1, status: "stall" });
    expect(port.posted.filter((m) => (m as { type?: unknown }).type === "usb.ringDetach")).toHaveLength(0);
  });
});

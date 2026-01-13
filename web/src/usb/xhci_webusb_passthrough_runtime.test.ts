import { describe, expect, it, vi } from "vitest";

import { WebUsbPassthroughRuntime, type UsbPassthroughBridgeLike } from "./webusb_passthrough_runtime";
import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";
import { createUsbProxyRingBuffer, UsbProxyRing } from "./usb_proxy_ring";

type Listener = (ev: MessageEvent<unknown>) => void;

class FakePort {
  readonly posted: Array<{ msg: unknown; transfer?: Transferable[] }> = [];
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
    this.posted.push({ msg, transfer });
  }

  emit(msg: unknown): void {
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of this.listeners) listener(ev);
  }
}

class FakeUsbPassthroughBridge implements UsbPassthroughBridgeLike {
  private queue: UsbHostAction[] = [];
  private readonly pending = new Map<number, UsbHostAction["kind"]>();

  readonly applied: UsbHostCompletion[] = [];
  drainCalls = 0;
  resetCalls = 0;

  enqueue(action: UsbHostAction): void {
    this.queue.push(action);
  }

  drain_actions(): unknown {
    this.drainCalls++;
    if (this.queue.length === 0) return null;
    const out = this.queue;
    this.queue = [];
    for (const action of out) this.pending.set(action.id, action.kind);
    return out;
  }

  push_completion(completion: UsbHostCompletion): void {
    const expectedKind = this.pending.get(completion.id);
    if (!expectedKind) {
      throw new Error(`Unexpected completion id=${completion.id} (kind=${completion.kind})`);
    }
    if (expectedKind !== completion.kind) {
      throw new Error(`Completion kind mismatch for id=${completion.id}: expected ${expectedKind}, got ${completion.kind}`);
    }
    this.pending.delete(completion.id);
    this.applied.push(completion);
  }

  reset(): void {
    this.resetCalls++;
    this.queue = [];
    this.pending.clear();
  }

  free(): void {
    // no-op
  }
}

describe("usb/xhci_webusb_passthrough_runtime", () => {
  it("proxies actions->broker->completions via rings, ignores invalid completion ids, respects blocked state, and falls back after ringDetach", async () => {
    vi.useFakeTimers();
    try {
      const port = new FakePort();
      const bridge = new FakeUsbPassthroughBridge();

      // Queue actions up-front; when blocked, the runtime must not drain them yet.
      const a1: UsbHostAction = {
        kind: "controlIn",
        id: 1,
        setup: { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 1 },
      };
      const a2: UsbHostAction = { kind: "bulkIn", id: 2, endpoint: 0x81, length: 4 };
      bridge.enqueue(a1);
      bridge.enqueue(a2);

      const runtime = new WebUsbPassthroughRuntime({
        bridge,
        port: port as unknown as MessagePort,
        pollIntervalMs: 0,
        initiallyBlocked: true,
      });

      const actionRingBuf = createUsbProxyRingBuffer(256);
      const completionRingBuf = createUsbProxyRingBuffer(256);
      port.emit({ type: "usb.ringAttach", actionRing: actionRingBuf, completionRing: completionRingBuf });

      // Blocked: should not drain or forward.
      await runtime.pollOnce();
      expect(bridge.drainCalls).toBe(0);
      expect(port.posted.map((p) => (p.msg as { type?: unknown }).type)).toEqual(["usb.ringAttachRequest", "usb.querySelected"]);

      // Unblock.
      port.emit({ type: "usb.selected", ok: true, info: { vendorId: 0x1234, productId: 0x5678 } });

      // First poll uses the ring fast path (no usb.action postMessage per action).
      const poll1 = runtime.pollOnce();

      const actionRing = new UsbProxyRing(actionRingBuf);
      const completionRing = new UsbProxyRing(completionRingBuf);

      const seen: UsbHostAction[] = [];
      for (;;) {
        const next = actionRing.popAction();
        if (!next) break;
        seen.push(next);
      }

      expect(seen.map((a) => `${a.kind}:${a.id}`)).toEqual(["controlIn:1", "bulkIn:2"]);
      const postedActions = port.posted.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
      expect(postedActions).toHaveLength(0);

      // Send an unrelated completion id first: must be ignored (should not reach push_completion).
      expect(completionRing.pushCompletion({ kind: "bulkIn", id: 999, status: "stall" })).toBe(true);

      // Then satisfy the real actions.
      expect(completionRing.pushCompletion({ kind: "controlIn", id: 1, status: "success", data: Uint8Array.of(9) })).toBe(true);
      expect(completionRing.pushCompletion({ kind: "bulkIn", id: 2, status: "stall" })).toBe(true);

      await vi.advanceTimersByTimeAsync(8);
      await poll1;

      expect(bridge.applied.map((c) => c.id)).toEqual([1, 2]);
      expect(bridge.applied.some((c) => c.id === 999)).toBe(false);

      // Ring detach should reset bridge and the runtime must fall back to postMessage proxying.
      port.emit({ type: "usb.ringDetach", reason: "disabled for test" });
      expect(bridge.resetCalls).toBe(1);

      const a3: UsbHostAction = { kind: "bulkIn", id: 3, endpoint: 0x81, length: 8 };
      bridge.enqueue(a3);

      const poll2 = runtime.pollOnce();

      const usbActionMsgs = port.posted.filter((p) => (p.msg as { type?: unknown }).type === "usb.action");
      expect(usbActionMsgs).toHaveLength(1);
      expect((usbActionMsgs[0]!.msg as { action?: UsbHostAction }).action).toMatchObject({ kind: "bulkIn", id: 3 });

      // Complete via the postMessage path.
      port.emit({ type: "usb.completion", completion: { kind: "bulkIn", id: 3, status: "stall" } satisfies UsbHostCompletion });
      await poll2;

      expect(bridge.applied.map((c) => c.id)).toEqual([1, 2, 3]);

      runtime.destroy();
    } finally {
      vi.useRealTimers();
    }
  });
});


import { describe, expect, it, vi } from "vitest";

import { applyUsbSelectedToWebUsbBridgeForMode } from "./usb_guest_controller";
import { WebUsbPassthroughRuntime } from "./webusb_passthrough_runtime";
import type { UsbHostAction, UsbHostCompletion, UsbSelectedMessage } from "./usb_proxy_protocol";

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
    // No-op; browsers require MessagePort.start() when using addEventListener.
  }

  postMessage(msg: unknown): void {
    this.posted.push(msg);
  }

  emit(msg: unknown): void {
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of this.listeners) listener(ev);
  }
}

describe("usb/usb_guest_controller", () => {
  it("routes usb.selected to the EHCI bridge when mode=ehci and pumps actions/completions", async () => {
    const port = new FakePort();

    const uhci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => null),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const action: UsbHostAction = {
      kind: "bulkIn",
      id: 1,
      endpoint: 0x81,
      length: 8,
    };

    const ehci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => [action]),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const selected: UsbSelectedMessage = { type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2 } };
    applyUsbSelectedToWebUsbBridgeForMode("ehci", { uhci, ehci }, selected);
    expect(ehci.set_connected).toHaveBeenCalledWith(true);
    expect(uhci.set_connected).not.toHaveBeenCalled();

    const runtime = new WebUsbPassthroughRuntime({
      bridge: ehci,
      port: port as unknown as MessagePort,
      pollIntervalMs: 0,
      initiallyBlocked: true,
    });

    // Unblock the runtime with the same selection message the worker would observe.
    port.emit(selected);

    const poll = runtime.pollOnce();
    expect(port.posted).toEqual([{ type: "usb.ringAttachRequest" }, { type: "usb.querySelected" }, { type: "usb.action", action }]);

    const completion: UsbHostCompletion = { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(9) };
    port.emit({ type: "usb.completion", completion } satisfies { type: "usb.completion"; completion: UsbHostCompletion });

    await poll;
    expect(ehci.push_completion).toHaveBeenCalledTimes(1);
    expect(ehci.push_completion).toHaveBeenCalledWith(completion);
  });
});


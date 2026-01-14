import { describe, expect, it, vi } from "vitest";

import { isUsbEhciHarnessStatusMessage, WebUsbEhciHarnessRuntime } from "./webusb_ehci_harness_runtime";
import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";

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

describe("usb/WebUsbEhciHarnessRuntime (ehci)", () => {
  it("validates usb.ehciHarness.status messages with a strict type guard (ehci)", () => {
    const msg = {
      type: "usb.ehciHarness.status",
      snapshot: {
        available: true,
        blocked: true,
        controllerAttached: false,
        deviceAttached: false,
        tickCount: 0,
        actionsForwarded: 0,
        completionsApplied: 0,
        pendingCompletions: 0,
        irqLevel: false,
        usbSts: 0,
        usbStsUsbInt: false,
        usbStsUsbErrInt: false,
        usbStsPcd: false,
        lastAction: null,
        lastCompletion: null,
        deviceDescriptor: null,
        configDescriptor: null,
        lastError: null,
      },
    };

    expect(isUsbEhciHarnessStatusMessage(msg)).toBe(true);

    // Reject malformed shapes.
    expect(isUsbEhciHarnessStatusMessage({ type: "usb.ehciHarness.status" })).toBe(false);
    expect(
      isUsbEhciHarnessStatusMessage({
        type: "usb.ehciHarness.status",
        snapshot: { ...msg.snapshot, lastAction: { kind: "bulkIn", id: 1, endpoint: 1, length: 8 } },
      }),
    ).toBe(false);
  });

  it("accepts camelCase harness exports (backwards compatibility)", () => {
    const port = new FakePort();

    const actions: UsbHostAction[] = [{ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 }];

    const harness = {
      attachController: vi.fn(),
      detachController: vi.fn(),
      attachDevice: vi.fn(),
      detachDevice: vi.fn(),
      cmdGetDeviceDescriptor: vi.fn(),
      cmdGetConfigDescriptor: vi.fn(),
      clearUsbsts: vi.fn(),
      tick: vi.fn(),
      drainActions: vi.fn(() => actions),
      pushCompletion: vi.fn(),
      controllerAttached: vi.fn(() => true),
      deviceAttached: vi.fn(() => true),
      usbSts: vi.fn(() => 0),
      irqLevel: vi.fn(() => false),
      lastError: vi.fn(() => null),
      free: vi.fn(),
    };

    const runtime = new WebUsbEhciHarnessRuntime({
      createHarness: () => harness as unknown as any,
      port: port as unknown as MessagePort,
      initiallyBlocked: false,
    });

    runtime.attachController();
    expect(harness.attachController).toHaveBeenCalledTimes(1);

    runtime.pollOnce();
    expect(harness.tick).toHaveBeenCalledTimes(1);

    const actionMsg = port.posted.find((m) => (m as { type?: unknown }).type === "usb.action") as
      | { type: "usb.action"; action: UsbHostAction }
      | undefined;
    expect(actionMsg).toBeTruthy();

    const brokerId = actionMsg!.action.id;
    port.emit({
      type: "usb.completion",
      completion: { kind: "bulkIn", id: brokerId, status: "stall" } satisfies UsbHostCompletion,
    });

    expect(harness.pushCompletion).toHaveBeenCalledTimes(1);
    expect(harness.pushCompletion.mock.calls[0]?.[0]).toMatchObject({ kind: "bulkIn", id: 1, status: "stall" });

    runtime.destroy();
    expect(harness.free).toHaveBeenCalledTimes(1);
  });
});

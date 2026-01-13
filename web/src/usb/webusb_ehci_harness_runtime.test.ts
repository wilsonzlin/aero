import { describe, expect, it } from "vitest";

import { isUsbEhciHarnessStatusMessage } from "./webusb_ehci_harness_runtime";

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
});


import { describe, expect, it, vi } from "vitest";

import { createUsbBrokerSubportNoOtherSpeedTranslation } from "./usb_broker_subport";

describe("webusb_guest_selection usb broker subport", () => {
  it("requests a dedicated port with OTHER_SPEED_CONFIGURATION translation disabled", () => {
    const parent = {
      postMessage: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    };

    const port = createUsbBrokerSubportNoOtherSpeedTranslation(parent);

    expect(parent.postMessage).toHaveBeenCalledTimes(1);
    const [msg, transfer] = parent.postMessage.mock.calls[0]!;
    expect(msg).toMatchObject({
      type: "usb.broker.attachPort",
      attachRings: false,
      backendOptions: { translateOtherSpeedConfigurationDescriptor: false },
    });
    expect(Array.isArray(transfer)).toBe(true);
    expect(transfer).toHaveLength(1);
    expect(transfer[0]).toBeInstanceOf(MessagePort);
    expect(port).toBeInstanceOf(MessagePort);

    // Ensure open ports don't keep the test runner alive.
    try {
      (port as MessagePort).close();
    } catch {
      // ignore
    }
    try {
      (transfer[0] as MessagePort).close();
    } catch {
      // ignore
    }
  });
});


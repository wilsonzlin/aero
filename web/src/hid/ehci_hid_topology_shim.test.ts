import { describe, expect, it, vi } from "vitest";

import { createEhciTopologyBridgeShim } from "./ehci_hid_topology_shim";
import { UhciHidTopologyManager, type UhciTopologyBridge } from "./uhci_hid_topology";
import { EXTERNAL_HUB_ROOT_PORT, WEBUSB_GUEST_ROOT_PORT } from "../usb/uhci_external_hub";

function createFakeBridge(): UhciTopologyBridge & {
  attach_hub: ReturnType<typeof vi.fn>;
  detach_at_path: ReturnType<typeof vi.fn>;
  attach_webhid_device: ReturnType<typeof vi.fn>;
  attach_usb_hid_passthrough_device: ReturnType<typeof vi.fn>;
} {
  return {
    attach_hub: vi.fn(),
    detach_at_path: vi.fn(),
    attach_webhid_device: vi.fn(),
    attach_usb_hid_passthrough_device: vi.fn(),
  };
}

describe("hid/createEhciTopologyBridgeShim", () => {
  it("passes UHCI-style root ports through unchanged for hub + device topology calls", () => {
    const ehci = createFakeBridge();
    const shim = createEhciTopologyBridgeShim(ehci);
    const dev = { kind: "device" };

    shim.attach_hub(EXTERNAL_HUB_ROOT_PORT, 16);
    shim.attach_hub(WEBUSB_GUEST_ROOT_PORT, 8);
    shim.detach_at_path([EXTERNAL_HUB_ROOT_PORT, 5]);
    shim.attach_webhid_device([EXTERNAL_HUB_ROOT_PORT, 6], dev);
    shim.attach_usb_hid_passthrough_device([WEBUSB_GUEST_ROOT_PORT, 7], dev);

    expect(ehci.attach_hub).toHaveBeenNthCalledWith(1, EXTERNAL_HUB_ROOT_PORT, 16);
    expect(ehci.attach_hub).toHaveBeenNthCalledWith(2, WEBUSB_GUEST_ROOT_PORT, 8);
    expect(ehci.detach_at_path).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 5]);
    expect(ehci.attach_webhid_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 6], dev);
    expect(ehci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([WEBUSB_GUEST_ROOT_PORT, 7], dev);
  });

  it("accepts camelCase topology helper exports (backwards compatibility)", () => {
    const thisContexts = {
      attachHub: [] as unknown[],
      detachAtPath: [] as unknown[],
      attachWebHidDevice: [] as unknown[],
      attachUsbHidPassthroughDevice: [] as unknown[],
    };

    const ehci = {
      attachHub: vi.fn(function (this: unknown) {
        thisContexts.attachHub.push(this);
      }),
      detachAtPath: vi.fn(function (this: unknown) {
        thisContexts.detachAtPath.push(this);
      }),
      attachWebHidDevice: vi.fn(function (this: unknown) {
        thisContexts.attachWebHidDevice.push(this);
      }),
      attachUsbHidPassthroughDevice: vi.fn(function (this: unknown) {
        thisContexts.attachUsbHidPassthroughDevice.push(this);
      }),
    };
    const shim = createEhciTopologyBridgeShim(ehci as unknown as UhciTopologyBridge);
    const dev = { kind: "device" };

    shim.attach_hub(EXTERNAL_HUB_ROOT_PORT, 16);
    shim.detach_at_path([EXTERNAL_HUB_ROOT_PORT, 5]);
    shim.attach_webhid_device([EXTERNAL_HUB_ROOT_PORT, 6], dev);
    shim.attach_usb_hid_passthrough_device([WEBUSB_GUEST_ROOT_PORT, 7], dev);

    expect(ehci.attachHub).toHaveBeenCalledWith(EXTERNAL_HUB_ROOT_PORT, 16);
    expect(ehci.detachAtPath).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 5]);
    expect(ehci.attachWebHidDevice).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 6], dev);
    expect(ehci.attachUsbHidPassthroughDevice).toHaveBeenCalledWith([WEBUSB_GUEST_ROOT_PORT, 7], dev);

    // Ensure wasm-bindgen method calls preserve `this` binding when invoked through the shim.
    expect(thisContexts.attachHub[0]).toBe(ehci);
    expect(thisContexts.detachAtPath[0]).toBe(ehci);
    expect(thisContexts.attachWebHidDevice[0]).toBe(ehci);
    expect(thisContexts.attachUsbHidPassthroughDevice[0]).toBe(ehci);
  });

  it("allows UhciHidTopologyManager to attach its external hub on EHCI root port 0", () => {
    const ehci = createFakeBridge();
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    mgr.setUhciBridge(createEhciTopologyBridgeShim(ehci));
    expect(ehci.attach_hub).toHaveBeenCalledWith(EXTERNAL_HUB_ROOT_PORT, 16);
  });
});

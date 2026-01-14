import { describe, expect, it, vi } from "vitest";

import { createEhciTopologyBridgeShim } from "./ehci_hid_topology_shim";
import { UhciHidTopologyManager, type UhciTopologyBridge } from "./uhci_hid_topology";

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
  it("swaps EHCI root ports 0/1 for hub + device topology calls", () => {
    const ehci = createFakeBridge();
    const shim = createEhciTopologyBridgeShim(ehci);
    const dev = { kind: "device" };

    shim.attach_hub(0, 16);
    shim.attach_hub(1, 8);
    shim.detach_at_path([0, 5]);
    shim.attach_webhid_device([0, 6], dev);
    shim.attach_usb_hid_passthrough_device([1, 7], dev);

    expect(ehci.attach_hub).toHaveBeenNthCalledWith(1, 1, 16);
    expect(ehci.attach_hub).toHaveBeenNthCalledWith(2, 0, 8);
    expect(ehci.detach_at_path).toHaveBeenCalledWith([1, 5]);
    expect(ehci.attach_webhid_device).toHaveBeenCalledWith([1, 6], dev);
    expect(ehci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([0, 7], dev);
  });

  it("allows UhciHidTopologyManager to attach its external hub on EHCI root port 1", () => {
    const ehci = createFakeBridge();
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    mgr.setUhciBridge(createEhciTopologyBridgeShim(ehci));
    expect(ehci.attach_hub).toHaveBeenCalledWith(1, 16);
  });
});


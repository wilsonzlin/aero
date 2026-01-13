import { describe, expect, it, vi } from "vitest";

import { XhciHidTopologyManager, type XhciTopologyBridge } from "./xhci_hid_topology";

function createFakeXhci(): XhciTopologyBridge & {
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

describe("hid/xhci_hid_topology", () => {
  it("attaches hubs eagerly when explicit hub config is provided before the xHCI bridge is set", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 15 });
    const xhci = createFakeXhci();

    mgr.setHubConfig([0], 8);
    mgr.setXhciBridge(xhci);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(0, 8);
  });

  it("attaches hubs lazily as devices demand them", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev = { kind: "device" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [0, 1], "webhid", dev);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(0, 8);
    expect(xhci.detach_at_path).toHaveBeenCalledWith([0, 1]);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([0, 1], dev);
  });

  it("re-attaches downstream devices when a hub is resized", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev1 = { kind: "device-1" };
    const dev2 = { kind: "device-2" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [0, 1], "webhid", dev1);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(0, 8);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([0, 1], dev1);

    // Attach a device on a higher port; the manager should replace the hub and then
    // reattach the existing device behind it.
    mgr.attachDevice(2, [0, 12], "webhid", dev2);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(2);
    expect(xhci.attach_hub).toHaveBeenNthCalledWith(2, 0, 12);
    expect(xhci.detach_at_path).toHaveBeenCalledWith([0]);

    const dev1Calls = xhci.attach_webhid_device.mock.calls.filter(([, dev]) => dev === dev1);
    expect(dev1Calls).toHaveLength(2);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([0, 12], dev2);
  });

  it("detachDevice is idempotent", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev = { kind: "device" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [0, 1], "usb-hid-passthrough", dev);

    mgr.detachDevice(1);
    mgr.detachDevice(1);

    // One detach for clearing on attach, one for explicit detach; subsequent detaches are no-ops.
    expect(xhci.detach_at_path).toHaveBeenCalledTimes(2);
    expect(xhci.detach_at_path).toHaveBeenLastCalledWith([0, 1]);
    expect(xhci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([0, 1], dev);
  });
});


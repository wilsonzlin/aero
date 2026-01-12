import { describe, expect, it, vi } from "vitest";

import { UhciHidTopologyManager, type UhciTopologyBridge } from "./uhci_hid_topology";

function createFakeUhci(): UhciTopologyBridge & {
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

describe("hid/UhciHidTopologyManager", () => {
  it("attaches the default external hub at root port 0 when the UHCI bridge is set", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();

    mgr.setUhciBridge(uhci);

    expect(uhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 16);
  });

  it("remaps legacy root-port-only paths onto stable hub ports behind root port 0", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.setUhciBridge(uhci);
    mgr.attachDevice(1, [0], "webhid", dev);

    // Hub should remain on root port 0.
    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 16);

    expect(uhci.detach_at_path).toHaveBeenCalledTimes(1);
    expect(uhci.detach_at_path).toHaveBeenCalledWith([0, 4]);
    expect(uhci.attach_webhid_device).toHaveBeenCalledWith([0, 4], dev);

    mgr.detachDevice(1);
    // Detach should use the normalized path as well.
    expect(uhci.detach_at_path).toHaveBeenCalledTimes(2);
    expect(uhci.detach_at_path).toHaveBeenLastCalledWith([0, 4]);
  });

  it("remaps legacy root-port-only path [1] onto a stable external hub port to avoid clobbering WebUSB", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.setUhciBridge(uhci);
    mgr.attachDevice(1, [1], "webhid", dev);

    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 16);
    expect(uhci.detach_at_path).toHaveBeenCalledWith([0, 5]);
    expect(uhci.attach_webhid_device).toHaveBeenCalledWith([0, 5], dev);

    mgr.detachDevice(1);
    expect(uhci.detach_at_path).toHaveBeenLastCalledWith([0, 5]);
  });

  it("detaches the previous guest path when a deviceId is re-attached at a new path", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev1 = { kind: "device" };
    const dev2 = { kind: "device" };

    mgr.setUhciBridge(uhci);

    mgr.attachDevice(1, [0, 4], "webhid", dev1);
    expect(uhci.attach_webhid_device).toHaveBeenCalledWith([0, 4], dev1);

    mgr.attachDevice(1, [0, 5], "webhid", dev2);

    // Detach old path (0.1) first, then clear/attach the new path (0.2).
    expect(uhci.detach_at_path).toHaveBeenCalledWith([0, 4]);
    expect(uhci.attach_webhid_device).toHaveBeenCalledWith([0, 5], dev2);
  });

  it("defers device attachment until the UHCI bridge is available", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.attachDevice(1, [0, 6], "webhid", dev);
    expect(uhci.attach_hub).not.toHaveBeenCalled();

    mgr.setUhciBridge(uhci);

    expect(uhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 16);
    expect(uhci.detach_at_path).toHaveBeenCalledWith([0, 6]);
    expect(uhci.attach_webhid_device).toHaveBeenCalledWith([0, 6], dev);
  });

  it("uses explicit hub config when provided", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.setHubConfig([0], 8);
    mgr.attachDevice(1, [0, 5], "webhid", dev);
    mgr.setUhciBridge(uhci);

    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 8);
  });

  it("ensures hubs have enough ports for the requested guest path", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.attachDevice(1, [0, 20], "webhid", dev);
    mgr.setUhciBridge(uhci);

    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 20);
  });

  it("detaches guest paths when devices are removed", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.setUhciBridge(uhci);
    mgr.attachDevice(1, [1], "usb-hid-passthrough", dev);
    mgr.detachDevice(1);

    // One detach for clearing on attach, one for explicit detach.
    expect(uhci.detach_at_path).toHaveBeenCalledWith([0, 5]);
    expect(uhci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([0, 5], dev);
    expect(uhci.detach_at_path).toHaveBeenCalledTimes(2);
  });

  it("does not re-attach hubs once attached", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev = { kind: "device" };

    mgr.setUhciBridge(uhci);
    mgr.attachDevice(1, [0, 4], "webhid", dev);
    expect(uhci.attach_hub).toHaveBeenCalledTimes(1);

    // Updating the config after the hub has been attached should not replace it.
    mgr.setHubConfig([0], 8);
    expect(uhci.attach_hub).toHaveBeenCalledTimes(1);
  });

  it("re-attaches hubs when the UHCI bridge is replaced", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci1 = createFakeUhci();
    const uhci2 = createFakeUhci();
    const dev = { kind: "device" };

    mgr.attachDevice(1, [0, 4], "webhid", dev);
    mgr.setUhciBridge(uhci1);
    expect(uhci1.attach_hub).toHaveBeenCalledTimes(1);

    mgr.setUhciBridge(null);
    mgr.setUhciBridge(uhci2);
    expect(uhci2.attach_hub).toHaveBeenCalledTimes(1);
  });

  it("resizes hubs when a device needs a higher downstream port", () => {
    const mgr = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const uhci = createFakeUhci();
    const dev1 = { kind: "device-1" };
    const dev2 = { kind: "device-2" };

    mgr.setUhciBridge(uhci);
    mgr.attachDevice(1, [0, 4], "webhid", dev1);
    expect(uhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(uhci.attach_hub).toHaveBeenCalledWith(0, 16);

    mgr.attachDevice(2, [0, 20], "webhid", dev2);
    expect(uhci.attach_hub).toHaveBeenCalledTimes(2);
    expect(uhci.attach_hub).toHaveBeenNthCalledWith(2, 0, 20);
    expect(uhci.detach_at_path).toHaveBeenCalledWith([0]);

    const dev1Calls = uhci.attach_webhid_device.mock.calls.filter(([, dev]) => dev === dev1);
    expect(dev1Calls).toHaveLength(2);
    expect(uhci.attach_webhid_device).toHaveBeenCalledWith([0, 20], dev2);
  });
}); 

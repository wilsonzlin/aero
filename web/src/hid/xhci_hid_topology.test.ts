import { describe, expect, it, vi } from "vitest";

import { XhciHidTopologyManager, type XhciTopologyBridge } from "./xhci_hid_topology";
import {
  EXTERNAL_HUB_ROOT_PORT,
  WEBUSB_GUEST_ROOT_PORT,
  remapLegacyRootPortToExternalHubPort,
} from "../usb/uhci_external_hub";

function createFakeXhci(): XhciTopologyBridge & {
  attach_hub: ReturnType<typeof vi.fn>;
  detach_at_path: ReturnType<typeof vi.fn>;
  attach_webhid_device: ReturnType<typeof vi.fn>;
  attach_usb_hid_passthrough_device: ReturnType<typeof vi.fn>;
} {
  return {
    free: vi.fn(),
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

    mgr.setHubConfig([EXTERNAL_HUB_ROOT_PORT], 8);
    mgr.setXhciBridge(xhci);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(EXTERNAL_HUB_ROOT_PORT, 8);
  });

  it("ignores hub config for reserved WebUSB root port 1", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 15 });
    const xhci = createFakeXhci();

    mgr.setHubConfig([WEBUSB_GUEST_ROOT_PORT], 8);
    mgr.setXhciBridge(xhci);

    expect(xhci.attach_hub).not.toHaveBeenCalled();
  });

  it("attaches hubs lazily as devices demand them", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev = { kind: "device" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, 1], "webhid", dev);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(EXTERNAL_HUB_ROOT_PORT, 8);
    expect(xhci.detach_at_path).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 1]);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 1], dev);
  });

  it("re-attaches downstream devices when a hub is resized", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev1 = { kind: "device-1" };
    const dev2 = { kind: "device-2" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, 1], "webhid", dev1);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(EXTERNAL_HUB_ROOT_PORT, 8);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 1], dev1);

    // Attach a device on a higher port; the manager should replace the hub and then
    // reattach the existing device behind it.
    mgr.attachDevice(2, [EXTERNAL_HUB_ROOT_PORT, 12], "webhid", dev2);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(2);
    expect(xhci.attach_hub).toHaveBeenNthCalledWith(2, EXTERNAL_HUB_ROOT_PORT, 12);
    expect(xhci.detach_at_path).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT]);

    const dev1Calls = xhci.attach_webhid_device.mock.calls.filter(([, dev]) => dev === dev1);
    expect(dev1Calls).toHaveLength(2);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 12], dev2);
  });

  it("detachDevice is idempotent", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev = { kind: "device" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, 1], "usb-hid-passthrough", dev);

    mgr.detachDevice(1);
    mgr.detachDevice(1);

    // One detach for clearing on attach, one for explicit detach; subsequent detaches are no-ops.
    expect(xhci.detach_at_path).toHaveBeenCalledTimes(2);
    expect(xhci.detach_at_path).toHaveBeenLastCalledWith([EXTERNAL_HUB_ROOT_PORT, 1]);
    expect(xhci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 1], dev);
  });

  it("keeps existing devices attached when re-attaching with an invalid path", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev = { kind: "device" };
    const invalidPort = 20; // >15 is not representable in the xHCI Route String.

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, 1], "webhid", dev);
    expect(xhci.detach_at_path).toHaveBeenCalledTimes(1);
    expect(xhci.attach_webhid_device).toHaveBeenCalledTimes(1);

    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, invalidPort], "webhid", dev);

    // Invalid reattach attempts should not disconnect the previously attached device.
    expect(xhci.detach_at_path).toHaveBeenCalledTimes(1);
    expect(xhci.attach_webhid_device).toHaveBeenCalledTimes(1);

    mgr.detachDevice(1);
    expect(xhci.detach_at_path).toHaveBeenCalledTimes(2);
    expect(xhci.detach_at_path).toHaveBeenLastCalledWith([EXTERNAL_HUB_ROOT_PORT, 1]);
  });

  it("ignores device attachments with invalid downstream port numbers (>15)", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev1 = { kind: "device-1" };
    const dev2 = { kind: "device-2" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, 1], "webhid", dev1);
    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);

    mgr.attachDevice(2, [EXTERNAL_HUB_ROOT_PORT, 20], "webhid", dev2);

    // The invalid port should not cause the hub to be resized or the device to be attached.
    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_webhid_device).toHaveBeenCalledTimes(1);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 1], dev1);
  });

  it("ignores device attachments with too-deep paths (>5 downstream hub tiers)", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev1 = { kind: "device-1" };
    const dev2 = { kind: "device-2" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT, 1], "webhid", dev1);
    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);

    // Route String is 20 bits (5 nibbles), so only 5 downstream ports are representable.
    mgr.attachDevice(2, [EXTERNAL_HUB_ROOT_PORT, 1, 1, 1, 1, 1, 1], "webhid", dev2);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_webhid_device).toHaveBeenCalledTimes(1);
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith([EXTERNAL_HUB_ROOT_PORT, 1], dev1);
  });

  it("remaps legacy root-port-only paths ([0] and [1]) onto the external hub behind root port 0", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev0 = { kind: "device-root-0" };
    const dev1 = { kind: "device-root-1" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [EXTERNAL_HUB_ROOT_PORT], "webhid", dev0);
    mgr.attachDevice(2, [WEBUSB_GUEST_ROOT_PORT], "webhid", dev1);

    expect(xhci.attach_hub).toHaveBeenCalledTimes(1);
    expect(xhci.attach_hub).toHaveBeenCalledWith(EXTERNAL_HUB_ROOT_PORT, 8);

    expect(xhci.attach_webhid_device).toHaveBeenCalledWith(
      [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(EXTERNAL_HUB_ROOT_PORT)],
      dev0,
    );
    expect(xhci.attach_webhid_device).toHaveBeenCalledWith(
      [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(WEBUSB_GUEST_ROOT_PORT)],
      dev1,
    );
  });

  it("rejects attaching devices behind reserved WebUSB root port 1", () => {
    const mgr = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const xhci = createFakeXhci();
    const dev = { kind: "device" };

    mgr.setXhciBridge(xhci);
    mgr.attachDevice(1, [WEBUSB_GUEST_ROOT_PORT, 2], "webhid", dev);

    expect(xhci.attach_hub).not.toHaveBeenCalled();
    expect(xhci.detach_at_path).not.toHaveBeenCalled();
    expect(xhci.attach_webhid_device).not.toHaveBeenCalled();
    expect(xhci.attach_usb_hid_passthrough_device).not.toHaveBeenCalled();
  });
});

import { describe, expect, it, vi } from "vitest";

import type { UsbSelectedMessage } from "./usb_proxy_protocol";
import { applyUsbSelectedToWebUsbXhciBridge, type WebUsbXhciHotplugBridgeLike } from "./xhci_webusb_bridge";

describe("xhci_webusb_bridge", () => {
  it("connects on usb.selected ok:true", () => {
    const bridge: WebUsbXhciHotplugBridgeLike = {
      set_connected: vi.fn<[boolean], void>(),
      reset: vi.fn<[], void>(),
    };

    const msg: UsbSelectedMessage = { type: "usb.selected", ok: true, info: { vendorId: 0x1234, productId: 0x5678 } };
    applyUsbSelectedToWebUsbXhciBridge(bridge, msg);

    expect(bridge.set_connected).toHaveBeenCalledTimes(1);
    expect(bridge.set_connected).toHaveBeenCalledWith(true);
    expect(bridge.reset).not.toHaveBeenCalled();
  });

  it("disconnects and resets on usb.selected ok:false", () => {
    const bridge: WebUsbXhciHotplugBridgeLike = {
      set_connected: vi.fn<[boolean], void>(),
      reset: vi.fn<[], void>(),
    };

    const msg: UsbSelectedMessage = { type: "usb.selected", ok: false, error: "no device" };
    applyUsbSelectedToWebUsbXhciBridge(bridge, msg);

    expect(bridge.set_connected).toHaveBeenCalledTimes(1);
    expect(bridge.set_connected).toHaveBeenCalledWith(false);
    expect(bridge.reset).toHaveBeenCalledTimes(1);

    const connectOrder = (bridge.set_connected as unknown as { mock: { invocationCallOrder: number[] } }).mock.invocationCallOrder[0];
    const resetOrder = (bridge.reset as unknown as { mock: { invocationCallOrder: number[] } }).mock.invocationCallOrder[0];
    expect(connectOrder).toBeLessThan(resetOrder);
  });

  it("accepts camelCase setConnected() (backwards compatibility)", () => {
    const bridge = {
      setConnected: vi.fn<[boolean], void>(),
      reset: vi.fn<[], void>(),
    };

    const msg: UsbSelectedMessage = { type: "usb.selected", ok: false, error: "no device" };
    applyUsbSelectedToWebUsbXhciBridge(bridge as unknown as WebUsbXhciHotplugBridgeLike, msg);

    expect(bridge.setConnected).toHaveBeenCalledTimes(1);
    expect(bridge.setConnected).toHaveBeenCalledWith(false);
    expect(bridge.reset).toHaveBeenCalledTimes(1);

    const connectOrder = (bridge.setConnected as unknown as { mock: { invocationCallOrder: number[] } }).mock.invocationCallOrder[0];
    const resetOrder = (bridge.reset as unknown as { mock: { invocationCallOrder: number[] } }).mock.invocationCallOrder[0];
    expect(connectOrder).toBeLessThan(resetOrder);
  });
});

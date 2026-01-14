import { describe, expect, it, vi } from "vitest";

import { applyUsbSelectedToWebUsbGuestBridge, chooseWebUsbGuestBridge } from "./io_webusb_guest_selection";

describe("webusb_guest_selection xhci_webusb guest selection (io worker)", () => {
  it("prefers xHCI over EHCI over UHCI when multiple passthrough bridges are present", () => {
    const uhci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => []),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };
    const ehci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => []),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };
    const xhci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => []),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    // In the real IO worker these instances come from WASM exports. For this unit test, plain JS
    // objects are sufficient as long as they satisfy the `WebUsbGuestBridgeLike` shape.
    const uhciBridge = uhci;
    const ehciBridge = ehci;
    const xhciBridge = xhci;

    const picked = chooseWebUsbGuestBridge({ xhciBridge, ehciBridge, uhciBridge });
    expect(picked?.kind).toBe("xhci");

    // Apply usb.selected to the chosen bridge; the worker should call set_connected(true).
    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(xhci.set_connected).toHaveBeenCalledWith(true);
    expect(ehci.set_connected).not.toHaveBeenCalled();
    expect(uhci.set_connected).not.toHaveBeenCalled();
    expect(xhci.reset).not.toHaveBeenCalled();

    xhci.set_connected.mockClear();
    xhci.reset.mockClear();

    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, { type: "usb.selected", ok: false, error: "no device" });
    expect(xhci.set_connected).toHaveBeenCalledWith(false);
    expect(xhci.reset).toHaveBeenCalledTimes(1);
  });

  it("selects EHCI when xHCI is unavailable but EHCI is present", () => {
    const uhci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => []),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };
    const ehci = {
      set_connected: vi.fn(),
      drain_actions: vi.fn(() => []),
      push_completion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const picked = chooseWebUsbGuestBridge({ xhciBridge: null, ehciBridge: ehci, uhciBridge: uhci });
    expect(picked?.kind).toBe("ehci");

    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(ehci.set_connected).toHaveBeenCalledWith(true);
    expect(uhci.set_connected).not.toHaveBeenCalled();
    expect(ehci.reset).not.toHaveBeenCalled();

    ehci.set_connected.mockClear();
    ehci.reset.mockClear();

    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, { type: "usb.selected", ok: false, error: "no device" });
    expect(ehci.set_connected).toHaveBeenCalledWith(false);
    expect(ehci.reset).toHaveBeenCalledTimes(1);
  });

  it("accepts camelCase WebUSB passthrough bridge methods (backwards compatibility)", () => {
    const xhci = {
      setConnected: vi.fn(),
      drainActions: vi.fn(() => []),
      pushCompletion: vi.fn(),
      reset: vi.fn(),
      free: vi.fn(),
    };

    const picked = chooseWebUsbGuestBridge({ xhciBridge: xhci, ehciBridge: null, uhciBridge: null });
    expect(picked?.kind).toBe("xhci");

    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(xhci.setConnected).toHaveBeenCalledWith(true);

    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, { type: "usb.selected", ok: false, error: "no device" });
    expect(xhci.setConnected).toHaveBeenCalledWith(false);
    expect(xhci.reset).toHaveBeenCalledTimes(1);
  });
});

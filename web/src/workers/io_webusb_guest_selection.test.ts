import { describe, expect, it, vi } from "vitest";

import type { WasmApi } from "../runtime/wasm_loader";
import { applyUsbSelectedToWebUsbGuestBridge, chooseWebUsbGuestBridge } from "./io_webusb_guest_selection";

describe("webusb xhci selection (io worker)", () => {
  it("prefers xHCI when both UHCI and xHCI passthrough bridges are present", () => {
    const uhci = {
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

    // Mock a WasmApi that exposes both controllers.
    const api = {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      UhciControllerBridge: function FakeUhciBridge(): any {
        return uhci;
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      XhciControllerBridge: function FakeXhciBridge(): any {
        return xhci;
      },
    } as unknown as WasmApi;
    void api;

    // In the real IO worker these instances come from the WASM bridges; for this unit test we
    // construct them via the mocked API to ensure both exist at the same time.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const uhciBridge = new (api.UhciControllerBridge as any)(0, 0);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const xhciBridge = new (api.XhciControllerBridge as any)(0, 0);

    const picked = chooseWebUsbGuestBridge({ xhciBridge, uhciBridge });
    expect(picked?.kind).toBe("xhci");
    expect(picked?.bridge).toBe(xhciBridge);

    // Apply usb.selected to the chosen bridge; the worker should call set_connected(true).
    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(xhci.set_connected).toHaveBeenCalledWith(true);
    expect(uhci.set_connected).not.toHaveBeenCalled();
    expect(xhci.reset).not.toHaveBeenCalled();

    xhci.set_connected.mockClear();
    xhci.reset.mockClear();

    applyUsbSelectedToWebUsbGuestBridge(picked!.kind, picked!.bridge, { type: "usb.selected", ok: false, error: "no device" });
    expect(xhci.set_connected).toHaveBeenCalledWith(false);
    expect(xhci.reset).toHaveBeenCalledTimes(1);
  });
});


import { describe, expect, it, vi } from "vitest";
import { UhciWebUsbPciDevice } from "./uhci_webusb";
import { applyUsbSelectedToWebUsbUhciBridge } from "../../usb/uhci_webusb_bridge";

describe("UhciWebUsbPciDevice", () => {
  it("forwards BAR4 I/O reads/writes to the WASM bridge with the same offset/size", () => {
    const io_read = vi.fn(() => 0x1234_5678);
    const io_write = vi.fn();

    const dev = new UhciWebUsbPciDevice({ io_read, io_write });

    const v = dev.ioRead?.(4, 0x10, 2);
    expect(v).toBe(0x1234_5678);
    expect(io_read).toHaveBeenCalledWith(0x10, 2);

    dev.ioWrite?.(4, 0x08, 4, 0xdead_beef);
    expect(io_write).toHaveBeenCalledWith(0x08, 4, 0xdead_beef);
  });
});

describe("applyUsbSelectedToWebUsbUhciBridge", () => {
  it("connects on ok:true and disconnects+resets on ok:false", () => {
    const bridge = {
      set_connected: vi.fn(),
      reset: vi.fn(),
    };

    applyUsbSelectedToWebUsbUhciBridge(bridge, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(bridge.set_connected).toHaveBeenCalledWith(true);
    expect(bridge.reset).not.toHaveBeenCalled();

    bridge.set_connected.mockClear();
    bridge.reset.mockClear();

    applyUsbSelectedToWebUsbUhciBridge(bridge, { type: "usb.selected", ok: false, error: "no device" });
    expect(bridge.set_connected).toHaveBeenCalledWith(false);
    expect(bridge.reset).toHaveBeenCalled();
  });
});

import { describe, expect, it, vi } from "vitest";

import { createXhciTopologyBridgeShim } from "./io_hid_topology_mux";

describe("workers/io_hid_topology_mux", () => {
  it("accepts camelCase XHCI topology helper exports and preserves `this` binding", () => {
    const thisContexts = {
      attachHub: [] as unknown[],
      detachAtPath: [] as unknown[],
      attachWebHidDevice: [] as unknown[],
      attachUsbHidPassthroughDevice: [] as unknown[],
      free: [] as unknown[],
    };

    const bridge = {
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
      free: vi.fn(function (this: unknown) {
        thisContexts.free.push(this);
      }),
    };

    const shim = createXhciTopologyBridgeShim(bridge);
    expect(shim).not.toBeNull();

    const dev = { kind: "device" };
    shim!.attach_hub?.(0, 8);
    shim!.detach_at_path?.([0, 1]);
    shim!.attach_webhid_device?.([0, 2], dev);
    shim!.attach_usb_hid_passthrough_device?.([0, 3], dev);
    shim!.free();

    expect(bridge.attachHub).toHaveBeenCalledWith(0, 8);
    expect(bridge.detachAtPath).toHaveBeenCalledWith([0, 1]);
    expect(bridge.attachWebHidDevice).toHaveBeenCalledWith([0, 2], dev);
    expect(bridge.attachUsbHidPassthroughDevice).toHaveBeenCalledWith([0, 3], dev);
    expect(bridge.free).toHaveBeenCalledTimes(1);

    expect(thisContexts.attachHub[0]).toBe(bridge);
    expect(thisContexts.detachAtPath[0]).toBe(bridge);
    expect(thisContexts.attachWebHidDevice[0]).toBe(bridge);
    expect(thisContexts.attachUsbHidPassthroughDevice[0]).toBe(bridge);
    expect(thisContexts.free[0]).toBe(bridge);
  });
});

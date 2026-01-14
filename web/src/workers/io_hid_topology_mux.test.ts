import { describe, expect, it, vi } from "vitest";

import type { HidAttachMessage, HidDetachMessage } from "../hid/hid_proxy_protocol";
import { WasmHidGuestBridge } from "../hid/wasm_hid_guest_bridge";
import { UhciHidTopologyManager } from "../hid/uhci_hid_topology";
import { XhciHidTopologyManager } from "../hid/xhci_hid_topology";
import type { WasmApi } from "../runtime/wasm_context";

import { createXhciTopologyBridgeShim, IoWorkerHidTopologyMux } from "./io_hid_topology_mux";

describe("workers/io_hid_topology_mux (xhci_hid_topology)", () => {
  it("accepts xHCI topology exports even when free() is missing (shim provides a no-op free)", () => {
    const bridge = {
      attach_hub: vi.fn(),
      detach_at_path: vi.fn(),
      attach_webhid_device: vi.fn(),
      attach_usb_hid_passthrough_device: vi.fn(),
    };

    const shim = createXhciTopologyBridgeShim(bridge);
    expect(shim).not.toBeNull();
    expect(() => shim!.free()).not.toThrow();

    shim!.attach_hub?.(0, 4);
    expect(bridge.attach_hub).toHaveBeenCalledWith(0, 4);
  });

  it("rejects xHCI topology exports when free is present but not a function", () => {
    const bridge = {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      free: 123 as any,
      attach_hub: vi.fn(),
      detach_at_path: vi.fn(),
      attach_webhid_device: vi.fn(),
      attach_usb_hid_passthrough_device: vi.fn(),
    };
    expect(createXhciTopologyBridgeShim(bridge)).toBeNull();
  });

  it("routes hid.attach/hid.detach to xHCI when the bridge exposes topology APIs", () => {
    const xhci = {
      free: vi.fn(),
      attach_hub: vi.fn(),
      detach_at_path: vi.fn(),
      attach_webhid_device: vi.fn(),
      attach_usb_hid_passthrough_device: vi.fn(),
    };

    const uhci = {
      free: vi.fn(),
      attach_hub: vi.fn(),
      detach_at_path: vi.fn(),
      attach_webhid_device: vi.fn(),
      attach_usb_hid_passthrough_device: vi.fn(),
    };

    const xhciTopology = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const uhciTopology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    uhciTopology.setUhciBridge(uhci);
    // `setUhciBridge` attaches the external hub eagerly; ignore that side effect for routing assertions.
    uhci.attach_hub.mockClear();

    const xhciShim = createXhciTopologyBridgeShim(xhci);
    expect(xhciShim).not.toBeNull();
    xhciTopology.setXhciBridge(xhciShim);

    const topologyMux = new IoWorkerHidTopologyMux({
      xhci: xhciTopology,
      uhci: uhciTopology,
      useXhci: () => xhciShim !== null,
    });

    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn(() => null);
      readonly configured = vi.fn(() => false);
      readonly free = vi.fn();

      constructor() {
        bridgeInstance = this;
      }
    }

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesize_webhid_report_descriptor: synthesizeSpy,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any as WasmApi;

    const host = { sendReport: vi.fn(), requestFeatureReport: vi.fn(), log: vi.fn(), error: vi.fn() };
    const guest = new WasmHidGuestBridge(api, host, topologyMux);

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 42,
      vendorId: 0x1234,
      productId: 0xabcd,
      guestPath: [0, 1],
      collections: [],
      hasInterruptOut: false,
    };

    guest.attach(attach);

    expect(bridgeInstance).not.toBeNull();
    expect(xhci.attach_hub).toHaveBeenCalledWith(0, 8);
    expect(xhci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([0, 1], bridgeInstance);

    // Ensure UHCI was not used for the attachment.
    expect(uhci.attach_usb_hid_passthrough_device).not.toHaveBeenCalled();

    const detach: HidDetachMessage = { type: "hid.detach", deviceId: attach.deviceId };
    guest.detach(detach);
    guest.detach(detach);

    // One detach during attach (clearing the path), one detach for the explicit detach; subsequent detaches are no-ops.
    expect(xhci.detach_at_path).toHaveBeenCalledTimes(2);
    expect(xhci.detach_at_path).toHaveBeenLastCalledWith([0, 1]);
  });

  it("falls back to UHCI when xHCI topology exports are unavailable", () => {
    const xhciMissingExports = {
      free: vi.fn(),
      attach_hub: vi.fn(),
      detach_at_path: vi.fn(),
      attach_webhid_device: vi.fn(),
      // Missing `attach_usb_hid_passthrough_device`.
    };

    const xhciShim = createXhciTopologyBridgeShim(xhciMissingExports);
    expect(xhciShim).toBeNull();

    const uhci = {
      free: vi.fn(),
      attach_hub: vi.fn(),
      detach_at_path: vi.fn(),
      attach_webhid_device: vi.fn(),
      attach_usb_hid_passthrough_device: vi.fn(),
    };

    const xhciTopology = new XhciHidTopologyManager({ defaultHubPortCount: 8 });
    const uhciTopology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    uhciTopology.setUhciBridge(uhci);
    // `setUhciBridge` attaches the external hub eagerly; ignore that side effect for routing assertions.
    uhci.attach_hub.mockClear();

    const topologyMux = new IoWorkerHidTopologyMux({
      xhci: xhciTopology,
      uhci: uhciTopology,
      useXhci: () => xhciShim !== null,
    });

    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn(() => null);
      readonly configured = vi.fn(() => false);
      readonly free = vi.fn();

      constructor() {
        bridgeInstance = this;
      }
    }

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesize_webhid_report_descriptor: synthesizeSpy,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any as WasmApi;

    const host = { sendReport: vi.fn(), requestFeatureReport: vi.fn(), log: vi.fn(), error: vi.fn() };
    const guest = new WasmHidGuestBridge(api, host, topologyMux);

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 42,
      vendorId: 0x1234,
      productId: 0xabcd,
      guestPath: [0, 1],
      collections: [],
      hasInterruptOut: false,
    };

    guest.attach(attach);

    expect(bridgeInstance).not.toBeNull();
    expect(uhci.attach_usb_hid_passthrough_device).toHaveBeenCalledWith([0, 1], bridgeInstance);
    expect(xhciMissingExports.attach_hub).not.toHaveBeenCalled();
    expect(xhciMissingExports.detach_at_path).not.toHaveBeenCalled();

    const detach: HidDetachMessage = { type: "hid.detach", deviceId: attach.deviceId };
    guest.detach(detach);
    guest.detach(detach);

    // One detach during attach (clearing the path), one detach for the explicit detach; subsequent detaches are no-ops.
    expect(uhci.detach_at_path).toHaveBeenCalledTimes(2);
    expect(uhci.detach_at_path).toHaveBeenLastCalledWith([0, 1]);
  });
});

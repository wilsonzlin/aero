import { describe, expect, it, vi } from "vitest";

import { WasmHidGuestBridge } from "./wasm_hid_guest_bridge";
import { UhciHidTopologyManager } from "./uhci_hid_topology";
import type { HidAttachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import type { WasmApi } from "../runtime/wasm_context";

describe("hid/WasmHidGuestBridge", () => {
  it("attaches usb-hid-passthrough devices and forwards input/output reports", () => {
    const ctorSpy = vi.fn();
    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
        bridgeInstance = this;
      }
    }

    const host = {
      sendReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const attachSpy = vi.spyOn(topology, "attachDevice");
    const detachSpy = vi.spyOn(topology, "detachDevice");

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesize_webhid_report_descriptor: synthesizeSpy,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [0],
      collections: [{ some: "collection" }] as any,
      hasInterruptOut: true,
    };
    guest.attach(attach);

    expect(synthesizeSpy).toHaveBeenCalledWith(attach.collections);
    expect(ctorSpy).toHaveBeenCalledTimes(1);
    expect(ctorSpy).toHaveBeenCalledWith(
      attach.vendorId,
      attach.productId,
      undefined,
      attach.productName,
      undefined,
      synthesizeSpy.mock.results[0]!.value,
      attach.hasInterruptOut,
      undefined,
      undefined,
    );

    // Attach remaps `[0]` to `[1]` because root port 0 is reserved for the external hub.
    expect(detachSpy).toHaveBeenCalledWith(attach.deviceId);
    expect(attachSpy).toHaveBeenCalledWith(attach.deviceId, [1], "usb-hid-passthrough", bridgeInstance);

    const input: HidInputReportMessage = {
      type: "hid.inputReport",
      deviceId: attach.deviceId,
      reportId: 7,
      data: new Uint8Array([9, 10]) as Uint8Array<ArrayBuffer>,
    };
    guest.inputReport(input);
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledWith(input.reportId, input.data);

    const outData = new Uint8Array([0xaa, 0xbb]);
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce({ reportType: "output", reportId: 1, data: outData });
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce(null);

    guest.poll?.();
    expect(host.sendReport).toHaveBeenCalledWith({
      deviceId: attach.deviceId,
      reportType: "output",
      reportId: 1,
      data: outData,
    });
  });

  it("falls back to WebHidPassthroughBridge when UsbHidPassthroughBridge is unavailable", () => {
    const ctorSpy = vi.fn();
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
        bridgeInstance = this;
      }
    }

    const host = {
      sendReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const attachSpy = vi.spyOn(topology, "attachDevice");

    const api = {
      WebHidPassthroughBridge: FakeBridge,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 2,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [1],
      collections: [{ some: "collection" }] as any,
      hasInterruptOut: false,
    };
    guest.attach(attach);

    expect(ctorSpy).toHaveBeenCalledTimes(1);
    expect(ctorSpy).toHaveBeenCalledWith(
      attach.vendorId,
      attach.productId,
      undefined,
      attach.productName,
      undefined,
      attach.collections,
    );
    expect(attachSpy).toHaveBeenCalledWith(attach.deviceId, attach.guestPath, "webhid", bridgeInstance);
  });
});


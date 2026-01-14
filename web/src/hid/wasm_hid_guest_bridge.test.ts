import { describe, expect, it, vi } from "vitest";

import { WasmHidGuestBridge } from "./wasm_hid_guest_bridge";
import { UhciHidTopologyManager } from "./uhci_hid_topology";
import type { HidAttachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import type { NormalizedHidCollectionInfo } from "./webhid_normalize";
import type { WasmApi } from "../runtime/wasm_context";

function makeTopLevelApplicationCollection(usagePage: number, usage: number): NormalizedHidCollectionInfo {
  return {
    usagePage,
    usage,
    collectionType: 1, // Application
    children: [],
    inputReports: [],
    outputReports: [],
    featureReports: [],
  };
}

describe("hid/WasmHidGuestBridge", () => {
  it("attaches usb-hid-passthrough devices and forwards input/output reports", () => {
    const ctorSpy = vi.fn();
    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly drain_next_feature_report_request = vi.fn();
      readonly complete_feature_report_request = vi.fn(() => true);
      readonly fail_feature_report_request = vi.fn(() => true);
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
        bridgeInstance = this;
      }
    }

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const attachSpy = vi.spyOn(topology, "attachDevice");
    const detachSpy = vi.spyOn(topology, "detachDevice");

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesize_webhid_report_descriptor: synthesizeSpy,
    } as unknown as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [0],
      // Generic Desktop / Joystick (non-boot). Ensure the guest bridge does not force the HID
      // boot subclass/protocol.
      collections: [makeTopLevelApplicationCollection(0x01, 0x04)],
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

    // Legacy root-port-only paths are normalized by `UhciHidTopologyManager`, so the guest bridge
    // should forward the path hint as-is.
    expect(detachSpy).toHaveBeenCalledWith(attach.deviceId);
    expect(attachSpy).toHaveBeenCalledWith(attach.deviceId, attach.guestPath, "usb-hid-passthrough", bridgeInstance);

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

    bridgeInstance!.drain_next_feature_report_request.mockReturnValueOnce({ requestId: 99, reportId: 3 });
    bridgeInstance!.drain_next_feature_report_request.mockReturnValueOnce(null);

    guest.poll?.();
    expect(host.sendReport).toHaveBeenCalledWith({
      deviceId: attach.deviceId,
      reportType: "output",
      reportId: 1,
      data: outData,
    });

    expect(host.requestFeatureReport).toHaveBeenCalledWith({ deviceId: attach.deviceId, requestId: 99, reportId: 3 });

    const featureData = new Uint8Array([1, 2, 3]);
    expect(guest.completeFeatureReportRequest?.({ deviceId: attach.deviceId, requestId: 99, reportId: 3, data: featureData })).toBe(true);
    expect(bridgeInstance!.complete_feature_report_request).toHaveBeenCalledWith(99, 3, featureData);
  });

  it("accepts camelCase passthrough bridge exports (backwards compatibility)", () => {
    const ctorSpy = vi.fn();
    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly pushInputReport = vi.fn();
      readonly drainNextOutputReport = vi.fn();
      readonly drainNextFeatureReportRequest = vi.fn();
      readonly completeFeatureReportRequest = vi.fn(() => true);
      readonly failFeatureReportRequest = vi.fn(() => true);
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
        bridgeInstance = this;
      }
    }

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesizeWebhidReportDescriptor: synthesizeSpy,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);
    guest.attach({
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [0],
      collections: [makeTopLevelApplicationCollection(0x01, 0x04)],
      hasInterruptOut: true,
    });

    expect(bridgeInstance).toBeTruthy();
    expect(synthesizeSpy).toHaveBeenCalledTimes(1);
    expect(ctorSpy).toHaveBeenCalledTimes(1);

    const inputData = new Uint8Array([9, 10]) as Uint8Array<ArrayBuffer>;
    guest.inputReport({
      type: "hid.inputReport",
      deviceId: 1,
      reportId: 7,
      data: inputData,
    });
    expect(bridgeInstance!.pushInputReport).toHaveBeenCalledWith(7, inputData);

    const outData = new Uint8Array([0xaa, 0xbb]);
    bridgeInstance!.drainNextOutputReport.mockReturnValueOnce({ reportType: "output", reportId: 1, data: outData });
    bridgeInstance!.drainNextOutputReport.mockReturnValueOnce(null);
    bridgeInstance!.drainNextFeatureReportRequest.mockReturnValueOnce({ requestId: 99, reportId: 3 });
    bridgeInstance!.drainNextFeatureReportRequest.mockReturnValueOnce(null);

    guest.poll?.();
    expect(host.sendReport).toHaveBeenCalledWith({ deviceId: 1, reportType: "output", reportId: 1, data: outData });
    expect(host.requestFeatureReport).toHaveBeenCalledWith({ deviceId: 1, requestId: 99, reportId: 3 });

    const featureData = new Uint8Array([1, 2, 3]);
    expect(guest.completeFeatureReportRequest?.({ deviceId: 1, requestId: 99, reportId: 3, data: featureData })).toBe(true);
    expect(bridgeInstance!.completeFeatureReportRequest).toHaveBeenCalledWith(99, 3, featureData);
  });

  it("infers HID boot keyboard subclass/protocol for keyboard collections", () => {
    const ctorSpy = vi.fn();
    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
      }
    }

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesize_webhid_report_descriptor: synthesizeSpy,
    } as unknown as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);
    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 3,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Keyboard",
      guestPath: [0],
      collections: [makeTopLevelApplicationCollection(0x01, 0x06)], // Generic Desktop / Keyboard
      hasInterruptOut: true,
    };
    guest.attach(attach);

    expect(ctorSpy).toHaveBeenCalledWith(
      attach.vendorId,
      attach.productId,
      undefined,
      attach.productName,
      undefined,
      synthesizeSpy.mock.results[0]!.value,
      attach.hasInterruptOut,
      1,
      1,
    );
  });

  it("infers HID boot mouse subclass/protocol for mouse collections", () => {
    const ctorSpy = vi.fn();
    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
      }
    }

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });

    const api = {
      UsbHidPassthroughBridge: FakeBridge,
      synthesize_webhid_report_descriptor: synthesizeSpy,
    } as unknown as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);
    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 4,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Mouse",
      guestPath: [0],
      collections: [makeTopLevelApplicationCollection(0x01, 0x02)], // Generic Desktop / Mouse
      hasInterruptOut: false,
    };
    guest.attach(attach);

    expect(ctorSpy).toHaveBeenCalledWith(
      attach.vendorId,
      attach.productId,
      undefined,
      attach.productName,
      undefined,
      synthesizeSpy.mock.results[0]!.value,
      attach.hasInterruptOut,
      1,
      2,
    );
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
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const attachSpy = vi.spyOn(topology, "attachDevice");

    const api = {
      WebHidPassthroughBridge: FakeBridge,
    } as unknown as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 2,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [0, 2],
      collections: [makeTopLevelApplicationCollection(0x01, 0x04)],
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

  it("clamps oversized input reports before forwarding to the WASM bridge", () => {
    let bridgeInstance: FakeBridge | null = null;
    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();
      constructor() {
        bridgeInstance = this;
      }
    }

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const topology = new UhciHidTopologyManager({ defaultHubPortCount: 16 });
    const api = {
      WebHidPassthroughBridge: FakeBridge,
    } as unknown as WasmApi;

    const guest = new WasmHidGuestBridge(api, host, topology);
    guest.attach({
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      collections: [makeTopLevelApplicationCollection(0x01, 0x04)],
      hasInterruptOut: false,
    });

    const huge = new Uint8Array(1024 * 1024);
    huge.set([1, 2, 3], 0);
    guest.inputReport({
      type: "hid.inputReport",
      deviceId: 1,
      reportId: 7,
      data: huge as Uint8Array<ArrayBuffer>,
    });

    expect(bridgeInstance).toBeTruthy();
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledTimes(1);
    const arg = bridgeInstance!.push_input_report.mock.calls[0]![1] as Uint8Array;
    expect(arg.byteLength).toBe(64);
    expect(Array.from(arg.slice(0, 3))).toEqual([1, 2, 3]);
  });
});

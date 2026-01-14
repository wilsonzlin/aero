import { describe, expect, it, vi } from "vitest";

import { WasmUhciHidGuestBridge, type UhciRuntimeHidApi } from "./wasm_uhci_hid_guest_bridge";
import type { HidAttachMessage } from "./hid_proxy_protocol";
import {
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT,
  WEBUSB_GUEST_ROOT_PORT,
  remapLegacyRootPortToExternalHubPort,
} from "../usb/uhci_external_hub";

describe("hid/WasmUhciHidGuestBridge", () => {
  it("uses webhid_attach_at_path when guestPath includes a downstream hub port", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_attach_at_path = vi.fn();
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_attach_at_path,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });

    const guestPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT] as const;
    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [...guestPath],
      collections: [],
      hasInterruptOut: false,
    };
    guest.attach(attach);

    expect(webhid_attach_at_path).toHaveBeenCalledWith(
      attach.deviceId,
      attach.vendorId,
      attach.productId,
      attach.productName,
      attach.collections,
      [...guestPath],
    );
    expect(webhid_attach).not.toHaveBeenCalled();
  });

  it("maps legacy single-part guestPath onto the external hub topology when available", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_attach_at_path = vi.fn();
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_attach_at_path,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 2,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Legacy",
      guestPath: [WEBUSB_GUEST_ROOT_PORT],
      collections: [],
      hasInterruptOut: false,
    };
    guest.attach(attach);

    const expectedPath = [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(WEBUSB_GUEST_ROOT_PORT)] as const;
    expect(webhid_attach_at_path).toHaveBeenCalledWith(
      attach.deviceId,
      attach.vendorId,
      attach.productId,
      attach.productName,
      attach.collections,
      [...expectedPath],
    );
    expect(webhid_attach).not.toHaveBeenCalled();
  });

  it("maps legacy guestPort onto the external hub topology when available", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_attach_at_path = vi.fn();
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_attach_at_path,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 3,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Legacy",
      guestPort: WEBUSB_GUEST_ROOT_PORT,
      collections: [],
      hasInterruptOut: false,
    };
    guest.attach(attach);

    const expectedPath = [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(WEBUSB_GUEST_ROOT_PORT)] as const;
    expect(webhid_attach_at_path).toHaveBeenCalledWith(
      attach.deviceId,
      attach.vendorId,
      attach.productId,
      attach.productName,
      attach.collections,
      [...expectedPath],
    );
    expect(webhid_attach).not.toHaveBeenCalled();
  });

  it("clamps oversized input reports before forwarding to the UHCI runtime API", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);
    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });
    const huge = new Uint8Array(1024 * 1024);
    huge.set([1, 2, 3], 0);
    guest.inputReport({ type: "hid.inputReport", deviceId: 1, reportId: 2, data: huge as Uint8Array<ArrayBuffer> });

    expect(webhid_push_input_report).toHaveBeenCalledTimes(1);
    const arg = webhid_push_input_report.mock.calls[0]![2] as Uint8Array;
    expect(arg.byteLength).toBe(64);
    expect(Array.from(arg.slice(0, 3))).toEqual([1, 2, 3]);
  });

  it("forwards feature report requests and completes them via the UHCI runtime API", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);
    const webhid_drain_feature_report_requests = vi.fn(() => [{ deviceId: 1, requestId: 7, reportId: 3 }]);
    const webhid_complete_feature_report_request = vi.fn();
    const webhid_fail_feature_report_request = vi.fn();

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
      webhid_drain_feature_report_requests,
      webhid_complete_feature_report_request,
      webhid_fail_feature_report_request,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });
    guest.poll();

    expect(webhid_drain_feature_report_requests).toHaveBeenCalledTimes(1);
    expect(host.requestFeatureReport).toHaveBeenCalledWith({ deviceId: 1, requestId: 7, reportId: 3 });

    const data = new Uint8Array([1, 2, 3]);
    expect(guest.completeFeatureReportRequest?.({ deviceId: 1, requestId: 7, reportId: 3, data })).toBe(true);
    expect(webhid_complete_feature_report_request).toHaveBeenCalledWith(1, 7, 3, data);

    expect(guest.failFeatureReportRequest?.({ deviceId: 1, requestId: 7, reportId: 3, error: "nope" })).toBe(true);
    expect(webhid_fail_feature_report_request).toHaveBeenCalledWith(1, 7, 3);
  });

  it("supports the legacy UHCI feature-report completion ABI (complete(data) + fail())", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);
    const webhid_drain_feature_report_requests = vi.fn(() => [{ deviceId: 1, requestId: 7, reportId: 3 }]);
    const webhid_complete_feature_report_request = vi.fn();
    const webhid_fail_feature_report_request = vi.fn();

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
      webhid_drain_feature_report_requests,
      webhid_complete_feature_report_request,
      webhid_fail_feature_report_request,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });
    guest.poll();

    expect(webhid_drain_feature_report_requests).toHaveBeenCalledTimes(1);
    expect(host.requestFeatureReport).toHaveBeenCalledWith({ deviceId: 1, requestId: 7, reportId: 3 });

    const data = new Uint8Array([1, 2, 3]);
    expect(guest.completeFeatureReportRequest?.({ deviceId: 1, requestId: 7, reportId: 3, data })).toBe(true);
    expect(webhid_complete_feature_report_request).toHaveBeenCalledWith(1, 7, 3, data);

    expect(guest.failFeatureReportRequest?.({ deviceId: 1, requestId: 7, reportId: 3, error: "nope" })).toBe(true);
    expect(webhid_fail_feature_report_request).toHaveBeenCalledWith(1, 7, 3);
  });

  it("caps feature report forwarding per tick", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);
    const queued = Array.from({ length: 32 }, (_, idx) => ({ deviceId: 1, requestId: idx + 1, reportId: 3 }));
    const webhid_drain_feature_report_requests = vi
      .fn()
      .mockReturnValueOnce(queued)
      .mockReturnValue([]);
    const webhid_complete_feature_report_request = vi.fn(() => true);

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
      webhid_drain_feature_report_requests,
      webhid_complete_feature_report_request,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });

    guest.poll();
    expect(host.requestFeatureReport).toHaveBeenCalledTimes(16);
    expect(host.requestFeatureReport).toHaveBeenNthCalledWith(1, { deviceId: 1, requestId: 1, reportId: 3 });
    expect(host.requestFeatureReport).toHaveBeenNthCalledWith(16, { deviceId: 1, requestId: 16, reportId: 3 });

    guest.poll();
    expect(host.requestFeatureReport).toHaveBeenCalledTimes(32);
    expect(host.requestFeatureReport).toHaveBeenNthCalledWith(17, { deviceId: 1, requestId: 17, reportId: 3 });
    expect(host.requestFeatureReport).toHaveBeenNthCalledWith(32, { deviceId: 1, requestId: 32, reportId: 3 });
  });

  it("supports legacy feature report completion APIs", () => {
    const webhid_attach = vi.fn(() => 0);
    const webhid_detach = vi.fn();
    const webhid_push_input_report = vi.fn();
    const webhid_drain_output_reports = vi.fn(() => []);
    const webhid_drain_feature_report_requests = vi.fn(() => [{ deviceId: 1, requestId: 7, reportId: 3 }]);

    const completeCalls: Array<[number, number, number, Uint8Array]> = [];
    function webhid_complete_feature_report_request(deviceId: number, requestId: number, reportId: number, data: Uint8Array): void {
      completeCalls.push([deviceId, requestId, reportId, data]);
    }
    const failCalls: Array<[number, number, number]> = [];
    function webhid_fail_feature_report_request(deviceId: number, requestId: number, reportId: number): void {
      failCalls.push([deviceId, requestId, reportId]);
    }

    const uhci: UhciRuntimeHidApi = {
      webhid_attach,
      webhid_detach,
      webhid_push_input_report,
      webhid_drain_output_reports,
      webhid_drain_feature_report_requests,
      webhid_complete_feature_report_request,
      webhid_fail_feature_report_request,
    };

    const host = {
      sendReport: vi.fn(),
      requestFeatureReport: vi.fn(),
      log: vi.fn(),
      error: vi.fn(),
    };

    const guest = new WasmUhciHidGuestBridge({ uhci, host });
    guest.poll();

    expect(host.requestFeatureReport).toHaveBeenCalledWith({ deviceId: 1, requestId: 7, reportId: 3 });

    const data = new Uint8Array([1, 2, 3]);
    expect(guest.completeFeatureReportRequest?.({ deviceId: 1, requestId: 7, reportId: 3, data })).toBe(true);
    expect(completeCalls).toEqual([[1, 7, 3, data]]);

    expect(guest.failFeatureReportRequest?.({ deviceId: 1, requestId: 7, reportId: 3, error: "nope" })).toBe(true);
    expect(failCalls).toEqual([[1, 7, 3]]);
  });
});

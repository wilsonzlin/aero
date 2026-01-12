import { describe, expect, it, vi } from "vitest";

import { WasmUhciHidGuestBridge, type UhciRuntimeHidApi } from "./wasm_uhci_hid_guest_bridge";
import type { HidAttachMessage } from "./hid_proxy_protocol";
import {
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT,
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
      collections: [{ some: "collection" }] as any,
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
      guestPath: [1],
      collections: [{ some: "collection" }] as any,
      hasInterruptOut: false,
    };
    guest.attach(attach);

    const expectedPath = [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(1)] as const;
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
      guestPort: 1,
      collections: [{ some: "collection" }] as any,
      hasInterruptOut: false,
    };
    guest.attach(attach);

    const expectedPath = [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(1)] as const;
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
});

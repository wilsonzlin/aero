import { describe, expect, it, vi } from "vitest";

import { WasmUhciHidGuestBridge, type UhciRuntimeHidApi } from "./wasm_uhci_hid_guest_bridge";
import type { HidAttachMessage } from "./hid_proxy_protocol";

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

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPath: [0, 3],
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
      attach.guestPath,
    );
    expect(webhid_attach).not.toHaveBeenCalled();
  });
});


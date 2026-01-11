import { describe, expect, it } from "vitest";

import type { HidAttachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import { InMemoryHidGuestBridge } from "./in_memory_hid_guest_bridge";
import type { HidHostSink } from "./wasm_hid_guest_bridge";

const noopHost: HidHostSink = {
  sendReport: () => {},
  log: () => {},
  error: () => {},
};

function makeAttach(deviceId: number): HidAttachMessage {
  return {
    type: "hid.attach",
    deviceId,
    vendorId: 0x1234,
    productId: 0x5678,
    collections: [],
    hasInterruptOut: false,
  };
}

function makeInputReport(deviceId: number, reportId: number): HidInputReportMessage {
  return {
    type: "hid.inputReport",
    deviceId,
    reportId,
    data: new Uint8Array([0x01, 0x02, 0x03]) as Uint8Array<ArrayBuffer>,
  };
}

describe("InMemoryHidGuestBridge", () => {
  it("preserves buffered input reports when attach arrives after input reports", () => {
    const bridge = new InMemoryHidGuestBridge(noopHost);
    bridge.inputReport(makeInputReport(1, 1));

    expect(bridge.inputReports.get(1)?.length).toBe(1);

    bridge.attach(makeAttach(1));
    expect(bridge.inputReports.get(1)?.length).toBe(1);
  });

  it("clears buffered input reports on re-attach", () => {
    const bridge = new InMemoryHidGuestBridge(noopHost);

    bridge.attach(makeAttach(1));
    bridge.inputReport(makeInputReport(1, 1));
    expect(bridge.inputReports.get(1)?.length).toBe(1);

    bridge.attach(makeAttach(1));
    expect(bridge.inputReports.get(1)?.length).toBe(0);
  });
});


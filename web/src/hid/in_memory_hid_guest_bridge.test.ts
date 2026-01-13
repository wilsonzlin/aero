import { describe, expect, it } from "vitest";

import type { HidAttachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import { InMemoryHidGuestBridge } from "./in_memory_hid_guest_bridge";
import type { HidHostSink } from "./wasm_hid_guest_bridge";

const noopHost: HidHostSink = {
  sendReport: () => {},
  requestFeatureReport: () => {},
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

  it("clamps oversized input reports before buffering (defense-in-depth)", () => {
    const bridge = new InMemoryHidGuestBridge(noopHost);

    const huge = new Uint8Array(1024 * 1024);
    huge.set([0xaa, 0xbb, 0xcc], 0);
    bridge.inputReport({
      type: "hid.inputReport",
      deviceId: 1,
      reportId: 1,
      data: huge as Uint8Array<ArrayBuffer>,
    });

    const buffered = bridge.inputReports.get(1);
    expect(buffered?.length).toBe(1);
    expect(buffered?.[0]!.data.byteLength).toBe(64);
    expect(Array.from(buffered?.[0]!.data.slice(0, 3) ?? [])).toEqual([0xaa, 0xbb, 0xcc]);
    // The buffered copy should not retain the original huge backing buffer.
    expect(buffered?.[0]!.data.buffer.byteLength).toBe(64);
  });
});

import { describe, expect, it } from "vitest";

import { decodeHidInputReportRingRecord, encodeHidInputReportRingRecord } from "./hid_input_report_ring";

describe("hid/hid_input_report_ring", () => {
  it("round-trips HID input report records", () => {
    const payload = encodeHidInputReportRingRecord({
      deviceId: 123,
      reportId: 7,
      tsMs: 456.78,
      data: Uint8Array.of(1, 2, 3),
    });

    const decoded = decodeHidInputReportRingRecord(payload);
    expect(decoded).toBeTruthy();
    expect(decoded!.deviceId).toBe(123);
    expect(decoded!.reportId).toBe(7);
    expect(decoded!.tsMs).toBe(456);
    expect(Array.from(decoded!.data)).toEqual([1, 2, 3]);
  });

  it("returns null for malformed records", () => {
    expect(decodeHidInputReportRingRecord(new Uint8Array())).toBeNull();
    expect(decodeHidInputReportRingRecord(new Uint8Array(11))).toBeNull();
  });
});


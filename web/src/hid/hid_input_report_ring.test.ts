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
    expect(decodeHidInputReportRingRecord(new Uint8Array(23))).toBeNull();

    // Invalid reportId (> u8).
    const badReportId = encodeHidInputReportRingRecord({
      deviceId: 1,
      reportId: 1,
      tsMs: 0,
      data: Uint8Array.of(1),
    });
    new DataView(badReportId.buffer, badReportId.byteOffset, badReportId.byteLength).setUint32(12, 0x1ff, true);
    expect(decodeHidInputReportRingRecord(badReportId)).toBeNull();

    // Oversized payload length (> 64 bytes).
    const tooLarge = new Uint8Array(24 + 65);
    const view = new DataView(tooLarge.buffer);
    view.setUint32(0, 0x5244_4948, true); // HID_INPUT_REPORT_RECORD_MAGIC
    view.setUint32(4, 1, true); // HID_INPUT_REPORT_RECORD_VERSION
    view.setUint32(8, 1, true); // deviceId
    view.setUint32(12, 1, true); // reportId
    view.setUint32(16, 0, true); // tsMs
    view.setUint32(20, 65, true); // len
    tooLarge.fill(0xaa, 24);
    expect(decodeHidInputReportRingRecord(tooLarge)).toBeNull();
  });
});

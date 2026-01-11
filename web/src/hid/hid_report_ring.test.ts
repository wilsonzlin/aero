import { describe, expect, it } from "vitest";

import { decodeInputReportRecord, encodeInputReportRecord, HID_REPORT_RING_MAGIC, HID_REPORT_RING_VERSION } from "./hid_report_ring";

describe("hid/hid_report_ring", () => {
  it("round-trips input report records", () => {
    const payload = encodeInputReportRecord(123, 7, Uint8Array.of(1, 2, 3), 456.78);
    const decoded = decodeInputReportRecord(payload);
    expect(decoded).toMatchObject({ deviceId: 123, reportId: 7, tsMs: 456 });
    expect(Array.from(decoded.data)).toEqual([1, 2, 3]);
  });

  it("treats tsMs=0 as absent", () => {
    const payload = encodeInputReportRecord(1, 2, Uint8Array.of(9));
    const decoded = decodeInputReportRecord(payload);
    expect(decoded.tsMs).toBeUndefined();
  });

  it("encodes a stable magic/version prefix", () => {
    const payload = encodeInputReportRecord(1, 1, new Uint8Array(0));
    const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    expect(view.getUint32(0, true)).toBe(HID_REPORT_RING_MAGIC);
    expect(view.getUint32(4, true)).toBe(HID_REPORT_RING_VERSION);
  });
});


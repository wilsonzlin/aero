import { describe, expect, it } from "vitest";

import { createHidReportRingBuffer, HidReportRing, HidReportType } from "./hid_report_ring";

describe("HidReportRing", () => {
  it("pushes and pops a single variable-length record", () => {
    const sab = createHidReportRingBuffer(64);
    const ring = new HidReportRing(sab);

    expect(ring.push(0x12345678, HidReportType.Input, 7, new Uint8Array([1, 2, 3]))).toBe(true);

    const rec = ring.pop();
    expect(rec).not.toBeNull();
    expect(rec).toMatchObject({ deviceId: 0x12345678, reportType: HidReportType.Input, reportId: 7 });
    expect(Array.from(rec!.payload)).toEqual([1, 2, 3]);

    expect(ring.pop()).toBeNull();
  });

  it("handles wraparound and preserves ordering", () => {
    const sab = createHidReportRingBuffer(64);
    const ring = new HidReportRing(sab);

    // Each record: header(8) + payload(8) = 16 bytes.
    const payload8 = new Uint8Array(8).fill(0xaa);
    expect(ring.push(1, HidReportType.Input, 1, payload8)).toBe(true);
    expect(ring.push(1, HidReportType.Input, 2, payload8)).toBe(true);
    expect(ring.push(1, HidReportType.Input, 3, payload8)).toBe(true);

    // Consume two records so there is free space at the start, but not enough at the end.
    expect(ring.pop()?.reportId).toBe(1);
    expect(ring.pop()?.reportId).toBe(2);

    // Record size: header(8) + payload(12) = 20 bytes. Tail is at 48, leaving only 16 bytes -> wrap.
    const payload12 = new Uint8Array(12).map((_, i) => i);
    expect(ring.push(2, HidReportType.Input, 4, payload12)).toBe(true);

    expect(ring.pop()?.reportId).toBe(3);
    const rec4 = ring.pop();
    expect(rec4).not.toBeNull();
    expect(rec4).toMatchObject({ deviceId: 2, reportType: HidReportType.Input, reportId: 4 });
    expect(Array.from(rec4!.payload)).toEqual(Array.from(payload12));

    expect(ring.pop()).toBeNull();
  });

  it("increments the drop counter when full", () => {
    const sab = createHidReportRingBuffer(32);
    const ring = new HidReportRing(sab);

    // header(8) + payload(8) = 16 bytes => exactly 2 records fill the 32-byte ring.
    const payload8 = new Uint8Array(8);
    expect(ring.push(1, HidReportType.Input, 1, payload8)).toBe(true);
    expect(ring.push(1, HidReportType.Input, 2, payload8)).toBe(true);
    expect(ring.dropped()).toBe(0);

    expect(ring.push(1, HidReportType.Input, 3, payload8)).toBe(false);
    expect(ring.dropped()).toBe(1);
  });
});

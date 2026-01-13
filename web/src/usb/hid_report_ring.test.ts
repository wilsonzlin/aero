import { describe, expect, it } from "vitest";

import {
  createHidReportRingBuffer,
  HidReportRing,
  HID_REPORT_RING_CTRL_BYTES,
  HID_REPORT_RING_CTRL_WORDS,
  HidReportType,
} from "./hid_report_ring";

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

  it("rejects payloads larger than 65535 bytes (u16 length field)", () => {
    const sab = createHidReportRingBuffer(128 * 1024);
    const ring = new HidReportRing(sab);

    const payload = new Uint8Array(70_000);
    expect(ring.dropped()).toBe(0);
    expect(ring.push(1, HidReportType.Input, 1, payload)).toBe(false);
    expect(ring.dropped()).toBe(1);
  });

  it("detects used>cap corruption and can reset to recover", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    // Force a non-zero drop counter so we can test resetDropped.
    expect(ring.push(1, HidReportType.Input, 1, new Uint8Array(cap))).toBe(false);
    expect(ring.dropped()).toBe(1);

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, 0);
    Atomics.store(ctrl, 1, (cap + 4) | 0);

    expect(ring.isEmpty()).toBe(false);
    expect(ring.isCorrupt()).toBe(true);

    ring.reset({ resetDropped: true });

    expect(ring.isEmpty()).toBe(true);
    expect(ring.isCorrupt()).toBe(false);
    expect(ring.dropped()).toBe(0);

    expect(ring.push(0x12345678, HidReportType.Input, 7, new Uint8Array([1, 2, 3]))).toBe(true);
    expect(ring.pop()?.reportId).toBe(7);
    expect(ring.pop()).toBeNull();
  });

  it("detects record-boundary corruption and can reset to recover", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    // Manufacture a record header at the very end of the ring such that the computed total
    // length would straddle the wrap boundary (which should be impossible in normal operation).
    const view = new DataView(sab, HID_REPORT_RING_CTRL_BYTES, cap);
    const headIndex = cap - 8;
    view.setUint32(headIndex + 0, 0xdeadbeef, true);
    view.setUint8(headIndex + 4, HidReportType.Input);
    view.setUint8(headIndex + 5, 1);
    view.setUint16(headIndex + 6, 4, true); // payloadLen=4 => total=12 > remaining(8)

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, headIndex | 0);
    Atomics.store(ctrl, 1, (headIndex + 8) | 0);

    // Pop cannot distinguish empty vs corrupt, but `isCorrupt()` can.
    expect(ring.pop()).toBeNull();
    expect(ring.isEmpty()).toBe(false);
    expect(ring.isCorrupt()).toBe(true);

    ring.reset();
    expect(ring.isEmpty()).toBe(true);
    expect(ring.isCorrupt()).toBe(false);

    expect(ring.push(0x11111111, HidReportType.Input, 2, new Uint8Array([9, 8, 7, 6]))).toBe(true);
    const rec = ring.pop();
    expect(rec).not.toBeNull();
    expect(rec).toMatchObject({ deviceId: 0x11111111, reportType: HidReportType.Input, reportId: 2 });
    expect(Array.from(rec!.payload)).toEqual([9, 8, 7, 6]);
    expect(ring.pop()).toBeNull();
  });

  it("popOrThrow throws on used>cap corruption", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, 0);
    Atomics.store(ctrl, 1, (cap + 4) | 0);

    expect(() => ring.popOrThrow()).toThrow(/tail\/head out of range/);
  });

  it("popOrThrow throws on straddling wrap boundary corruption", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    const view = new DataView(sab, HID_REPORT_RING_CTRL_BYTES, cap);
    const headIndex = cap - 8;
    view.setUint32(headIndex + 0, 0xdeadbeef, true);
    view.setUint8(headIndex + 4, HidReportType.Input);
    view.setUint8(headIndex + 5, 1);
    view.setUint16(headIndex + 6, 4, true); // payloadLen=4 => total=12 > remaining(8)

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, headIndex | 0);
    Atomics.store(ctrl, 1, (headIndex + 8) | 0);

    expect(() => ring.popOrThrow()).toThrow(/straddles wrap boundary/);
  });

  it("popOrThrow throws when a record exceeds the available used bytes", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    // Craft a header that claims a 4-byte payload (total=12), but only expose 8 bytes of used data.
    const view = new DataView(sab, HID_REPORT_RING_CTRL_BYTES, cap);
    view.setUint32(0, 0xdeadbeef, true);
    view.setUint8(4, HidReportType.Input);
    view.setUint8(5, 1);
    view.setUint16(6, 4, true);

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, 0);
    Atomics.store(ctrl, 1, 8);

    expect(() => ring.popOrThrow()).toThrow(/exceeds available bytes/);
  });

  it("consumeNextOrThrow throws on used>cap corruption", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, 0);
    Atomics.store(ctrl, 1, (cap + 4) | 0);

    expect(() => ring.consumeNextOrThrow(() => undefined)).toThrow(/tail\/head out of range/);
  });

  it("consumeNextOrThrow throws on straddling wrap boundary corruption", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    const view = new DataView(sab, HID_REPORT_RING_CTRL_BYTES, cap);
    const headIndex = cap - 8;
    view.setUint32(headIndex + 0, 0xdeadbeef, true);
    view.setUint8(headIndex + 4, HidReportType.Input);
    view.setUint8(headIndex + 5, 1);
    view.setUint16(headIndex + 6, 4, true); // payloadLen=4 => total=12 > remaining(8)

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, headIndex | 0);
    Atomics.store(ctrl, 1, (headIndex + 8) | 0);

    expect(() => ring.consumeNextOrThrow(() => undefined)).toThrow(/straddles wrap boundary/);
  });

  it("consumeNextOrThrow throws when a record exceeds the available used bytes", () => {
    const cap = 32;
    const sab = createHidReportRingBuffer(cap);
    const ring = new HidReportRing(sab);

    const view = new DataView(sab, HID_REPORT_RING_CTRL_BYTES, cap);
    view.setUint32(0, 0xdeadbeef, true);
    view.setUint8(4, HidReportType.Input);
    view.setUint8(5, 1);
    view.setUint16(6, 4, true);

    const ctrl = new Int32Array(sab, 0, HID_REPORT_RING_CTRL_WORDS);
    Atomics.store(ctrl, 0, 0);
    Atomics.store(ctrl, 1, 8);

    expect(() => ring.consumeNextOrThrow(() => undefined)).toThrow(/exceeds available bytes/);
  });
});

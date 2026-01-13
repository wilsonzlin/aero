import { describe, expect, it } from "vitest";

import { encodeHidInputReportRingRecord } from "../hid/hid_input_report_ring";
import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import type { HidInputReportMessage } from "../hid/hid_proxy_protocol";
import { drainIoHidInputRing } from "./io_hid_input_ring";

describe("workers/io_hid_input_ring", () => {
  it("drains valid records and drops invalid payloads", () => {
    const kind = 1;
    const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
    const ring = openRingByKind(sab, kind);

    ring.tryPush(encodeHidInputReportRingRecord({ deviceId: 1, reportId: 2, tsMs: 3, data: Uint8Array.of(4) }));
    ring.tryPush(encodeHidInputReportRingRecord({ deviceId: 5, reportId: 6, tsMs: 7, data: Uint8Array.of(8, 9) }));
    ring.tryPush(new Uint8Array([1, 2, 3])); // malformed (too short)

    const received: HidInputReportMessage[] = [];
    const res = drainIoHidInputRing(ring, (msg) => received.push(msg));

    expect(res.forwarded).toBe(2);
    expect(res.invalid).toBe(1);
    expect(received.map((m) => [m.deviceId, m.reportId, Array.from(m.data)])).toEqual([
      [1, 2, [4]],
      [5, 6, [8, 9]],
    ]);
  });

  it("bounds work per tick by record count", () => {
    const kind = 1;
    const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
    const ring = openRingByKind(sab, kind);

    const records = Array.from({ length: 8 }, (_, i) =>
      encodeHidInputReportRingRecord({ deviceId: i, reportId: i, tsMs: 0, data: Uint8Array.of(i) }),
    );
    for (const rec of records) expect(ring.tryPush(rec)).toBe(true);

    const received: HidInputReportMessage[] = [];
    const res = drainIoHidInputRing(ring, (msg) => received.push(msg), { maxRecords: 3 });
    expect(res.forwarded).toBe(3);
    expect(res.invalid).toBe(0);
    expect(res.bytes).toBe(records.slice(0, 3).reduce((sum, rec) => sum + rec.byteLength, 0));
    expect(received.map((m) => [m.deviceId, m.reportId, Array.from(m.data)])).toEqual([
      [0, 0, [0]],
      [1, 1, [1]],
      [2, 2, [2]],
    ]);

    const remaining: number[] = [];
    for (;;) {
      const msg = ring.tryPop();
      if (!msg) break;
      const record = new DataView(msg.buffer, msg.byteOffset, msg.byteLength).getUint32(8, true);
      remaining.push(record);
    }
    expect(remaining).toEqual([3, 4, 5, 6, 7]);
  });

  it("counts invalid records and continues draining subsequent valid ones", () => {
    const kind = 1;
    const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
    const ring = openRingByKind(sab, kind);

    const good1 = encodeHidInputReportRingRecord({ deviceId: 1, reportId: 1, tsMs: 0, data: Uint8Array.of(1) });

    const badMagic = encodeHidInputReportRingRecord({ deviceId: 2, reportId: 2, tsMs: 0, data: Uint8Array.of(2) });
    new DataView(badMagic.buffer, badMagic.byteOffset, badMagic.byteLength).setUint32(0, 0xdead_beef, true);

    const good2 = encodeHidInputReportRingRecord({ deviceId: 3, reportId: 3, tsMs: 0, data: Uint8Array.of(3) });

    const badVersion = encodeHidInputReportRingRecord({ deviceId: 4, reportId: 4, tsMs: 0, data: Uint8Array.of(4) });
    new DataView(badVersion.buffer, badVersion.byteOffset, badVersion.byteLength).setUint32(4, 1234, true);

    const badLen = encodeHidInputReportRingRecord({ deviceId: 5, reportId: 5, tsMs: 0, data: Uint8Array.of(5) });
    new DataView(badLen.buffer, badLen.byteOffset, badLen.byteLength).setUint32(20, 0xffff_ffff, true);

    const good3 = encodeHidInputReportRingRecord({ deviceId: 6, reportId: 6, tsMs: 0, data: Uint8Array.of(6) });

    const all = [good1, badMagic, good2, badVersion, badLen, good3];
    for (const rec of all) expect(ring.tryPush(rec)).toBe(true);

    const received: HidInputReportMessage[] = [];
    const res = drainIoHidInputRing(ring, (msg) => received.push(msg));

    expect(res.forwarded).toBe(3);
    expect(res.invalid).toBe(3);
    expect(res.bytes).toBe(all.reduce((sum, rec) => sum + rec.byteLength, 0));
    expect(received.map((m) => m.reportId)).toEqual([1, 3, 6]);
    expect(ring.tryPop()).toBeNull();
  });

  it("bounds work per tick by byte count", () => {
    const kind = 1;
    const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
    const ring = openRingByKind(sab, kind);

    const records = Array.from({ length: 4 }, (_, i) =>
      encodeHidInputReportRingRecord({ deviceId: i, reportId: i, tsMs: 0, data: Uint8Array.of(i) }),
    );
    for (const rec of records) expect(ring.tryPush(rec)).toBe(true);

    const maxBytes = records[0]!.byteLength + records[1]!.byteLength;
    const received: HidInputReportMessage[] = [];
    const res = drainIoHidInputRing(ring, (msg) => received.push(msg), { maxBytes });

    expect(res.forwarded).toBe(2);
    expect(res.invalid).toBe(0);
    expect(res.bytes).toBe(maxBytes);
    expect(received.map((m) => m.reportId)).toEqual([0, 1]);

    const remaining: number[] = [];
    for (;;) {
      const msg = ring.tryPop();
      if (!msg) break;
      const record = new DataView(msg.buffer, msg.byteOffset, msg.byteLength).getUint32(12, true);
      remaining.push(record);
    }
    expect(remaining).toEqual([2, 3]);
  });

  it("does not wedge the ring if the consumer throws", () => {
    const kind = 1;
    const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
    const ring = openRingByKind(sab, kind);

    ring.tryPush(encodeHidInputReportRingRecord({ deviceId: 1, reportId: 1, tsMs: 0, data: Uint8Array.of(1) }));
    ring.tryPush(encodeHidInputReportRingRecord({ deviceId: 1, reportId: 2, tsMs: 0, data: Uint8Array.of(2) }));

    const received: HidInputReportMessage[] = [];
    const res = drainIoHidInputRing(ring, (msg) => {
      if (msg.reportId === 1) throw new Error("boom");
      received.push(msg);
    });

    expect(res.forwarded).toBe(1);
    expect(res.invalid).toBe(1);
    expect(received.map((m) => m.reportId)).toEqual([2]);
    expect(ring.tryPop()).toBeNull();
  });
});

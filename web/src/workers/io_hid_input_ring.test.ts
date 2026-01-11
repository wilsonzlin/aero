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

    for (let i = 0; i < 8; i++) {
      ring.tryPush(encodeHidInputReportRingRecord({ deviceId: i, reportId: i, tsMs: 0, data: Uint8Array.of(i) }));
    }

    const received: HidInputReportMessage[] = [];
    const res = drainIoHidInputRing(ring, (msg) => received.push(msg), { maxRecords: 3 });
    expect(res.forwarded).toBe(3);

    let remaining = 0;
    while (ring.tryPop()) remaining += 1;
    expect(remaining).toBe(5);
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

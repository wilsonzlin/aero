import { describe, expect, it, vi } from "vitest";

import type { HidSendReportMessage } from "../hid/hid_proxy_protocol";
import { createHidReportRingBuffer, HidReportRing, HidReportType } from "../usb/hid_report_ring";
import { forwardHidSendReportToMainThread } from "./io_hid_output_report_forwarding";

describe("io_hid_output_report_forwarding", () => {
  it("uses the output ring fast path when push succeeds", () => {
    const ring = new HidReportRing(createHidReportRingBuffer(64));
    const postMessage = vi.fn<[HidSendReportMessage, Transferable[]], void>();
    const payload = { deviceId: 1, reportType: "output" as const, reportId: 2, data: new Uint8Array([1, 2, 3]) };

    const res = forwardHidSendReportToMainThread(payload, {
      outputRing: ring,
      postMessage,
    });

    expect(res).toEqual({ path: "ring" });
    expect(postMessage).not.toHaveBeenCalled();

    const rec = ring.pop();
    expect(rec).not.toBeNull();
    expect(rec).toMatchObject({ deviceId: 1, reportType: HidReportType.Output, reportId: 2 });
    expect(Array.from(rec!.payload)).toEqual([1, 2, 3]);
  });

  it("falls back to postMessage when the ring push fails (payload too large)", () => {
    const ring = new HidReportRing(createHidReportRingBuffer(16));

    const sent: Array<{ msg: HidSendReportMessage; transfer: Transferable[] }> = [];
    const postMessage = (msg: HidSendReportMessage, transfer: Transferable[]): void => {
      sent.push({ msg, transfer });
    };

    const shared = new SharedArrayBuffer(64);
    const bytes = new Uint8Array(shared, 0, 32);
    bytes.fill(0x11);

    const payload = { deviceId: 7, reportType: "feature" as const, reportId: 9, data: bytes };
    const res = forwardHidSendReportToMainThread(payload, {
      outputRing: ring,
      postMessage,
    });

    expect(res).toEqual({ path: "postMessage", ringFailed: true });
    expect(sent).toHaveLength(1);

    const { msg, transfer } = sent[0];
    expect(msg).toMatchObject({ type: "hid.sendReport", deviceId: 7, reportType: "feature", reportId: 9 });
    expect(msg.outputRingTail).toBe(0);
    expect(msg.data.byteLength).toBe(32);
    expect(msg.data.buffer).toBeInstanceOf(ArrayBuffer);
    expect(msg.data.buffer).not.toBe(shared);
    expect(Array.from(msg.data)).toEqual(Array.from(bytes));
    expect(transfer).toEqual([msg.data.buffer]);
  });

  it("does not include outputRingTail when no ring is available", () => {
    const postMessage = vi.fn<[HidSendReportMessage, Transferable[]], void>();
    const payload = { deviceId: 1, reportType: "output" as const, reportId: 2, data: new Uint8Array([1, 2, 3]) };

    const res = forwardHidSendReportToMainThread(payload, {
      outputRing: null,
      postMessage,
    });

    expect(res).toEqual({ path: "postMessage", ringFailed: false });
    expect(postMessage).toHaveBeenCalledTimes(1);
    const [msg] = postMessage.mock.calls[0]!;
    expect(msg.outputRingTail).toBeUndefined();
  });
});

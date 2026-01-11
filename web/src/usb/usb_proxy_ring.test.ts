import { describe, expect, it } from "vitest";

import { createUsbProxyRingBuffer, UsbProxyRing } from "./usb_proxy_ring";
import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";

describe("usb/UsbProxyRing", () => {
  it("round-trips all UsbHostAction kinds", () => {
    const sab = createUsbProxyRingBuffer(512);
    const ring = new UsbProxyRing(sab);

    const setup = { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 18 };
    const actions: UsbHostAction[] = [
      { kind: "controlIn", id: 1, setup },
      { kind: "controlOut", id: 2, setup: { ...setup, wLength: 3 }, data: Uint8Array.of(1, 2, 3) },
      { kind: "bulkIn", id: 3, endpoint: 0x81, length: 64 },
      { kind: "bulkOut", id: 4, endpoint: 0x02, data: Uint8Array.of(9, 8, 7) },
    ];

    for (const action of actions) {
      expect(ring.pushAction(action)).toBe(true);
    }

    const roundTripped: UsbHostAction[] = [];
    while (true) {
      const next = ring.popAction();
      if (!next) break;
      roundTripped.push(next);
    }

    expect(roundTripped).toHaveLength(actions.length);
    expect(roundTripped[0]).toEqual(actions[0]);

    // Uint8Array payloads are copied; compare by bytes.
    const controlOut = roundTripped[1];
    if (controlOut?.kind !== "controlOut") throw new Error("unreachable");
    const expectedControlOut = actions[1];
    if (!expectedControlOut || expectedControlOut.kind !== "controlOut") throw new Error("unreachable");
    expect(controlOut.setup).toEqual(expectedControlOut.setup);
    expect(Array.from(controlOut.data)).toEqual(Array.from(expectedControlOut.data));

    expect(roundTripped[2]).toEqual(actions[2]);

    const bulkOut = roundTripped[3];
    if (bulkOut?.kind !== "bulkOut") throw new Error("unreachable");
    expect(bulkOut.endpoint).toBe((actions[3] as Extract<UsbHostAction, { kind: "bulkOut" }>).endpoint);
    expect(Array.from(bulkOut.data)).toEqual(Array.from((actions[3] as Extract<UsbHostAction, { kind: "bulkOut" }>).data));
  });

  it("round-trips all UsbHostCompletion variants", () => {
    const sab = createUsbProxyRingBuffer(1024);
    const ring = new UsbProxyRing(sab);

    const completions: UsbHostCompletion[] = [
      { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) },
      { kind: "bulkOut", id: 2, status: "success", bytesWritten: 3 },
      { kind: "controlIn", id: 3, status: "stall" },
      { kind: "controlOut", id: 4, status: "error", message: "nope" },
    ];

    for (const completion of completions) {
      expect(ring.pushCompletion(completion)).toBe(true);
    }

    const roundTripped: UsbHostCompletion[] = [];
    while (true) {
      const next = ring.popCompletion();
      if (!next) break;
      roundTripped.push(next);
    }

    expect(roundTripped).toHaveLength(completions.length);

    const inSuccess = roundTripped[0];
    if (!inSuccess || inSuccess.kind !== "bulkIn" || inSuccess.status !== "success") throw new Error("unreachable");
    expect(Array.from(inSuccess.data)).toEqual([1]);

    expect(roundTripped[1]).toEqual(completions[1]);
    expect(roundTripped[2]).toEqual(completions[2]);
    expect(roundTripped[3]).toEqual(completions[3]);
  });

  it("handles wraparound and preserves ordering", () => {
    const sab = createUsbProxyRingBuffer(64);
    const ring = new UsbProxyRing(sab);

    // Each bulkIn record is 16 bytes; 3 pushes land at offsets 0,16,32.
    expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 1, length: 8 })).toBe(true);
    expect(ring.pushAction({ kind: "bulkIn", id: 2, endpoint: 1, length: 8 })).toBe(true);
    expect(ring.pushAction({ kind: "bulkIn", id: 3, endpoint: 1, length: 8 })).toBe(true);

    // Consume 2 records so there is free space at the start, but not enough at the end.
    expect(ring.popAction()?.id).toBe(1);
    expect(ring.popAction()?.id).toBe(2);

    // bulkOut record is 16 + 1 bytes (aligned to 20). Tail is at 48, leaving 16 bytes -> wrap.
    expect(ring.pushAction({ kind: "bulkOut", id: 4, endpoint: 2, data: Uint8Array.of(9) })).toBe(true);

    expect(ring.popAction()?.id).toBe(3);
    const wrapped = ring.popAction();
    expect(wrapped?.id).toBe(4);

    expect(ring.popAction()).toBeNull();
  });

  it("increments the drop counter when full", () => {
    const sab = createUsbProxyRingBuffer(20);
    const ring = new UsbProxyRing(sab);

    // bulkIn record is 16 bytes; only one fits in 20-byte ring.
    expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 1, length: 8 })).toBe(true);
    expect(ring.dropped()).toBe(0);

    expect(ring.pushAction({ kind: "bulkIn", id: 2, endpoint: 1, length: 8 })).toBe(false);
    expect(ring.dropped()).toBe(1);
  });
});

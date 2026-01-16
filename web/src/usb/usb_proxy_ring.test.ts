import { describe, expect, it } from "vitest";

import { createUsbProxyRingBuffer, USB_PROXY_RING_CTRL_BYTES, UsbProxyRing } from "./usb_proxy_ring";
import { MAX_USB_PROXY_BYTES, type UsbHostAction, type UsbHostCompletion } from "./usb_proxy_protocol";

describe("usb/UsbProxyRing", () => {
  it("validates createUsbProxyRingBuffer inputs", () => {
    expect(() => createUsbProxyRingBuffer(0)).toThrow(/positive safe integer/);
    expect(() => createUsbProxyRingBuffer(-1)).toThrow(/positive safe integer/);
    expect(() => createUsbProxyRingBuffer(16 * 1024 * 1024 + 1)).toThrow(/must be <=/);
  });

  it("aligns createUsbProxyRingBuffer capacity to record alignment", () => {
    const sab = createUsbProxyRingBuffer(65);
    const ring = new UsbProxyRing(sab);
    expect(ring.dataCapacityBytes()).toBe(68);
  });

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
    if (!bulkOut || bulkOut.kind !== "bulkOut") throw new Error("unreachable");
    const expectedBulkOut = actions[3] as Extract<UsbHostAction, { kind: "bulkOut" }>;
    expect(bulkOut.endpoint).toBe(expectedBulkOut.endpoint);
    expect(Array.from(bulkOut.data)).toEqual(Array.from(expectedBulkOut.data));
  });

  it("round-trips action options via header flags", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    const setup = { bmRequestType: 0x80, bRequest: 6, wValue: 0x0200, wIndex: 0, wLength: 9 };
    const action: UsbHostAction = { kind: "controlIn", id: 1, setup };
    expect(ring.pushAction(action, { translateOtherSpeedConfigurationDescriptor: false })).toBe(true);

    const record = ring.popActionRecord();
    expect(record).toBeTruthy();
    expect(record?.action).toEqual(action);
    expect(record?.options).toEqual({ translateOtherSpeedConfigurationDescriptor: false });
    expect(ring.popAction()).toBeNull();
  });

  it("popActionInfo drains actions without copying payload buffers", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    const payload = Uint8Array.of(1, 2, 3);
    expect(ring.pushAction({ kind: "bulkOut", id: 1, endpoint: 0x01, data: payload })).toBe(true);

    expect(ring.popActionInfo()).toEqual({ kind: "bulkOut", id: 1, options: undefined, payloadBytes: payload.byteLength });
    expect(ring.popAction()).toBeNull();
  });

  it("rejects bulkIn actions with lengths exceeding MAX_USB_PROXY_BYTES", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: MAX_USB_PROXY_BYTES + 1 })).toBe(false);
    expect(ring.dropped()).toBe(1);
  });

  it("rejects bulkOut actions with payloads exceeding MAX_USB_PROXY_BYTES", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    const payload = new Uint8Array(MAX_USB_PROXY_BYTES + 1);
    expect(ring.pushAction({ kind: "bulkOut", id: 1, endpoint: 0x02, data: payload })).toBe(false);
    expect(ring.dropped()).toBe(1);
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

  it("truncates oversized error messages to fit the ring", () => {
    // dataCapacityBytes=64 => fixed error record header is 12 bytes, leaving 52 bytes for message.
    // The truncation marker is 12 bytes, so we expect 40 bytes of payload + marker.
    const sab = createUsbProxyRingBuffer(64);
    const ring = new UsbProxyRing(sab);

    const completion: UsbHostCompletion = {
      kind: "controlOut",
      id: 1,
      status: "error",
      message: "a".repeat(100),
    };
    expect(ring.pushCompletion(completion)).toBe(true);

    const popped = ring.popCompletion();
    if (!popped || popped.status !== "error") throw new Error("unreachable");
    expect(popped.message).toBe("a".repeat(40) + " [truncated]");
    expect(ring.popCompletion()).toBeNull();
  });

  it("sanitizes and byte-bounds error completion messages", () => {
    const sab = createUsbProxyRingBuffer(64 * 1024);
    const ring = new UsbProxyRing(sab);

    const multiline: UsbHostCompletion = {
      kind: "controlOut",
      id: 1,
      status: "error",
      message: "hello\n\tworld",
    };
    expect(ring.pushCompletion(multiline)).toBe(true);
    const popped1 = ring.popCompletion();
    if (!popped1 || popped1.status !== "error") throw new Error("unreachable");
    expect(popped1.message).toBe("hello world");

    const huge: UsbHostCompletion = {
      kind: "controlOut",
      id: 2,
      status: "error",
      message: "x".repeat(600),
    };
    expect(ring.pushCompletion(huge)).toBe(true);
    const popped2 = ring.popCompletion();
    if (!popped2 || popped2.status !== "error") throw new Error("unreachable");
    expect(popped2.message).toBe("x".repeat(512));
    expect(new TextEncoder().encode(popped2.message).byteLength).toBeLessThanOrEqual(512);

    expect(ring.popCompletion()).toBeNull();
  });

  it("handles wraparound and preserves ordering", () => {
    const sab = createUsbProxyRingBuffer(64);
    const ring = new UsbProxyRing(sab);

    // Each bulkIn record is 16 bytes; 3 pushes land at offsets 0,16,32.
    expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 })).toBe(true);
    expect(ring.pushAction({ kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 })).toBe(true);
    expect(ring.pushAction({ kind: "bulkIn", id: 3, endpoint: 0x81, length: 8 })).toBe(true);

    // Consume 2 records so there is free space at the start, but not enough at the end.
    expect(ring.popAction()?.id).toBe(1);
    expect(ring.popAction()?.id).toBe(2);

    // bulkOut record is 16 + 1 bytes (aligned to 20). Tail is at 48, leaving 16 bytes -> wrap.
    expect(ring.pushAction({ kind: "bulkOut", id: 4, endpoint: 0x02, data: Uint8Array.of(9) })).toBe(true);

    expect(ring.popAction()?.id).toBe(3);
    const wrapped = ring.popAction();
    expect(wrapped?.id).toBe(4);

    expect(ring.popAction()).toBeNull();
  });

  it("increments the drop counter when full", () => {
    const sab = createUsbProxyRingBuffer(20);
    const ring = new UsbProxyRing(sab);

    // bulkIn record is 16 bytes; only one fits in 20-byte ring.
    expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 })).toBe(true);
    expect(ring.dropped()).toBe(0);

    expect(ring.pushAction({ kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 })).toBe(false);
    expect(ring.dropped()).toBe(1);
  });

  it("rejects malformed controlOut payload length mismatches and keeps the ring usable", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    const invalid = {
      kind: "controlOut",
      id: 1,
      setup: { bmRequestType: 0, bRequest: 9, wValue: 1, wIndex: 0, wLength: 2 },
      data: Uint8Array.of(1, 2, 3),
    } as const satisfies UsbHostAction;

    expect(ring.pushAction(invalid)).toBe(false);
    expect(ring.dropped()).toBe(1);
    expect(ring.popAction()).toBeNull();

    const valid: UsbHostAction = { kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 };
    expect(ring.pushAction(valid)).toBe(true);
    expect(ring.popAction()).toEqual(valid);
    expect(ring.popAction()).toBeNull();
  });

  it("throws when an action record claims more bytes than are available (tail/head inconsistent)", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    const setup = { bmRequestType: 0, bRequest: 9, wValue: 1, wIndex: 0, wLength: 3 };
    expect(ring.pushAction({ kind: "controlOut", id: 1, setup, data: Uint8Array.of(1, 2, 3) })).toBe(true);

    // Corrupt the record header to claim a larger payload without advancing the tail.
    const view = new DataView(sab, USB_PROXY_RING_CTRL_BYTES);
    // setup.wLength lives at offset 8 (action header) + 6 (setup.wLength field).
    view.setUint16(8 + 6, 100, true);
    // dataLen lives at offset 8 (action header) + 8 (setup) = 16.
    view.setUint32(8 + 8, 100, true);

    expect(() => ring.popActionRecord()).toThrow(/exceeds available bytes/);
  });

  it("throws when a bulkOut action record claims a payload larger than MAX_USB_PROXY_BYTES", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushAction({ kind: "bulkOut", id: 1, endpoint: 0x02, data: Uint8Array.of(1) })).toBe(true);

    const view = new DataView(sab, USB_PROXY_RING_CTRL_BYTES);
    // bulkOut dataLen lives at offset 8 (action header) + 4.
    view.setUint32(8 + 4, MAX_USB_PROXY_BYTES + 1, true);

    expect(() => ring.popActionRecord()).toThrow(/payload too large/i);
  });

  it("throws when a completion record claims more bytes than are available (tail/head inconsistent)", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushCompletion({ kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) })).toBe(true);

    // Corrupt the completion data length without advancing the tail.
    const view = new DataView(sab, USB_PROXY_RING_CTRL_BYTES);
    // completion dataLen lives at offset 8 (completion header).
    view.setUint32(8, 200, true);

    expect(() => ring.popCompletion()).toThrow(/exceeds available bytes/);
  });

  it("throws when a completion record claims a payload larger than MAX_USB_PROXY_BYTES", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushCompletion({ kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) })).toBe(true);

    const view = new DataView(sab, USB_PROXY_RING_CTRL_BYTES);
    // completion dataLen lives at offset 8 (completion header).
    view.setUint32(8, MAX_USB_PROXY_BYTES + 1, true);

    expect(() => ring.popCompletion()).toThrow(/payload too large/i);
  });

  it("throws when an error completion record claims a message larger than the hard cap", () => {
    const sab = createUsbProxyRingBuffer(64 * 1024);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushCompletion({ kind: "controlOut", id: 1, status: "error", message: "nope" })).toBe(true);

    const ctrl = new Int32Array(sab, 0, 3);
    const view = new DataView(sab, USB_PROXY_RING_CTRL_BYTES);
    const msgLen = 16 * 1024 + 1;
    // msgLen lives at offset 8 (completion header).
    view.setUint32(8, msgLen, true);
    // Pretend the producer advanced tail far enough for the record to be "fully written".
    const fixed = 12;
    const total = Math.ceil((fixed + msgLen) / 4) * 4;
    Atomics.store(ctrl, 1, total);

    expect(() => ring.popCompletion()).toThrow(/error message too large/i);
  });

  it("rejects invalid bulkIn endpoint addresses (OUT endpoints)", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushAction({ kind: "bulkIn", id: 1, endpoint: 0x02, length: 8 })).toBe(false);
    expect(ring.dropped()).toBe(1);
    expect(ring.popAction()).toBeNull();

    const valid: UsbHostAction = { kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 };
    expect(ring.pushAction(valid)).toBe(true);
    expect(ring.popAction()).toEqual(valid);
    expect(ring.popAction()).toBeNull();
  });

  it("rejects invalid bulkOut endpoint addresses (IN endpoints)", () => {
    const sab = createUsbProxyRingBuffer(256);
    const ring = new UsbProxyRing(sab);

    expect(ring.pushAction({ kind: "bulkOut", id: 1, endpoint: 0x81, data: Uint8Array.of(1) })).toBe(false);
    expect(ring.dropped()).toBe(1);
    expect(ring.popAction()).toBeNull();

    const ok = { kind: "bulkOut", id: 2, endpoint: 0x02, data: Uint8Array.of(1) } as const satisfies UsbHostAction;
    expect(ring.pushAction(ok)).toBe(true);
    const popped = ring.popAction();
    if (!popped || popped.kind !== "bulkOut") throw new Error("unreachable");
    expect(popped.endpoint).toBe(ok.endpoint);
    expect(Array.from(popped.data)).toEqual(Array.from(ok.data));
    expect(ring.popAction()).toBeNull();
  });
});

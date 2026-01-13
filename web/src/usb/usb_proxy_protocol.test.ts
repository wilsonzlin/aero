import { describe, expect, it } from "vitest";

import {
  getTransferablesForUsbActionMessage,
  getTransferablesForUsbCompletionMessage,
  isUsbActionMessage,
  isUsbCompletionMessage,
  isUsbHostAction,
  isUsbHostCompletion,
  isUsbGuestControllerModeMessage,
  isUsbProxyMessage,
  isUsbRingAttachMessage,
  isUsbRingAttachRequestMessage,
  isUsbRingDetachMessage,
  MAX_USB_PROXY_BYTES,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
} from "./usb_proxy_protocol";

describe("usb/usb_proxy_protocol", () => {
  it("accepts the supported action shapes", () => {
    const setup = { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 18 };

    const actions: UsbHostAction[] = [
      { kind: "controlIn", id: 1, setup },
      { kind: "controlOut", id: 2, setup: { ...setup, wLength: 3 }, data: Uint8Array.of(1, 2, 3) },
      { kind: "bulkIn", id: 3, endpoint: 0x81, length: 64 },
      { kind: "bulkOut", id: 4, endpoint: 2, data: Uint8Array.of(9, 8, 7) },
    ];

    for (const action of actions) {
      expect(isUsbHostAction(action)).toBe(true);
    }

    expect(isUsbHostAction({ kind: "bulkIn", id: "nope" })).toBe(false);
    expect(isUsbHostAction({ kind: "unknown", id: 1 })).toBe(false);
  });

  it("rejects invalid numeric ranges in actions/completions", () => {
    const setup = { bmRequestType: 0x80, bRequest: 6, wValue: 0x0100, wIndex: 0, wLength: 18 };

    // id must be a safe integer.
    expect(isUsbHostAction({ kind: "controlIn", id: 1.5, setup })).toBe(false);
    expect(isUsbHostAction({ kind: "controlIn", id: Number.NaN, setup })).toBe(false);
    expect(isUsbHostAction({ kind: "controlIn", id: -1, setup })).toBe(false);
    expect(isUsbHostAction({ kind: "controlIn", id: 0xffff_ffff + 1, setup })).toBe(false);

    // Setup packet fields are u8/u16.
    expect(isUsbHostAction({ kind: "controlIn", id: 1, setup: { ...setup, wLength: 0x1_0000 } })).toBe(false);
    expect(isUsbHostAction({ kind: "controlIn", id: 1, setup: { ...setup, bmRequestType: 0x1_00 } })).toBe(false);

    // controlOut data length must match wLength.
    expect(
      isUsbHostAction({
        kind: "controlOut",
        id: 1,
        setup: { ...setup, wLength: 1 },
        data: Uint8Array.of(1, 2),
      }),
    ).toBe(false);

    // Endpoint is u8, length is non-negative.
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x1_00, length: 8 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: -1 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 0xffff_ffff + 1 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x81, length: MAX_USB_PROXY_BYTES + 1 })).toBe(false);

    // bytesWritten must be a non-negative safe integer.
    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: 1.5 })).toBe(false);
    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: -1 })).toBe(false);
    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: 0xffff_ffff + 1 })).toBe(false);

    // usb.selected vendor/product IDs are u16.
    expect(
      isUsbProxyMessage({ type: "usb.selected", ok: true, info: { vendorId: 0x1_0000, productId: 1 } }),
    ).toBe(false);
  });

  it("rejects oversized and unsafe byte payload buffers in actions/completions", () => {
    const setup = { bmRequestType: 0, bRequest: 9, wValue: 1, wIndex: 0, wLength: 1 };

    const oversized = new Uint8Array(MAX_USB_PROXY_BYTES + 1);
    expect(isUsbHostAction({ kind: "controlOut", id: 1, setup, data: oversized })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkOut", id: 2, endpoint: 0x02, data: oversized })).toBe(false);
    expect(isUsbHostCompletion({ kind: "controlIn", id: 3, status: "success", data: oversized })).toBe(false);

    // Reject subviews into oversized buffers: structured cloning can copy the full buffer, not just the view.
    const largeBuffer = new ArrayBuffer(MAX_USB_PROXY_BYTES + 1);
    const view = new Uint8Array(largeBuffer, 0, 1);
    expect(isUsbHostAction({ kind: "bulkOut", id: 4, endpoint: 0x02, data: view })).toBe(false);
    expect(isUsbHostCompletion({ kind: "bulkIn", id: 5, status: "success", data: view })).toBe(false);

    // SharedArrayBuffer-backed payloads should not be accepted in wire messages (avoid sharing large SABs).
    if (typeof SharedArrayBuffer !== "undefined") {
      const sab = new SharedArrayBuffer(16);
      const sabView = new Uint8Array(sab);
      expect(isUsbHostAction({ kind: "bulkOut", id: 6, endpoint: 0x02, data: sabView })).toBe(false);
      expect(isUsbHostCompletion({ kind: "bulkIn", id: 7, status: "success", data: sabView })).toBe(false);
    }
  });

  it("rejects malformed usb.selected messages", () => {
    // ok:true requires info and forbids error.
    expect(isUsbProxyMessage({ type: "usb.selected", ok: true, error: "nope", info: { vendorId: 1, productId: 2 } })).toBe(false);
    expect(isUsbProxyMessage({ type: "usb.selected", ok: true, info: { vendorId: 1, productId: 2, productName: 123 } })).toBe(false);

    // ok:false forbids info.
    expect(isUsbProxyMessage({ type: "usb.selected", ok: false, info: { vendorId: 1, productId: 2 } })).toBe(false);
  });

  it("rejects invalid bulk endpoint directions and endpoint 0", () => {
    // bulkIn expects an IN endpoint address (`0x80 | ep_num`).
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 1, length: 8 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x80, length: 8 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x91, length: 8 })).toBe(false);

    // bulkOut expects an OUT endpoint address (`ep_num` with bit7 clear).
    expect(isUsbHostAction({ kind: "bulkOut", id: 1, endpoint: 0x81, data: Uint8Array.of(1) })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkOut", id: 1, endpoint: 0, data: Uint8Array.of(1) })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkOut", id: 1, endpoint: 0x71, data: Uint8Array.of(1) })).toBe(false);
  });

  it("accepts the supported completion shapes", () => {
    const completions: UsbHostCompletion[] = [
      { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) },
      { kind: "bulkOut", id: 2, status: "success", bytesWritten: 3 },
      { kind: "controlIn", id: 3, status: "stall" },
      usbErrorCompletion("controlOut", 4, "nope"),
    ];

    for (const completion of completions) {
      expect(isUsbHostCompletion(completion)).toBe(true);
    }

    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: "bad" })).toBe(false);
    expect(isUsbHostCompletion({ kind: "unknown", id: 1 })).toBe(false);
  });

  it("validates usb.* envelope messages", () => {
    const actionMsg: UsbActionMessage = {
      type: "usb.action",
      action: { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 },
    };
    expect(isUsbActionMessage(actionMsg)).toBe(true);
    expect(isUsbProxyMessage(actionMsg)).toBe(true);

    const completionMsg: UsbCompletionMessage = {
      type: "usb.completion",
      completion: { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1, 2) },
    };
    expect(isUsbCompletionMessage(completionMsg)).toBe(true);
    expect(isUsbProxyMessage(completionMsg)).toBe(true);

    expect(isUsbProxyMessage({ type: "usb.querySelected" })).toBe(true);

    const ringAttach = { type: "usb.ringAttach", actionRing: new SharedArrayBuffer(16), completionRing: new SharedArrayBuffer(16) };
    expect(isUsbRingAttachMessage(ringAttach)).toBe(true);
    expect(isUsbProxyMessage(ringAttach)).toBe(true);

    const ringAttachReq = { type: "usb.ringAttachRequest" };
    expect(isUsbRingAttachRequestMessage(ringAttachReq)).toBe(true);
    expect(isUsbProxyMessage(ringAttachReq)).toBe(true);

    const ringDetach = { type: "usb.ringDetach", reason: "disable fast path" };
    expect(isUsbRingDetachMessage(ringDetach)).toBe(true);
    expect(isUsbProxyMessage(ringDetach)).toBe(true);
    expect(isUsbProxyMessage({ type: "usb.ringDetach" })).toBe(true);
    expect(isUsbProxyMessage({ type: "usb.ringDetach", reason: 123 })).toBe(false);
    expect(isUsbGuestControllerModeMessage({ type: "usb.guest.controller", mode: "uhci" })).toBe(true);
    expect(isUsbProxyMessage({ type: "usb.guest.controller", mode: "uhci" })).toBe(true);
    expect(isUsbGuestControllerModeMessage({ type: "usb.guest.controller", mode: "ehci" })).toBe(true);
    expect(isUsbGuestControllerModeMessage({ type: "usb.guest.controller", mode: "nope" })).toBe(false);
    expect(
      isUsbProxyMessage({
        type: "usb.guest.status",
        snapshot: { available: true, attached: false, blocked: true, controllerKind: "xhci", rootPort: 1, lastError: null },
      }),
    ).toBe(true);

    expect(isUsbProxyMessage({ type: "usb.action", action: { kind: "bulkIn", id: 1 } })).toBe(false);
    expect(isUsbProxyMessage({ type: "unknown" })).toBe(false);
  });

  it("messages are structured-cloneable", () => {
    const msg: UsbActionMessage = {
      type: "usb.action",
      action: {
        kind: "controlOut",
        id: 123,
        setup: { bmRequestType: 0, bRequest: 9, wValue: 1, wIndex: 0, wLength: 3 },
        data: Uint8Array.of(1, 2, 3),
      },
    };

    const cloned = structuredClone(msg) as unknown;
    expect(isUsbProxyMessage(cloned)).toBe(true);

    const completion: UsbCompletionMessage = {
      type: "usb.completion",
      completion: { kind: "bulkIn", id: 123, status: "success", data: Uint8Array.of(9, 8, 7) },
    };
    const clonedCompletion = structuredClone(completion) as unknown;
    expect(isUsbProxyMessage(clonedCompletion)).toBe(true);
  });

  it("provides transferables for bulk/control payloads", () => {
    const actionMsg = {
      type: "usb.action",
      action: { kind: "bulkOut", id: 1, endpoint: 1, data: Uint8Array.of(1, 2, 3) },
    } satisfies UsbActionMessage;
    expect(getTransferablesForUsbActionMessage(actionMsg)).toEqual([actionMsg.action.data.buffer]);

    // Do not transfer subviews: detaching would detach unrelated bytes from the sender.
    const big = new Uint8Array(16);
    const sub = new Uint8Array(big.buffer, 4, 4);
    const subMsg = {
      type: "usb.action",
      action: { kind: "bulkOut", id: 99, endpoint: 1, data: sub },
    } satisfies UsbActionMessage;
    expect(getTransferablesForUsbActionMessage(subMsg)).toBeUndefined();

    const completionMsg = {
      type: "usb.completion",
      completion: { kind: "bulkIn", id: 2, status: "success", data: Uint8Array.of(9) },
    } satisfies UsbCompletionMessage;
    expect(getTransferablesForUsbCompletionMessage(completionMsg)).toEqual([completionMsg.completion.data.buffer]);

    const stall: UsbCompletionMessage = { type: "usb.completion", completion: { kind: "bulkIn", id: 3, status: "stall" } };
    expect(getTransferablesForUsbCompletionMessage(stall)).toBeUndefined();
  });
});

import { describe, expect, it } from "vitest";

import {
  isUsbActionMessage,
  isUsbCompletionMessage,
  isUsbHostAction,
  isUsbHostCompletion,
  isUsbProxyMessage,
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
      { kind: "controlOut", id: 2, setup, data: Uint8Array.of(1, 2, 3) },
      { kind: "bulkIn", id: 3, endpoint: 1, length: 64 },
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

    // Endpoint is u8, length is non-negative.
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 0x1_00, length: 8 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 1, length: -1 })).toBe(false);
    expect(isUsbHostAction({ kind: "bulkIn", id: 1, endpoint: 1, length: 0xffff_ffff + 1 })).toBe(false);

    // bytesWritten must be a non-negative safe integer.
    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: 1.5 })).toBe(false);
    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: -1 })).toBe(false);
    expect(isUsbHostCompletion({ kind: "bulkOut", id: 1, status: "success", bytesWritten: 0xffff_ffff + 1 })).toBe(false);

    // usb.selected vendor/product IDs are u16.
    expect(
      isUsbProxyMessage({ type: "usb.selected", ok: true, info: { vendorId: 0x1_0000, productId: 1 } }),
    ).toBe(false);
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
      action: { kind: "bulkIn", id: 1, endpoint: 1, length: 8 },
    };
    expect(isUsbActionMessage(actionMsg)).toBe(true);
    expect(isUsbProxyMessage(actionMsg)).toBe(true);

    const completionMsg: UsbCompletionMessage = {
      type: "usb.completion",
      completion: { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1, 2) },
    };
    expect(isUsbCompletionMessage(completionMsg)).toBe(true);
    expect(isUsbProxyMessage(completionMsg)).toBe(true);

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
});

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
      { kind: "bulkIn", id: 3, ep: 1, length: 64 },
      { kind: "bulkOut", id: 4, ep: 2, data: Uint8Array.of(9, 8, 7) },
    ];

    for (const action of actions) {
      expect(isUsbHostAction(action)).toBe(true);
    }

    expect(isUsbHostAction({ kind: "bulkIn", id: "nope" })).toBe(false);
    expect(isUsbHostAction({ kind: "unknown", id: 1 })).toBe(false);
  });

  it("accepts the supported completion shapes", () => {
    const completions: UsbHostCompletion[] = [
      { kind: "okIn", id: 1, data: Uint8Array.of(1) },
      { kind: "okOut", id: 2, bytesWritten: 3 },
      { kind: "stall", id: 3 },
      usbErrorCompletion(4, "nope"),
    ];

    for (const completion of completions) {
      expect(isUsbHostCompletion(completion)).toBe(true);
    }

    expect(isUsbHostCompletion({ kind: "okOut", id: 1, bytesWritten: "bad" })).toBe(false);
    expect(isUsbHostCompletion({ kind: "unknown", id: 1 })).toBe(false);
  });

  it("validates usb.* envelope messages", () => {
    const actionMsg: UsbActionMessage = {
      type: "usb.action",
      action: { kind: "bulkIn", id: 1, ep: 1, length: 8 },
    };
    expect(isUsbActionMessage(actionMsg)).toBe(true);
    expect(isUsbProxyMessage(actionMsg)).toBe(true);

    const completionMsg: UsbCompletionMessage = {
      type: "usb.completion",
      completion: { kind: "okIn", id: 1, data: Uint8Array.of(1, 2) },
    };
    expect(isUsbCompletionMessage(completionMsg)).toBe(true);
    expect(isUsbProxyMessage(completionMsg)).toBe(true);

    expect(isUsbProxyMessage({ type: "usb.action", action: { kind: "bulkIn", id: 1 } })).toBe(false);
    expect(isUsbProxyMessage({ type: "unknown" })).toBe(false);
  });
});


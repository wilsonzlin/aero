import { describe, expect, it } from "vitest";

import { UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT } from "../usb/uhci_external_hub";
import {
  isHidAttachMessage,
  isHidErrorMessage,
  isHidInputReportMessage,
  isHidLogMessage,
  isHidRingAttachMessage,
  isHidRingInitMessage,
  isHidProxyMessage,
  isHidSendReportMessage,
  type HidAttachMessage,
  type HidInputReportMessage,
  type HidRingAttachMessage,
  type HidRingInitMessage,
  type HidSendReportMessage,
} from "./hid_proxy_protocol";

import type { NormalizedHidCollectionInfo } from "./webhid_normalize";

function sampleCollections(): NormalizedHidCollectionInfo[] {
  return [
    {
      usagePage: 0x01,
      usage: 0x06,
      collectionType: 1,
      children: [],
      inputReports: [
        {
          reportId: 1,
          items: [
            {
              usagePage: 0x01,
              usages: [0x30, 0x31],
              usageMinimum: 0,
              usageMaximum: 0,
              reportSize: 8,
              reportCount: 2,
              unitExponent: 0,
              unit: 0,
              logicalMinimum: 0,
              logicalMaximum: 255,
              physicalMinimum: 0,
              physicalMaximum: 255,
              strings: [],
              stringMinimum: 0,
              stringMaximum: 0,
              designators: [],
              designatorMinimum: 0,
              designatorMaximum: 0,
              isAbsolute: true,
              isArray: false,
              isBufferedBytes: false,
              isConstant: false,
              isLinear: true,
              isRange: false,
              isRelative: false,
              isVolatile: false,
              hasNull: false,
              hasPreferredState: true,
              isWrapped: false,
            },
          ],
        },
      ],
      outputReports: [],
      featureReports: [],
    },
  ];
}

describe("hid/hid_proxy_protocol", () => {
  it("validates hid.attach", () => {
    const msg: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      guestPort: 0,
      guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT],
      collections: sampleCollections(),
      hasInterruptOut: false,
    };

    expect(isHidAttachMessage(msg)).toBe(true);
    expect(isHidProxyMessage(msg)).toBe(true);

    expect(
      isHidAttachMessage({
        type: "hid.attach",
        deviceId: 2,
        vendorId: 0x1234,
        productId: 0xabcd,
        guestPort: 0,
        collections: sampleCollections(),
        hasInterruptOut: false,
      }),
    ).toBe(true);

    expect(isHidAttachMessage({ type: "hid.attach", deviceId: 1 })).toBe(false);
    expect(isHidAttachMessage({ ...msg, guestPort: 1 })).toBe(false);
    expect(isHidAttachMessage({ ...msg, guestPath: [] })).toBe(false);
  });

  it("validates hid.inputReport and hid.sendReport", () => {
    const input: HidInputReportMessage = {
      type: "hid.inputReport",
      deviceId: 1,
      reportId: 2,
      data: Uint8Array.of(1, 2, 3),
      tsMs: 123.4,
    };
    expect(isHidInputReportMessage(input)).toBe(true);
    expect(isHidProxyMessage(input)).toBe(true);

    const send: HidSendReportMessage = {
      type: "hid.sendReport",
      deviceId: 1,
      reportType: "output",
      reportId: 7,
      data: Uint8Array.of(9, 8, 7),
    };
    expect(isHidSendReportMessage(send)).toBe(true);
    expect(isHidProxyMessage(send)).toBe(true);

    expect(isHidSendReportMessage({ type: "hid.sendReport", deviceId: 1, reportType: "bad", reportId: 0, data: Uint8Array.of() })).toBe(
      false,
    );

    // SharedArrayBuffer views should be rejected (messages transfer ArrayBuffers).
    if (typeof SharedArrayBuffer !== "undefined") {
      const sab = new SharedArrayBuffer(3);
      const bytes = new Uint8Array(sab);
      expect(isHidInputReportMessage({ ...input, data: bytes } as unknown)).toBe(false);
      expect(isHidSendReportMessage({ ...send, data: bytes } as unknown)).toBe(false);
    }
  });

  it("validates hid.ringAttach", () => {
    const msg: HidRingAttachMessage = {
      type: "hid.ringAttach",
      inputRing: new SharedArrayBuffer(64),
      outputRing: new SharedArrayBuffer(64),
    };
    expect(isHidRingAttachMessage(msg)).toBe(true);
    expect(isHidProxyMessage(msg)).toBe(true);

    expect(isHidRingAttachMessage({ type: "hid.ringAttach", inputRing: new ArrayBuffer(1), outputRing: new SharedArrayBuffer(1) })).toBe(
      false,
    );
  });

  it("validates hid.ring.init", () => {
    const msg: HidRingInitMessage = {
      type: "hid.ring.init",
      sab: new SharedArrayBuffer(64),
      offsetBytes: 0,
    };
    expect(isHidRingInitMessage(msg)).toBe(true);
    expect(isHidProxyMessage(msg)).toBe(true);

    expect(isHidRingInitMessage({ type: "hid.ring.init", sab: new ArrayBuffer(64), offsetBytes: 0 })).toBe(false);
    expect(isHidRingInitMessage({ type: "hid.ring.init", sab: new SharedArrayBuffer(64), offsetBytes: -1 })).toBe(false);
  });

  it("validates optional hid.log/hid.error", () => {
    expect(isHidLogMessage({ type: "hid.log", message: "hello" })).toBe(true);
    expect(isHidErrorMessage({ type: "hid.error", message: "nope", deviceId: 1 })).toBe(true);

    expect(isHidLogMessage({ type: "hid.log", message: 123 })).toBe(false);
    expect(isHidErrorMessage({ type: "hid.error" })).toBe(false);
  });

  it("messages are structured-cloneable", () => {
    const msg: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 7,
      vendorId: 1,
      productId: 2,
      guestPath: [1],
      collections: sampleCollections(),
      hasInterruptOut: true,
    };
    expect(isHidProxyMessage(structuredClone(msg) as unknown)).toBe(true);

    const input: HidInputReportMessage = {
      type: "hid.inputReport",
      deviceId: 7,
      reportId: 1,
      data: Uint8Array.of(1, 2),
    };
    expect(isHidProxyMessage(structuredClone(input) as unknown)).toBe(true);

    const send: HidSendReportMessage = {
      type: "hid.sendReport",
      deviceId: 7,
      reportType: "feature",
      reportId: 3,
      data: Uint8Array.of(4, 5),
    };
    expect(isHidProxyMessage(structuredClone(send) as unknown)).toBe(true);

    const rings: HidRingAttachMessage = {
      type: "hid.ringAttach",
      inputRing: new SharedArrayBuffer(64),
      outputRing: new SharedArrayBuffer(64),
    };
    expect(isHidProxyMessage(structuredClone(rings) as unknown)).toBe(true);

    const ringInit: HidRingInitMessage = {
      type: "hid.ring.init",
      sab: new SharedArrayBuffer(64),
      offsetBytes: 0,
    };
    expect(isHidProxyMessage(structuredClone(ringInit) as unknown)).toBe(true);
  });
});

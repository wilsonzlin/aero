import { describe, expect, it } from "vitest";

import {
  isHidAttachMessage,
  isHidInputReportMessage,
  isHidPassthroughMessage,
  isHidSendReportMessage,
  type HidAttachMessage,
  type HidInputReportMessage,
  type HidSendReportMessage,
} from "./hid_passthrough_protocol";

import type { NormalizedHidCollectionInfo } from "../hid/webhid_normalize";

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

describe("platform/hid_passthrough_protocol", () => {
  it("validates hid:attach (guestPort-only v0 and guestPath v1)", () => {
    const base = {
      type: "hid:attach" as const,
      deviceId: "dev-1",
      vendorId: 0x1234,
      productId: 0xabcd,
      productName: "Demo",
      collections: sampleCollections(),
    };

    const v0: HidAttachMessage = { ...base, guestPort: 0 };
    expect(isHidAttachMessage(v0)).toBe(true);
    expect(isHidPassthroughMessage(v0)).toBe(true);

    const v1: HidAttachMessage = { ...base, guestPath: [0, 1], guestPort: 0 };
    expect(isHidAttachMessage(v1)).toBe(true);
    expect(isHidPassthroughMessage(v1)).toBe(true);

    expect(isHidAttachMessage({ ...base })).toBe(false);
    expect(isHidAttachMessage({ ...v1, guestPath: [] })).toBe(false);
  });

  it("validates hid:inputReport and hid:sendReport ArrayBuffer payloads", () => {
    const input: HidInputReportMessage = {
      type: "hid:inputReport",
      deviceId: "dev-1",
      reportId: 7,
      data: new Uint8Array([1, 2, 3]).buffer,
    };
    expect(isHidInputReportMessage(input)).toBe(true);
    expect(isHidPassthroughMessage(input)).toBe(true);

    const send: HidSendReportMessage = {
      type: "hid:sendReport",
      deviceId: "dev-1",
      reportType: "output",
      reportId: 1,
      data: new Uint8Array([4]).buffer,
    };
    expect(isHidSendReportMessage(send)).toBe(true);
    expect(isHidPassthroughMessage(send)).toBe(true);

    // Views are not accepted (we require ArrayBuffer so it can be transferred).
    expect(isHidInputReportMessage({ ...input, data: new Uint8Array([1]) } as unknown)).toBe(false);
    expect(isHidSendReportMessage({ ...send, data: new Uint8Array([2]) } as unknown)).toBe(false);
  });

  it("messages are structured-cloneable", () => {
    const attach: HidAttachMessage = {
      type: "hid:attach",
      deviceId: "dev-1",
      guestPath: [0, 1],
      vendorId: 1,
      productId: 2,
      collections: sampleCollections(),
    };
    expect(isHidPassthroughMessage(structuredClone(attach) as unknown)).toBe(true);

    const input: HidInputReportMessage = {
      type: "hid:inputReport",
      deviceId: "dev-1",
      reportId: 1,
      data: new Uint8Array([1, 2]).buffer,
    };
    expect(isHidPassthroughMessage(structuredClone(input) as unknown)).toBe(true);

    const send: HidSendReportMessage = {
      type: "hid:sendReport",
      deviceId: "dev-1",
      reportType: "feature",
      reportId: 3,
      data: new Uint8Array([4, 5]).buffer,
    };
    expect(isHidPassthroughMessage(structuredClone(send) as unknown)).toBe(true);
  });
});


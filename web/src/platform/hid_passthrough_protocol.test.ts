import { describe, expect, it } from "vitest";

import {
  isHidAttachMessage,
  isHidDetachMessage,
  isHidFeatureReportResultMessage,
  isHidGetFeatureReportMessage,
  isHidInputReportMessage,
  isHidPassthroughMessage,
  isHidSendReportMessage,
  type HidAttachMessage,
  type HidDetachMessage,
  type HidFeatureReportResultMessage,
  type HidGetFeatureReportMessage,
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

    const withNumeric: HidAttachMessage = { ...v1, numericDeviceId: 123 };
    expect(isHidAttachMessage(withNumeric)).toBe(true);
    expect(isHidPassthroughMessage(withNumeric)).toBe(true);

    expect(isHidAttachMessage({ ...v1, numericDeviceId: -1 })).toBe(false);
    expect(isHidAttachMessage({ ...v1, numericDeviceId: 1.5 })).toBe(false);
    expect(isHidAttachMessage({ ...v1, numericDeviceId: 0x1_0000_0000 })).toBe(false);

    expect(isHidAttachMessage({ ...base })).toBe(false);
    expect(isHidAttachMessage({ ...v1, guestPort: 1 })).toBe(false);
    expect(isHidAttachMessage({ ...v1, guestPath: [] })).toBe(false);
  });

  it("validates hid:detach (optional guestPath/guestPort hints)", () => {
    const bare: HidDetachMessage = { type: "hid:detach", deviceId: "dev-1" };
    expect(isHidDetachMessage(bare)).toBe(true);
    expect(isHidPassthroughMessage(bare)).toBe(true);

    const hinted: HidDetachMessage = { type: "hid:detach", deviceId: "dev-1", guestPath: [0, 1], guestPort: 0 };
    expect(isHidDetachMessage(hinted)).toBe(true);

    expect(isHidDetachMessage({ ...hinted, guestPort: 1 })).toBe(false);
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

    expect(isHidInputReportMessage({ ...input, reportId: -1 })).toBe(false);
    expect(isHidInputReportMessage({ ...input, reportId: 1.5 })).toBe(false);
    expect(isHidInputReportMessage({ ...input, reportId: 256 })).toBe(false);

    const send: HidSendReportMessage = {
      type: "hid:sendReport",
      deviceId: "dev-1",
      reportType: "output",
      reportId: 1,
      data: new Uint8Array([4]).buffer,
    };
    expect(isHidSendReportMessage(send)).toBe(true);
    expect(isHidPassthroughMessage(send)).toBe(true);

    expect(isHidSendReportMessage({ ...send, reportId: -1 })).toBe(false);
    expect(isHidSendReportMessage({ ...send, reportId: 1.5 })).toBe(false);
    expect(isHidSendReportMessage({ ...send, reportId: 256 })).toBe(false);

    // Views are not accepted (we require ArrayBuffer so it can be transferred).
    expect(isHidInputReportMessage({ ...input, data: new Uint8Array([1]) } as unknown)).toBe(false);
    expect(isHidSendReportMessage({ ...send, data: new Uint8Array([2]) } as unknown)).toBe(false);
  });

  it("validates hid:getFeatureReport and hid:featureReportResult messages", () => {
    const get: HidGetFeatureReportMessage = {
      type: "hid:getFeatureReport",
      deviceId: "dev-1",
      requestId: 1,
      reportId: 7,
    };
    expect(isHidGetFeatureReportMessage(get)).toBe(true);
    expect(isHidPassthroughMessage(get)).toBe(true);

    expect(isHidGetFeatureReportMessage({ ...get, reportId: -1 })).toBe(false);
    expect(isHidGetFeatureReportMessage({ ...get, reportId: 1.5 })).toBe(false);
    expect(isHidGetFeatureReportMessage({ ...get, reportId: 256 })).toBe(false);

    const ok: HidFeatureReportResultMessage = {
      type: "hid:featureReportResult",
      deviceId: "dev-1",
      requestId: 1,
      reportId: 7,
      ok: true,
      data: new Uint8Array([1, 2, 3]).buffer,
    };
    expect(isHidFeatureReportResultMessage(ok)).toBe(true);
    expect(isHidPassthroughMessage(ok)).toBe(true);

    expect(isHidFeatureReportResultMessage({ ...ok, reportId: -1 })).toBe(false);
    expect(isHidFeatureReportResultMessage({ ...ok, reportId: 1.5 })).toBe(false);
    expect(isHidFeatureReportResultMessage({ ...ok, reportId: 256 })).toBe(false);

    const err: HidFeatureReportResultMessage = {
      type: "hid:featureReportResult",
      deviceId: "dev-1",
      requestId: 2,
      reportId: 7,
      ok: false,
      error: "boom",
    };
    expect(isHidFeatureReportResultMessage(err)).toBe(true);
    expect(isHidPassthroughMessage(err)).toBe(true);

    expect(isHidFeatureReportResultMessage({ ...ok, data: undefined } as unknown)).toBe(false);
    expect(isHidFeatureReportResultMessage({ ...err, data: new ArrayBuffer(0) } as unknown)).toBe(false);
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

    const get: HidGetFeatureReportMessage = {
      type: "hid:getFeatureReport",
      deviceId: "dev-1",
      requestId: 9,
      reportId: 7,
    };
    expect(isHidPassthroughMessage(structuredClone(get) as unknown)).toBe(true);

    const res: HidFeatureReportResultMessage = {
      type: "hid:featureReportResult",
      deviceId: "dev-1",
      requestId: 9,
      reportId: 7,
      ok: true,
      data: new Uint8Array([1]).buffer,
    };
    expect(isHidPassthroughMessage(structuredClone(res) as unknown)).toBe(true);
  });
});

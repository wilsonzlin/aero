import { describe, expect, it } from "vitest";

import { IoWorkerLegacyHidPassthroughAdapter, computeHasInterruptOut } from "./io_hid_passthrough_legacy_adapter";
import type { NormalizedHidCollectionInfo } from "../hid/webhid_normalize";

describe("workers/IoWorkerLegacyHidPassthroughAdapter", () => {
  it("computes hasInterruptOut based on max output report on-wire size (feature-only does not require interrupt OUT)", () => {
    const withOutput = [
      {
        outputReports: [{ reportId: 1, items: [] }],
        children: [],
      },
    ] as unknown as NormalizedHidCollectionInfo[];
    expect(computeHasInterruptOut(withOutput)).toBe(true);

    const withChildOutput = [
      {
        outputReports: [],
        children: [
          {
            outputReports: [{ reportId: 1, items: [] }],
            children: [],
          },
        ],
      },
    ] as unknown as NormalizedHidCollectionInfo[];
    expect(computeHasInterruptOut(withChildOutput)).toBe(true);

    const withLargeOutput = [
      {
        outputReports: [{ reportId: 0, items: [{ reportSize: 8, reportCount: 65 }] }],
        children: [],
      },
    ] as unknown as NormalizedHidCollectionInfo[];
    expect(computeHasInterruptOut(withLargeOutput)).toBe(false);

    const featureOnly = [
      {
        outputReports: [],
        children: [],
      },
    ] as unknown as NormalizedHidCollectionInfo[];
    expect(computeHasInterruptOut(featureOnly)).toBe(false);
  });

  it("translates legacy hid:attach/hid:detach/hid:inputReport into hid.* messages with stable numeric IDs", () => {
    const adapter = new IoWorkerLegacyHidPassthroughAdapter({ firstDeviceId: 123 });

    const collections = [
      {
        outputReports: [],
        children: [],
      },
    ] as unknown as NormalizedHidCollectionInfo[];

    const attach = adapter.attach({
      type: "hid:attach",
      deviceId: "dev-a",
      guestPort: 0,
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      collections,
    });

    expect(attach).toEqual({
      type: "hid.attach",
      deviceId: 123,
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      guestPath: [0],
      guestPort: 0,
      collections,
      hasInterruptOut: false,
    });

    const inputBuffer = new Uint8Array([1, 2, 3]).buffer;
    const input = adapter.inputReport({ type: "hid:inputReport", deviceId: "dev-a", reportId: 7, data: inputBuffer });
    expect(input).not.toBeNull();
    expect(input!.type).toBe("hid.inputReport");
    expect(input!.deviceId).toBe(123);
    expect(input!.reportId).toBe(7);
    expect(input!.data).toBeInstanceOf(Uint8Array);
    expect(new Uint8Array(input!.data)).toEqual(new Uint8Array([1, 2, 3]));

    const detach = adapter.detach({ type: "hid:detach", deviceId: "dev-a" });
    expect(detach).toEqual({ type: "hid.detach", deviceId: 123 });
    expect(adapter.inputReport({ type: "hid:inputReport", deviceId: "dev-a", reportId: 7, data: inputBuffer })).toBeNull();

    // Re-attach should reuse the same numeric ID.
    const reattach = adapter.attach({
      type: "hid:attach",
      deviceId: "dev-a",
      guestPath: [1, 3],
      vendorId: 0x1234,
      productId: 0x5678,
      collections,
    });
    expect(reattach.deviceId).toBe(123);
    expect(reattach.guestPath).toEqual([1, 3]);
    expect(reattach.guestPort).toBe(1);
  });

  it("honors numericDeviceId when provided by the sender", () => {
    const adapter = new IoWorkerLegacyHidPassthroughAdapter({ firstDeviceId: 100 });

    const collections = [
      {
        outputReports: [],
        children: [],
      },
    ] as unknown as NormalizedHidCollectionInfo[];

    const attachA = adapter.attach({
      type: "hid:attach",
      deviceId: "dev-a",
      numericDeviceId: 200,
      guestPort: 0,
      vendorId: 1,
      productId: 2,
      collections,
    });
    expect(attachA.deviceId).toBe(200);

    const attachB = adapter.attach({
      type: "hid:attach",
      deviceId: "dev-b",
      // Collision: dev-a already reserved 200, so dev-b should get the next free ID.
      numericDeviceId: 200,
      guestPort: 0,
      vendorId: 3,
      productId: 4,
      collections,
    });
    expect(attachB.deviceId).toBe(201);

    const input = adapter.inputReport({ type: "hid:inputReport", deviceId: "dev-a", reportId: 1, data: new ArrayBuffer(0) });
    expect(input?.deviceId).toBe(200);
  });

  it("translates hid.sendReport payloads into legacy hid:sendReport messages", () => {
    const adapter = new IoWorkerLegacyHidPassthroughAdapter({ firstDeviceId: 10 });

    adapter.attach({
      type: "hid:attach",
      deviceId: "dev-a",
      guestPort: 0,
      vendorId: 1,
      productId: 2,
      collections: [] as unknown as NormalizedHidCollectionInfo[],
    });

    const backing = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]).buffer;
    const view = new Uint8Array(backing, 1, 2);

    const msg = adapter.sendReport({ deviceId: 10, reportType: "output", reportId: 1, data: view });
    expect(msg).not.toBeNull();
    expect(msg!.type).toBe("hid:sendReport");
    expect(msg!.deviceId).toBe("dev-a");
    expect(msg!.reportType).toBe("output");
    expect(msg!.reportId).toBe(1);
    expect(new Uint8Array(msg!.data)).toEqual(new Uint8Array([0xbb, 0xcc]));
  });

  it("hard-caps oversized hid.sendReport payloads when translating to legacy hid:sendReport messages", () => {
    const adapter = new IoWorkerLegacyHidPassthroughAdapter({ firstDeviceId: 10 });

    adapter.attach({
      type: "hid:attach",
      deviceId: "dev-a",
      guestPort: 0,
      vendorId: 1,
      productId: 2,
      collections: [] as unknown as NormalizedHidCollectionInfo[],
    });

    const shared = new SharedArrayBuffer(1024 * 1024);
    const view = new Uint8Array(shared);
    view.set([1, 2, 3], 0);

    const msg = adapter.sendReport({ deviceId: 10, reportType: "feature", reportId: 9, data: view });
    expect(msg).not.toBeNull();
    expect(msg!.type).toBe("hid:sendReport");
    // reportId != 0 => on-wire report includes a reportId prefix byte, so clamp payload to 0xfffe.
    expect(new Uint8Array(msg!.data).byteLength).toBe(0xfffe);
    expect(Array.from(new Uint8Array(msg!.data).slice(0, 3))).toEqual([1, 2, 3]);
  });

  it("translates hid.getFeatureReport payloads into legacy hid:getFeatureReport and translates results back", () => {
    const adapter = new IoWorkerLegacyHidPassthroughAdapter({ firstDeviceId: 10 });

    adapter.attach({
      type: "hid:attach",
      deviceId: "dev-a",
      guestPort: 0,
      vendorId: 1,
      productId: 2,
      collections: [] as unknown as NormalizedHidCollectionInfo[],
    });

    const req = adapter.getFeatureReport({ deviceId: 10, requestId: 1, reportId: 7 });
    expect(req).not.toBeNull();
    expect(req!.type).toBe("hid:getFeatureReport");
    expect(req!.deviceId).toBe("dev-a");
    expect(req!.requestId).toBe(1);
    expect(req!.reportId).toBe(7);

    const ok = adapter.featureReportResult({
      type: "hid:featureReportResult",
      deviceId: "dev-a",
      requestId: 1,
      reportId: 7,
      ok: true,
      data: new Uint8Array([1, 2, 3]).buffer,
    });
    expect(ok).not.toBeNull();
    expect(ok!.type).toBe("hid.featureReportResult");
    expect(ok!.deviceId).toBe(10);
    expect(ok!.ok).toBe(true);
    expect(Array.from(ok!.data ?? [])).toEqual([1, 2, 3]);

    const err = adapter.featureReportResult({
      type: "hid:featureReportResult",
      deviceId: "dev-a",
      requestId: 2,
      reportId: 7,
      ok: false,
      error: "boom",
    });
    expect(err).not.toBeNull();
    expect(err!.ok).toBe(false);
    expect(err!.error).toBe("boom");
  });
});

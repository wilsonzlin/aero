import { describe, expect, it, vi } from "vitest";

import type { HidAttachMessage, HidInputReportMessage, HidSendReportMessage } from "../hid/hid_proxy_protocol";
import type { NormalizedHidCollectionInfo } from "../hid/webhid_normalize";
import type { WasmApi } from "../runtime/wasm_context";
import { IoWorkerHidPassthrough } from "./io_hid_passthrough";

describe("workers/IoWorkerHidPassthrough", () => {
  const collections: NormalizedHidCollectionInfo[] = [
    {
      usagePage: 1,
      usage: 2,
      collectionType: 1, // application
      children: [],
      inputReports: [],
      outputReports: [],
      featureReports: [],
    },
  ];

  it("hid.attach constructs a bridge; hid.inputReport forwards; tick drains output reports", () => {
    const ctorSpy = vi.fn();
    const synthesizeSpy = vi.fn(() => new Uint8Array([1, 2, 3]));
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
        bridgeInstance = this;
      }
    }

    const posted: Array<{ msg: HidSendReportMessage; transfer: Transferable[] }> = [];

    const wasm = {
      // Minimal subset required by the IoWorkerHidPassthrough helper.
      synthesize_webhid_report_descriptor: synthesizeSpy,
      UsbHidPassthroughBridge: FakeBridge,
    } as unknown as WasmApi;

    const mgr = new IoWorkerHidPassthrough(wasm, (msg, transfer) => posted.push({ msg, transfer }));

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 123,
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      collections,
      hasInterruptOut: true,
    };
    mgr.attach(attach);

    expect(ctorSpy).toHaveBeenCalledTimes(1);
    expect(ctorSpy).toHaveBeenCalledWith(
      attach.vendorId,
      attach.productId,
      undefined,
      attach.productName,
      undefined,
      synthesizeSpy.mock.results[0]!.value,
      attach.hasInterruptOut,
      undefined,
      undefined,
    );
    expect(synthesizeSpy).toHaveBeenCalledTimes(1);
    expect(synthesizeSpy).toHaveBeenCalledWith(attach.collections);

    const input: HidInputReportMessage = {
      type: "hid.inputReport",
      deviceId: attach.deviceId,
      reportId: 7,
      data: new Uint8Array([9, 10]) as Uint8Array<ArrayBuffer>,
    };
    mgr.inputReport(input);

    expect(bridgeInstance).not.toBeNull();
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledTimes(1);
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledWith(input.reportId, input.data);

    const outData = new Uint8Array([0xaa, 0xbb]) as Uint8Array<ArrayBuffer>;
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce({ reportType: "output", reportId: 1, data: outData });
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce(null);

    mgr.tick();

    expect(posted).toHaveLength(1);
    expect(posted[0]!.msg).toEqual({
      type: "hid.sendReport",
      deviceId: attach.deviceId,
      reportType: "output",
      reportId: 1,
      data: outData,
    });
    expect(posted[0]!.transfer).toEqual([outData.buffer]);
  });

  it("falls back to WebHidPassthroughBridge when synthesize/UsbHidPassthroughBridge are unavailable", () => {
    const ctorSpy = vi.fn();
    let bridgeInstance: FakeBridge | null = null;

    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();

      constructor(...args: unknown[]) {
        ctorSpy(...args);
        bridgeInstance = this;
      }
    }

    const posted: Array<{ msg: HidSendReportMessage; transfer: Transferable[] }> = [];

    const wasm = {
      WebHidPassthroughBridge: FakeBridge,
    } as unknown as WasmApi;

    const mgr = new IoWorkerHidPassthrough(wasm, (msg, transfer) => posted.push({ msg, transfer }));

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 123,
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      collections,
      hasInterruptOut: true,
    };
    mgr.attach(attach);

    expect(ctorSpy).toHaveBeenCalledTimes(1);
    expect(ctorSpy).toHaveBeenCalledWith(
      attach.vendorId,
      attach.productId,
      undefined,
      attach.productName,
      undefined,
      attach.collections,
    );

    const input: HidInputReportMessage = {
      type: "hid.inputReport",
      deviceId: attach.deviceId,
      reportId: 7,
      data: new Uint8Array([9, 10]) as Uint8Array<ArrayBuffer>,
    };
    mgr.inputReport(input);

    expect(bridgeInstance).not.toBeNull();
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledTimes(1);
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledWith(input.reportId, input.data);

    const outData = new Uint8Array([0xaa, 0xbb]) as Uint8Array<ArrayBuffer>;
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce({ reportType: "output", reportId: 1, data: outData });
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce(null);

    mgr.tick();

    expect(posted).toHaveLength(1);
    expect(posted[0]!.msg).toEqual({
      type: "hid.sendReport",
      deviceId: attach.deviceId,
      reportType: "output",
      reportId: 1,
      data: outData,
    });
    expect(posted[0]!.transfer).toEqual([outData.buffer]);
  });

  it("clamps oversized input reports before forwarding to push_input_report", () => {
    let bridgeInstance: FakeBridge | null = null;
    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn(() => null);
      readonly configured = vi.fn(() => false);
      readonly free = vi.fn();
      constructor() {
        bridgeInstance = this;
      }
    }

    const wasm = { WebHidPassthroughBridge: FakeBridge } as unknown as WasmApi;
    const mgr = new IoWorkerHidPassthrough(wasm, () => {});

    mgr.attach({
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0x5678,
      collections: [],
      hasInterruptOut: false,
    });

    const huge = new Uint8Array(1024 * 1024);
    huge.set([1, 2, 3], 0);
    mgr.inputReport({
      type: "hid.inputReport",
      deviceId: 1,
      reportId: 7,
      data: huge as Uint8Array<ArrayBuffer>,
    });

    expect(bridgeInstance).not.toBeNull();
    expect(bridgeInstance!.push_input_report).toHaveBeenCalledTimes(1);
    const payload = bridgeInstance!.push_input_report.mock.calls[0]![1] as Uint8Array;
    expect(payload.byteLength).toBe(64);
    expect(Array.from(payload.slice(0, 3))).toEqual([1, 2, 3]);
  });

  it("clamps and copies oversized output report payloads before posting", () => {
    let bridgeInstance: FakeBridge | null = null;
    class FakeBridge {
      readonly push_input_report = vi.fn();
      readonly drain_next_output_report = vi.fn();
      readonly configured = vi.fn(() => true);
      readonly free = vi.fn();
      constructor() {
        bridgeInstance = this;
      }
    }

    const posted: Array<{ msg: HidSendReportMessage; transfer: Transferable[] }> = [];
    const wasm = { WebHidPassthroughBridge: FakeBridge } as unknown as WasmApi;
    const mgr = new IoWorkerHidPassthrough(wasm, (msg, transfer) => posted.push({ msg, transfer }));

    mgr.attach({
      type: "hid.attach",
      deviceId: 1,
      vendorId: 0x1234,
      productId: 0x5678,
      collections: [],
      hasInterruptOut: false,
    });

    expect(bridgeInstance).not.toBeNull();
    const backing = new Uint8Array(0xffff + 128);
    backing.set([1, 2, 3], 64);
    const view = backing.subarray(64, 64 + 0xffff);
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce({ reportType: "output", reportId: 9, data: view });
    bridgeInstance!.drain_next_output_report.mockReturnValueOnce(null);

    mgr.tick();

    expect(posted).toHaveLength(1);
    const { msg, transfer } = posted[0]!;
    expect(msg.type).toBe("hid.sendReport");
    expect(msg.deviceId).toBe(1);
    expect(msg.reportType).toBe("output");
    expect(msg.reportId).toBe(9);
    // reportId != 0 => on-wire report includes a reportId prefix byte, so clamp payload to 0xfffe.
    expect(msg.data.byteLength).toBe(0xfffe);
    expect(Array.from(msg.data.slice(0, 3))).toEqual([1, 2, 3]);
    // Ensure we don't transfer the entire backing buffer.
    expect(msg.data.buffer).not.toBe(backing.buffer);
    expect(msg.data.byteOffset).toBe(0);
    expect(msg.data.byteLength).toBe(msg.data.buffer.byteLength);
    expect(transfer).toEqual([msg.data.buffer]);
  });
});

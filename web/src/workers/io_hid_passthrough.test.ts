import { describe, expect, it, vi } from "vitest";

import type { HidAttachMessage, HidInputReportMessage, HidSendReportMessage } from "../hid/hid_proxy_protocol";
import type { WasmApi } from "../runtime/wasm_context";
import { IoWorkerHidPassthrough } from "./io_hid_passthrough";

describe("workers/IoWorkerHidPassthrough", () => {
  it("hid.attach constructs a bridge; hid.inputReport forwards; tick drains output reports", () => {
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

    const posted: HidSendReportMessage[] = [];

    const wasm = {
      // Minimal subset required by the IoWorkerHidPassthrough helper.
      WebHidPassthroughBridge: FakeBridge,
    } as unknown as WasmApi;

    const mgr = new IoWorkerHidPassthrough(wasm, (msg) => posted.push(msg));

    const attach: HidAttachMessage = {
      type: "hid.attach",
      deviceId: 123,
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      collections: [{ some: "collection" }] as any,
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

    expect(posted).toEqual([
      {
        type: "hid.sendReport",
        deviceId: attach.deviceId,
        reportType: "output",
        reportId: 1,
        data: outData,
      },
    ]);
  });
});

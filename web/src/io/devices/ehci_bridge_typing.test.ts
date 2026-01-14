import { describe, expect, it } from "vitest";

import type { WasmApi } from "../../runtime/wasm_context";

import type { EhciControllerBridgeLike } from "./ehci";

describe("io/devices/ehci bridge typing", () => {
  it("accepts the typed WASM bridge without `as any` casts", () => {
    type Bridge = InstanceType<NonNullable<WasmApi["EhciControllerBridge"]>>;

    type AssertAssignableToPci = Bridge extends EhciControllerBridgeLike ? true : never;
    type AssertHasTopologyHelpers = Bridge extends {
      readonly guest_base: number;
      readonly guest_size: number;

      attach_hub(rootPort: number, portCount: number): void;
      detach_at_path(path: number[]): void;
      attach_webhid_device(path: number[], device: InstanceType<WasmApi["WebHidPassthroughBridge"]>): void;
      attach_usb_hid_passthrough_device(
        path: number[],
        device: InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>,
      ): void;
    }
      ? true
      : never;

    // Compile-time checks: if the WasmApi typing regresses and the bridge is no longer assignable to
    // the PCI wrapper's expected interface (or loses topology helpers), this assignment will fail.
    const okPci: AssertAssignableToPci = true;
    const okTopology: AssertHasTopologyHelpers = true;
    expect(okPci).toBe(true);
    expect(okTopology).toBe(true);
  });
});


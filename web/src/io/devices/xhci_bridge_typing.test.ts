import { describe, expect, it } from "vitest";

import type { WasmApi } from "../../runtime/wasm_context";
import type { XhciControllerBridgeLike } from "./xhci";

describe("io/devices/xhci bridge typing", () => {
  it("accepts the typed WASM bridge without `as any` casts", () => {
    type Bridge = InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;
    type AssertAssignable = Bridge extends XhciControllerBridgeLike ? true : never;

    // Compile-time check: if the WasmApi typing regresses and the bridge is no longer assignable to
    // the PCI wrapper's expected interface, this assignment will fail to type-check.
    const ok: AssertAssignable = true;
    expect(ok).toBe(true);
  });
});


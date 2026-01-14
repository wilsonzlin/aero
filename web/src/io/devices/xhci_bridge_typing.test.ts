import { describe, expect, it } from "vitest";

import type { WasmApi } from "../../runtime/wasm_context";
import type { XhciTopologyBridge } from "../../hid/xhci_hid_topology";
import type { XhciControllerBridgeLike } from "./xhci";

describe("io/devices/xhci bridge typing", () => {
  it("accepts the typed WASM bridge without `as any` casts", () => {
    type Bridge = InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;
    type AssertAssignableToPci = Bridge extends XhciControllerBridgeLike ? true : never;
    type AssertAssignableToTopology = Bridge extends XhciTopologyBridge ? true : never;

    // Compile-time check: if the WasmApi typing regresses and the bridge is no longer assignable to
    // the PCI wrapper's expected interface, this assignment will fail to type-check.
    const okPci: AssertAssignableToPci = true;
    const okTopology: AssertAssignableToTopology = true;
    expect(okPci).toBe(true);
    expect(okTopology).toBe(true);
  });
});

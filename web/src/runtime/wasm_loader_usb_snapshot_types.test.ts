import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (USB snapshot typings)", () => {
  it("requires feature detection for optional snapshot methods", () => {
    type WebUsbBridge = InstanceType<NonNullable<WasmApi["WebUsbUhciBridge"]>>;
    type Runtime = InstanceType<NonNullable<WasmApi["UhciRuntime"]>>;
    type UhciBridge = InstanceType<NonNullable<WasmApi["UhciControllerBridge"]>>;

    const webusb = {} as WebUsbBridge;
    const runtime = {} as Runtime;
    const uhci = {} as UhciBridge;

    // Optional methods should require feature detection under `strictNullChecks`.
    //
    // These are type-level assertions only: Vitest does not typecheck TS during execution, so we
    // must avoid running the calls at runtime (the objects are just `{}` stubs here).
    //
    // The `@ts-expect-error` annotations are still validated by `tsc` (see `npm -w web run typecheck`).
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error snapshot_state may be undefined
      webusb.snapshot_state();
      // @ts-expect-error restore_state may be undefined
      webusb.restore_state(new Uint8Array());

      // @ts-expect-error snapshot_state may be undefined
      runtime.snapshot_state();
      // @ts-expect-error restore_state may be undefined
      runtime.restore_state(new Uint8Array());

      // @ts-expect-error snapshot_state may be undefined
      uhci.snapshot_state();
      // @ts-expect-error restore_state may be undefined
      uhci.restore_state(new Uint8Array());
    }
    void assertStrictNullChecksEnforced;

    if (webusb.snapshot_state && webusb.restore_state) {
      const bytes = webusb.snapshot_state();
      webusb.restore_state(bytes);
    }
    if (runtime.snapshot_state && runtime.restore_state) {
      const bytes = runtime.snapshot_state();
      runtime.restore_state(bytes);
    }
    if (uhci.snapshot_state && uhci.restore_state) {
      const bytes = uhci.snapshot_state();
      uhci.restore_state(bytes);
    }

    expect(true).toBe(true);
  });
});

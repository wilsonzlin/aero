import { describe, expect, it } from "vitest";

import type { UsbHostAction } from "../usb/webusb_backend";
import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (UhciRuntime WebUSB drain typings)", () => {
  it("requires null-handling for UhciRuntime.webusb_drain_actions()", () => {
    type Runtime = InstanceType<NonNullable<WasmApi["UhciRuntime"]>>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // a concrete implementation to avoid `undefined is not a function` crashes. The compile-time
    // checks are encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const runtime = {
      webusb_drain_actions: () => null,
    } as unknown as Runtime;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error webusb_drain_actions can return null
      const _actions: UsbHostAction[] = runtime.webusb_drain_actions();
      void _actions;
    }
    void assertStrictNullChecksEnforced;

    const drained = runtime.webusb_drain_actions();
    expect(drained).toBeNull();
  });
});


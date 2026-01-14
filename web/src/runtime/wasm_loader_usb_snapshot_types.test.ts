import { describe, expect, it } from "vitest";

import type { UsbHostCompletion } from "../usb/webusb_backend";
import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (USB snapshot typings)", () => {
  it("requires feature detection for optional snapshot methods", () => {
    type WebUsbBridge = InstanceType<NonNullable<WasmApi["WebUsbUhciBridge"]>>;
    type Runtime = InstanceType<NonNullable<WasmApi["UhciRuntime"]>>;
    type UhciBridge = InstanceType<NonNullable<WasmApi["UhciControllerBridge"]>>;
    type EhciBridge = InstanceType<NonNullable<WasmApi["EhciControllerBridge"]>>;
    type XhciBridge = InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const webusb = { snapshot_state: () => new Uint8Array(), restore_state: (_bytes: Uint8Array) => {} } as unknown as WebUsbBridge;
    const runtime = { snapshot_state: () => new Uint8Array(), restore_state: (_bytes: Uint8Array) => {} } as unknown as Runtime;
    const uhci = { snapshot_state: () => new Uint8Array(), restore_state: (_bytes: Uint8Array) => {} } as unknown as UhciBridge;
    const ehci = { snapshot_state: () => new Uint8Array(), restore_state: (_bytes: Uint8Array) => {} } as unknown as EhciBridge;
    const xhci = { snapshot_state: () => new Uint8Array(), restore_state: (_bytes: Uint8Array) => {} } as unknown as XhciBridge;

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

      // @ts-expect-error snapshot_state may be undefined
      ehci.snapshot_state();
      // @ts-expect-error restore_state may be undefined
      ehci.restore_state(new Uint8Array());

      // @ts-expect-error snapshot_state may be undefined
      xhci.snapshot_state();
      // @ts-expect-error restore_state may be undefined
      xhci.restore_state(new Uint8Array());
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
    if (ehci.snapshot_state && ehci.restore_state) {
      const bytes = ehci.snapshot_state();
      ehci.restore_state(bytes);
    }
    if (xhci.snapshot_state && xhci.restore_state) {
      const bytes = xhci.snapshot_state();
      xhci.restore_state(bytes);
    }

    expect(true).toBe(true);
  });

  it("exposes xHCI topology + WebUSB helpers as optional (back-compat)", () => {
    type XhciBridge = InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking; keep runtime
    // values concrete and use `@ts-expect-error` for compile-time assertions (validated by `tsc`).
    const xhci = {} as unknown as XhciBridge;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error attach_hub may be undefined
      xhci.attach_hub(0, 8);
      // @ts-expect-error detach_at_path may be undefined
      xhci.detach_at_path([0]);
      // @ts-expect-error attach_webhid_device may be undefined
      xhci.attach_webhid_device([0], {});
      // @ts-expect-error attach_usb_hid_passthrough_device may be undefined
      xhci.attach_usb_hid_passthrough_device([0], {});

      // @ts-expect-error set_connected may be undefined
      xhci.set_connected(true);
      // @ts-expect-error drain_actions may be undefined
      xhci.drain_actions();
      // @ts-expect-error push_completion may be undefined
      xhci.push_completion({ kind: "controlIn", id: 0, status: "stall" } satisfies UsbHostCompletion);
      // @ts-expect-error reset may be undefined
      xhci.reset();
      // @ts-expect-error pending_summary may be undefined
      xhci.pending_summary();
    }
    void assertStrictNullChecksEnforced;

    // Optional chaining should compile (method exists in the type) and be safe at runtime when
    // running against older WASM builds that do not export these helpers.
    xhci.attach_hub?.(0, 8);
    xhci.detach_at_path?.([0]);
    xhci.attach_webhid_device?.([0], {});
    xhci.attach_usb_hid_passthrough_device?.([0], {});
    xhci.set_connected?.(true);
    void xhci.drain_actions?.();
    xhci.push_completion?.({ kind: "controlIn", id: 0, status: "stall" });
    xhci.reset?.();
    void xhci.pending_summary?.();

    expect(true).toBe(true);
  });
});

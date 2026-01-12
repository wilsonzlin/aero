import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (E1000 snapshot typings)", () => {
  it("requires feature detection for optional snapshot methods", () => {
    type E1000Bridge = InstanceType<NonNullable<WasmApi["E1000Bridge"]>>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const e1000 = {
      mmio_read: () => 0,
      mmio_write: () => {},
      io_read: () => 0,
      io_write: () => {},
      poll: () => {},
      receive_frame: () => {},
      pop_tx_frame: () => undefined,
      irq_level: () => false,
      save_state: () => new Uint8Array(),
      load_state: (_bytes: Uint8Array) => {},
      snapshot_state: () => new Uint8Array(),
      restore_state: (_bytes: Uint8Array) => {},
      free: () => {},
    } as unknown as E1000Bridge;

    function assertStrictNullChecksEnforced() {
      // Optional methods should require feature detection under `strictNullChecks`.
      // @ts-expect-error save_state may be undefined
      e1000.save_state();
      // @ts-expect-error load_state may be undefined
      e1000.load_state(new Uint8Array());
      // @ts-expect-error snapshot_state may be undefined
      e1000.snapshot_state();
      // @ts-expect-error restore_state may be undefined
      e1000.restore_state(new Uint8Array());
    }
    void assertStrictNullChecksEnforced;

    if (e1000.save_state && e1000.load_state) {
      const bytes = e1000.save_state();
      e1000.load_state(bytes);
    }
    if (e1000.snapshot_state && e1000.restore_state) {
      const bytes = e1000.snapshot_state();
      e1000.restore_state(bytes);
    }

    expect(true).toBe(true);
  });
});


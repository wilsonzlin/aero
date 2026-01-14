import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine scanout-state typings)", () => {
  it("requires feature detection for optional scanout state exports", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      scanout_state_ptr: () => 0,
      scanout_state_len_bytes: () => 32,
    } as unknown as Machine;

    // Optional methods should require feature detection under `strictNullChecks`.
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error scanout_state_ptr may be undefined
      machine.scanout_state_ptr();
      // @ts-expect-error scanout_state_len_bytes may be undefined
      machine.scanout_state_len_bytes();
    }
    void assertStrictNullChecksEnforced;

    if (machine.scanout_state_ptr && machine.scanout_state_len_bytes) {
      const ptr = machine.scanout_state_ptr();
      const len = machine.scanout_state_len_bytes();
      expect(ptr).toBeGreaterThanOrEqual(0);
      expect(len).toBeGreaterThan(0);
    }
  });
});


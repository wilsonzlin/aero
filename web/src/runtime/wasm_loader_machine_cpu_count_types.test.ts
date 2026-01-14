import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine cpu_count typings)", () => {
  it("requires feature detection for optional cpu_count APIs", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;
    type MachineCtor = WasmApi["Machine"];

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      cpu_count: () => 1,
    } as unknown as Machine;

    const machineCtor = {
      new_with_cpu_count: (_ramSizeBytes: number, _cpuCount: number) => machine,
    } as unknown as MachineCtor;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error cpu_count may be undefined
      machine.cpu_count();
      // @ts-expect-error new_with_cpu_count may be undefined
      machineCtor.new_with_cpu_count(2 * 1024 * 1024, 2);
    }
    void assertStrictNullChecksEnforced;

    if (machine.cpu_count) {
      expect(machine.cpu_count()).toBeGreaterThanOrEqual(1);
    }

    if (machineCtor.new_with_cpu_count) {
      machineCtor.new_with_cpu_count(2 * 1024 * 1024, 2);
    }
  });
});


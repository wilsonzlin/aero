import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine constructor typings)", () => {
  it("requires feature detection for optional Machine.new_with_config", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;
    type MachineCtor = WasmApi["Machine"];

    const machine = {
      free: () => {},
    } as unknown as Machine;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete values to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machineCtor = {
      new_with_config: (
        _ramSizeBytes: number,
        _enableAerogpu: boolean,
        _enableVga?: boolean,
        _cpuCount?: number,
      ) => machine,
    } as unknown as MachineCtor;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error new_with_config may be undefined
      machineCtor.new_with_config(2 * 1024 * 1024, true);
      // @ts-expect-error enableAerogpu must be boolean
      machineCtor.new_with_config?.(2 * 1024 * 1024, 1);
      // @ts-expect-error enableVga must be boolean
      machineCtor.new_with_config?.(2 * 1024 * 1024, true, 1);
      // @ts-expect-error cpuCount must be number
      machineCtor.new_with_config?.(2 * 1024 * 1024, true, undefined, "2");
    }
    void assertStrictNullChecksEnforced;

    if (machineCtor.new_with_config) {
      const m = machineCtor.new_with_config(2 * 1024 * 1024, true, undefined, 2);
      m.free();
    }

    expect(true).toBe(true);
  });
});

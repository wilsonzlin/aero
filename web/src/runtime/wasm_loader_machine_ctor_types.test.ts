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

  it("requires feature detection for optional Machine.new_with_input_backends", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;
    type MachineCtor = WasmApi["Machine"];

    const machine = {
      free: () => {},
    } as unknown as Machine;

    const machineCtor = {
      new_with_input_backends: (
        _ramSizeBytes: number,
        _enableVirtioInput: boolean,
        _enableSyntheticUsbHid: boolean,
      ) => machine,
    } as unknown as MachineCtor;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error new_with_input_backends may be undefined
      machineCtor.new_with_input_backends(2 * 1024 * 1024, true, true);
      // @ts-expect-error enableVirtioInput must be boolean
      machineCtor.new_with_input_backends?.(2 * 1024 * 1024, 1, true);
      // @ts-expect-error enableSyntheticUsbHid must be boolean
      machineCtor.new_with_input_backends?.(2 * 1024 * 1024, true, "1");
    }
    void assertStrictNullChecksEnforced;

    if (machineCtor.new_with_input_backends) {
      const m = machineCtor.new_with_input_backends(2 * 1024 * 1024, true, false);
      m.free();
    }

    expect(true).toBe(true);
  });

  it("requires feature detection for optional Machine.new_with_options", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;
    type MachineCtor = WasmApi["Machine"];

    const machine = {
      free: () => {},
    } as unknown as Machine;

    const machineCtor = {
      new_with_options: (_ramSizeBytes: number, _options?: unknown) => machine,
    } as unknown as MachineCtor;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error new_with_options may be undefined
      machineCtor.new_with_options(2 * 1024 * 1024);
      // @ts-expect-error ramSizeBytes must be a number
      machineCtor.new_with_options?.("2");
      // @ts-expect-error options must be an object/null/undefined
      machineCtor.new_with_options?.(2 * 1024 * 1024, 1);
      // @ts-expect-error enable_aerogpu must be boolean
      machineCtor.new_with_options?.(2 * 1024 * 1024, { enable_aerogpu: 1 });
    }
    void assertStrictNullChecksEnforced;

    if (machineCtor.new_with_options) {
      const m = machineCtor.new_with_options(2 * 1024 * 1024, { enable_aerogpu: true });
      m.free();
    }

    expect(true).toBe(true);
  });

  it("requires feature detection for optional Machine.new_shared_with_config", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;
    type MachineCtor = WasmApi["Machine"];

    const machine = {
      free: () => {},
    } as unknown as Machine;

    const machineCtor = {
      new_shared_with_config: (
        _guestBase: number,
        _guestSize: number,
        _enableAerogpu: boolean,
        _enableVga?: boolean,
        _cpuCount?: number,
      ) => machine,
    } as unknown as MachineCtor;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error new_shared_with_config may be undefined
      machineCtor.new_shared_with_config(0, 0, true);
      // @ts-expect-error guestBase must be number
      machineCtor.new_shared_with_config?.("0", 0, true);
      // @ts-expect-error enableAerogpu must be boolean
      machineCtor.new_shared_with_config?.(0, 0, 1);
      // @ts-expect-error enableVga must be boolean
      machineCtor.new_shared_with_config?.(0, 0, true, 1);
      // @ts-expect-error cpuCount must be number
      machineCtor.new_shared_with_config?.(0, 0, true, undefined, "2");
    }
    void assertStrictNullChecksEnforced;

    if (machineCtor.new_shared_with_config) {
      const m = machineCtor.new_shared_with_config(0, 1024, true, undefined, 1);
      m.free();
    }

    expect(true).toBe(true);
  });
});

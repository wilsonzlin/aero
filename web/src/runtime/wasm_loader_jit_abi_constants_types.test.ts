import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (jit_abi_constants typings)", () => {
  it("includes optional code version table offsets", () => {
    type JitAbiConstants = ReturnType<NonNullable<WasmApi["jit_abi_constants"]>>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // a concrete value to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const c = {} as unknown as JitAbiConstants;

    // These offsets are optional because older wasm builds did not export them.
    const ptrOff: number | undefined = c.code_version_table_ptr_offset;
    const lenOff: number | undefined = c.code_version_table_len_offset;
    void ptrOff;
    void lenOff;

    // @ts-expect-error offset may be undefined
    const _ptrStrict: number = c.code_version_table_ptr_offset;
    // @ts-expect-error offset may be undefined
    const _lenStrict: number = c.code_version_table_len_offset;
    void _ptrStrict;
    void _lenStrict;

    // Runtime value is not meaningful here; this is a typing/contract test.
    expect(c.code_version_table_ptr_offset).toBeUndefined();
    expect(c.code_version_table_len_offset).toBeUndefined();
  });
});


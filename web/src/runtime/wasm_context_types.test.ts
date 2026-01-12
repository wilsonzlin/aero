import { describe, expect, it } from "vitest";

import type { WasmInitResult } from "./wasm_loader";

describe("runtime/wasm_context typings", () => {
  it("exposes wasmMemory in initWasmForContext return type", () => {
    type InitFn = typeof import("./wasm_context").initWasmForContext;
    type Result = Awaited<ReturnType<InitFn>>;

    // Compile-time checks (validated by `tsc` in CI).
    const result = {} as Result;
    const memory: WebAssembly.Memory | undefined = result.wasmMemory;
    void memory;

    const _assert: WasmInitResult = result;
    void _assert;

    // Runtime no-op; Vitest does not typecheck.
    expect(true).toBe(true);
  });
});


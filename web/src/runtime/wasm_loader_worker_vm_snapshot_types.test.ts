import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (WorkerVmSnapshot typings)", () => {
  it("requires feature detection for the optional WorkerVmSnapshot export", () => {
    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete values to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const api = {} as WasmApi;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error WorkerVmSnapshot may be undefined
      // eslint-disable-next-line @typescript-eslint/no-unused-expressions
      new api.WorkerVmSnapshot(0, 0);
    }
    void assertStrictNullChecksEnforced;

    if (api.WorkerVmSnapshot) {
      const builder = new api.WorkerVmSnapshot(0, 0);
      builder.set_cpu_state_v2(new Uint8Array(), new Uint8Array());
      builder.add_device_state(12, 1, 0, new Uint8Array());
      void builder.snapshot_full_to_opfs("snapshot.bin");
      void builder.restore_snapshot_from_opfs("snapshot.bin");
      builder.free();
    }

    expect(true).toBe(true);
  });
});


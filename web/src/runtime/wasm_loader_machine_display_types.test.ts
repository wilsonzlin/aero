import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine display scanout typings)", () => {
  it("requires feature detection for optional display scanout methods", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      display_present: () => {},
      display_width: () => 800,
      display_height: () => 600,
      display_stride_bytes: () => 800 * 4,
      display_framebuffer_ptr: () => 0,
      display_framebuffer_len_bytes: () => 0,
      display_framebuffer_copy_rgba8888: () => new Uint8Array(),
    } as unknown as Machine;

    // Optional methods should require feature detection under `strictNullChecks`.
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error display_present may be undefined
      machine.display_present();
      // @ts-expect-error display_width may be undefined
      machine.display_width();
      // @ts-expect-error display_height may be undefined
      machine.display_height();
      // @ts-expect-error display_stride_bytes may be undefined
      machine.display_stride_bytes();
      // @ts-expect-error display_framebuffer_ptr may be undefined
      machine.display_framebuffer_ptr();
      // @ts-expect-error display_framebuffer_len_bytes may be undefined
      machine.display_framebuffer_len_bytes();
      // @ts-expect-error display_framebuffer_copy_rgba8888 may be undefined
      machine.display_framebuffer_copy_rgba8888();
    }
    void assertStrictNullChecksEnforced;

    if (machine.display_present) {
      machine.display_present();
    }
    if (machine.display_width && machine.display_height && machine.display_stride_bytes) {
      const w = machine.display_width();
      const h = machine.display_height();
      const stride = machine.display_stride_bytes();
      expect(stride).toBe(w * 4);
      expect(w).toBeGreaterThan(0);
      expect(h).toBeGreaterThan(0);
    }
    if (machine.display_framebuffer_copy_rgba8888) {
      const bytes = machine.display_framebuffer_copy_rgba8888();
      expect(bytes).toBeInstanceOf(Uint8Array);
    }
  });
});


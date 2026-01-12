import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine VGA typings)", () => {
  it("requires feature detection for optional VGA scanout methods", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      vga_present: () => {},
      vga_width: () => 720,
      vga_height: () => 400,
      vga_stride_bytes: () => 720 * 4,
      vga_framebuffer_ptr: () => 0,
      vga_framebuffer_len_bytes: () => 0,
      vga_framebuffer_copy_rgba8888: () => new Uint8Array(),
      vga_framebuffer_rgba8888_copy: () => null,
    } as unknown as Machine;

    // Optional methods should require feature detection under `strictNullChecks`.
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error vga_present may be undefined
      machine.vga_present();
      // @ts-expect-error vga_width may be undefined
      machine.vga_width();
      // @ts-expect-error vga_height may be undefined
      machine.vga_height();
      // @ts-expect-error vga_stride_bytes may be undefined
      machine.vga_stride_bytes();
      // @ts-expect-error vga_framebuffer_ptr may be undefined
      machine.vga_framebuffer_ptr();
      // @ts-expect-error vga_framebuffer_len_bytes may be undefined
      machine.vga_framebuffer_len_bytes();
      // @ts-expect-error vga_framebuffer_copy_rgba8888 may be undefined
      machine.vga_framebuffer_copy_rgba8888();
      // @ts-expect-error vga_framebuffer_rgba8888_copy may be undefined
      machine.vga_framebuffer_rgba8888_copy();
    }
    void assertStrictNullChecksEnforced;

    if (machine.vga_present) {
      machine.vga_present();
    }
    if (machine.vga_width && machine.vga_height && machine.vga_stride_bytes) {
      const w = machine.vga_width();
      const h = machine.vga_height();
      const stride = machine.vga_stride_bytes();
      expect(stride).toBe(w * 4);
      expect(w).toBeGreaterThan(0);
      expect(h).toBeGreaterThan(0);
    }
    if (machine.vga_framebuffer_copy_rgba8888) {
      const bytes = machine.vga_framebuffer_copy_rgba8888();
      expect(bytes).toBeInstanceOf(Uint8Array);
    }
  });
});


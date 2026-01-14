import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine boot media typings)", () => {
  it("requires feature detection for optional boot drive + ISO-bytes attach helpers", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      set_boot_drive: (_drive: number) => {},
      attach_install_media_iso_bytes: (_bytes: Uint8Array) => {},
      set_cd_image: (_bytes: Uint8Array) => {},
      set_cd_opfs_existing: async (_path: string) => {},
    } as unknown as Machine;

    // Optional methods should require feature detection under `strictNullChecks`.
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error set_boot_drive may be undefined
      machine.set_boot_drive(0x80);
      // @ts-expect-error attach_install_media_iso_bytes may be undefined
      machine.attach_install_media_iso_bytes(new Uint8Array());
      // @ts-expect-error set_cd_image may be undefined
      machine.set_cd_image(new Uint8Array());
      // @ts-expect-error set_cd_opfs_existing may be undefined
      machine.set_cd_opfs_existing("install.iso");
    }
    void assertStrictNullChecksEnforced;

    if (machine.set_boot_drive) {
      machine.set_boot_drive(0x80);
    }
    if (machine.attach_install_media_iso_bytes) {
      machine.attach_install_media_iso_bytes(new Uint8Array([0x01]));
    }
    if (machine.set_cd_image) {
      machine.set_cd_image(new Uint8Array([0x01]));
    }
    if (machine.set_cd_opfs_existing) {
      void machine.set_cd_opfs_existing("install.iso");
    }

    expect(true).toBe(true);
  });
});

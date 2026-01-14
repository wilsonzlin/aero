import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine OPFS disk typings)", () => {
  it("requires feature detection and uses bigint sizeBytes", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Provide a concrete function so Vitest runtime doesn't crash; compile-time checks are encoded
    // via `@ts-expect-error` comments and validated by `tsc` in CI.
    const machine = {
      set_disk_opfs: async (_path: string, _create: boolean, _sizeBytes: bigint) => {},
      set_disk_opfs_with_progress: async (
        _path: string,
        _create: boolean,
        _sizeBytes: bigint,
        _progress: (progress: number) => void,
      ) => {},
      set_disk_opfs_existing: async (_path: string) => {},
      attach_ide_primary_master_disk_opfs: async (_path: string, _create: boolean, _sizeBytes: bigint) => {},
      attach_ide_primary_master_disk_opfs_with_progress: async (
        _path: string,
        _create: boolean,
        _sizeBytes: bigint,
        _progress: (progress: number) => void,
      ) => {},
      attach_ide_primary_master_disk_opfs_existing: async (_path: string) => {},
    } as unknown as Machine;

    function assertStrictNullChecksEnforced() {
      // @ts-expect-error set_disk_opfs may be undefined
      machine.set_disk_opfs("disk.img", true, 1024n);
      // @ts-expect-error sizeBytes must be a bigint (wasm-bindgen u64), not a number
      machine.set_disk_opfs?.("disk.img", true, 1024);
      // @ts-expect-error set_disk_opfs_with_progress may be undefined
      machine.set_disk_opfs_with_progress("disk.img", true, 1024n, () => {});
      // @ts-expect-error sizeBytes must be a bigint (wasm-bindgen u64), not a number
      machine.set_disk_opfs_with_progress?.("disk.img", true, 1024, () => {});
      // @ts-expect-error progress callback must be a function
      machine.set_disk_opfs_with_progress?.("disk.img", true, 1024n, 123);
      // @ts-expect-error set_disk_opfs_existing may be undefined
      machine.set_disk_opfs_existing("disk.img");
      // @ts-expect-error set_disk_opfs_existing only accepts a path string
      machine.set_disk_opfs_existing?.("disk.img", true);
      // @ts-expect-error attach_ide_primary_master_disk_opfs may be undefined
      machine.attach_ide_primary_master_disk_opfs("disk.img", true, 1024n);
      // @ts-expect-error sizeBytes must be a bigint (wasm-bindgen u64), not a number
      machine.attach_ide_primary_master_disk_opfs?.("disk.img", true, 1024);
      // @ts-expect-error attach_ide_primary_master_disk_opfs_with_progress may be undefined
      machine.attach_ide_primary_master_disk_opfs_with_progress("disk.img", true, 1024n, () => {});
      // @ts-expect-error sizeBytes must be a bigint (wasm-bindgen u64), not a number
      machine.attach_ide_primary_master_disk_opfs_with_progress?.("disk.img", true, 1024, () => {});
      // @ts-expect-error progress callback must be a function
      machine.attach_ide_primary_master_disk_opfs_with_progress?.("disk.img", true, 1024n, 123);
      // @ts-expect-error attach_ide_primary_master_disk_opfs_existing may be undefined
      machine.attach_ide_primary_master_disk_opfs_existing("disk.img");
      // @ts-expect-error attach_ide_primary_master_disk_opfs_existing only accepts a path string
      machine.attach_ide_primary_master_disk_opfs_existing?.("disk.img", true);
    }
    void assertStrictNullChecksEnforced;

    if (machine.set_disk_opfs) {
      void machine.set_disk_opfs("disk.img", true, 1024n);
    }
    if (machine.set_disk_opfs_with_progress) {
      void machine.set_disk_opfs_with_progress("disk.img", true, 1024n, () => {});
    }
    if (machine.set_disk_opfs_existing) {
      void machine.set_disk_opfs_existing("disk.img");
    }
    if (machine.attach_ide_primary_master_disk_opfs) {
      void machine.attach_ide_primary_master_disk_opfs("disk.img", true, 1024n);
    }
    if (machine.attach_ide_primary_master_disk_opfs_with_progress) {
      void machine.attach_ide_primary_master_disk_opfs_with_progress("disk.img", true, 1024n, () => {});
    }
    if (machine.attach_ide_primary_master_disk_opfs_existing) {
      void machine.attach_ide_primary_master_disk_opfs_existing("disk.img");
    }

    expect(true).toBe(true);
  });
});

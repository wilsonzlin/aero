import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine disk overlay typings)", () => {
  it("requires feature detection for optional DISKS overlay methods", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // concrete functions to avoid `undefined is not a function` crashes. The compile-time checks are
    // encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const machine = {
      set_ahci_port0_disk_overlay_ref: (_base: string, _overlay: string) => {},
      clear_ahci_port0_disk_overlay_ref: () => {},
      set_ide_secondary_master_atapi_overlay_ref: (_base: string, _overlay: string) => {},
      clear_ide_secondary_master_atapi_overlay_ref: () => {},
      set_ide_primary_master_ata_overlay_ref: (_base: string, _overlay: string) => {},
      clear_ide_primary_master_ata_overlay_ref: () => {},
      take_restored_disk_overlays: () => null,
    } as unknown as Machine;

    // Optional methods should require feature detection under `strictNullChecks`.
    function assertStrictNullChecksEnforced() {
      // @ts-expect-error set_ahci_port0_disk_overlay_ref may be undefined
      machine.set_ahci_port0_disk_overlay_ref("base.img", "overlay.img");
      // @ts-expect-error clear_ahci_port0_disk_overlay_ref may be undefined
      machine.clear_ahci_port0_disk_overlay_ref();
      // @ts-expect-error set_ide_secondary_master_atapi_overlay_ref may be undefined
      machine.set_ide_secondary_master_atapi_overlay_ref("install.iso", "install.overlay");
      // @ts-expect-error clear_ide_secondary_master_atapi_overlay_ref may be undefined
      machine.clear_ide_secondary_master_atapi_overlay_ref();
      // @ts-expect-error set_ide_primary_master_ata_overlay_ref may be undefined
      machine.set_ide_primary_master_ata_overlay_ref("d2.base", "d2.overlay");
      // @ts-expect-error clear_ide_primary_master_ata_overlay_ref may be undefined
      machine.clear_ide_primary_master_ata_overlay_ref();
      // @ts-expect-error take_restored_disk_overlays may be undefined
      machine.take_restored_disk_overlays();
    }
    void assertStrictNullChecksEnforced;

    if (machine.set_ahci_port0_disk_overlay_ref) {
      machine.set_ahci_port0_disk_overlay_ref("base.img", "overlay.img");
    }
    if (machine.clear_ahci_port0_disk_overlay_ref) {
      machine.clear_ahci_port0_disk_overlay_ref();
    }
    if (machine.set_ide_secondary_master_atapi_overlay_ref) {
      machine.set_ide_secondary_master_atapi_overlay_ref("install.iso", "install.overlay");
    }
    if (machine.clear_ide_secondary_master_atapi_overlay_ref) {
      machine.clear_ide_secondary_master_atapi_overlay_ref();
    }
    if (machine.set_ide_primary_master_ata_overlay_ref) {
      machine.set_ide_primary_master_ata_overlay_ref("d2.base", "d2.overlay");
    }
    if (machine.clear_ide_primary_master_ata_overlay_ref) {
      machine.clear_ide_primary_master_ata_overlay_ref();
    }
    if (machine.take_restored_disk_overlays) {
      machine.take_restored_disk_overlays();
    }

    expect(true).toBe(true);
  });
});


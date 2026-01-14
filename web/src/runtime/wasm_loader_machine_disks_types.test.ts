import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (Machine disk overlay typings)", () => {
  it("requires feature detection for optional DISKS overlay methods", () => {
    type Machine = InstanceType<WasmApi["Machine"]>;
    type MachineCtor = WasmApi["Machine"];

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
      reattach_restored_disks_from_opfs: async () => {},
      set_primary_hdd_opfs_cow: (_base: string, _overlay: string) => {},
      attach_install_media_iso_opfs: (_path: string) => {},
      attach_install_media_opfs_iso: (_path: string) => {},
      take_restored_disk_overlays: () => null,
    } as unknown as Machine;

    const machineCtor = {
      disk_id_primary_hdd: () => 0,
      disk_id_install_media: () => 1,
      disk_id_ide_primary_master: () => 2,
      new_shared: (_guestBase: number, _guestSize: number) => machine,
      new_win7_storage_shared: (_guestBase: number, _guestSize: number) => machine,
      new_win7_storage: (_ramBytes: number) => machine,
    } as unknown as MachineCtor;

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
      // @ts-expect-error reattach_restored_disks_from_opfs may be undefined
      void machine.reattach_restored_disks_from_opfs();
      // @ts-expect-error set_primary_hdd_opfs_cow may be undefined
      void machine.set_primary_hdd_opfs_cow("d.base", "d.overlay");
      // @ts-expect-error attach_install_media_iso_opfs may be undefined
      void machine.attach_install_media_iso_opfs("win7.iso");
      // @ts-expect-error attach_install_media_opfs_iso may be undefined
      void machine.attach_install_media_opfs_iso("win7.iso");
      // @ts-expect-error take_restored_disk_overlays may be undefined
      machine.take_restored_disk_overlays();

      // Static disk_id helpers are also optional and require feature detection.
      // @ts-expect-error disk_id_primary_hdd may be undefined
      machineCtor.disk_id_primary_hdd();
      // @ts-expect-error disk_id_install_media may be undefined
      machineCtor.disk_id_install_media();
      // @ts-expect-error disk_id_ide_primary_master may be undefined
      machineCtor.disk_id_ide_primary_master();

      // Static constructors are optional too.
      // @ts-expect-error new_shared may be undefined
      machineCtor.new_shared(0, 0);
      // @ts-expect-error new_win7_storage_shared may be undefined
      machineCtor.new_win7_storage_shared(0, 0);
      // @ts-expect-error new_win7_storage may be undefined
      machineCtor.new_win7_storage(0);
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
    if (machine.reattach_restored_disks_from_opfs) {
      void machine.reattach_restored_disks_from_opfs();
    }
    if (machine.set_primary_hdd_opfs_cow) {
      void machine.set_primary_hdd_opfs_cow("d.base", "d.overlay");
    }
    if (machine.attach_install_media_iso_opfs) {
      void machine.attach_install_media_iso_opfs("win7.iso");
    }
    if (machine.attach_install_media_opfs_iso) {
      void machine.attach_install_media_opfs_iso("win7.iso");
    }
    if (machine.take_restored_disk_overlays) {
      machine.take_restored_disk_overlays();
    }

    if (machineCtor.disk_id_primary_hdd) {
      machineCtor.disk_id_primary_hdd();
    }
    if (machineCtor.disk_id_install_media) {
      machineCtor.disk_id_install_media();
    }
    if (machineCtor.disk_id_ide_primary_master) {
      machineCtor.disk_id_ide_primary_master();
    }
    if (machineCtor.new_shared) {
      machineCtor.new_shared(0, 0);
    }
    if (machineCtor.new_win7_storage_shared) {
      machineCtor.new_win7_storage_shared(0, 0);
    }
    if (machineCtor.new_win7_storage) {
      machineCtor.new_win7_storage(0);
    }

    expect(true).toBe(true);
  });
});

import { describe, expect, it, vi } from "vitest";

import type { WasmApi } from "./wasm_loader";
import { restoreMachineSnapshotAndReattachDisks, restoreMachineSnapshotFromOpfsAndReattachDisks } from "./machine_snapshot_disks";

describe("runtime/machine_snapshot_disks", () => {
  it("reattaches disk overlay refs after Machine.restore_snapshot_from_opfs", async () => {
    const events: string[] = [];

    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });

    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [
        { disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "aero/disks/win7.overlay" },
        { disk_id: 2, base_image: "aero/isos/win7.iso", overlay_image: "" },
        { disk_id: 999, base_image: "unknown.base", overlay_image: "unknown.overlay" },
      ];
    });

    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string) => {
      events.push("primary");
    });
    const attach_install_media_opfs_iso = vi.fn(async (_path: string) => {
      events.push("iso");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_primary_hdd_opfs_cow,
      attach_install_media_opfs_iso,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(restore_snapshot_from_opfs).toHaveBeenCalledWith("state/test.snap");
    expect(take_restored_disk_overlays).toHaveBeenCalledTimes(1);
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith("aero/disks/win7.base", "aero/disks/win7.overlay");
    expect(attach_install_media_opfs_iso).toHaveBeenCalledWith("aero/isos/win7.iso");
    expect(events).toEqual(["restore", "take", "primary", "iso"]);
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it("reattaches disk overlay refs after Machine.restore_snapshot(bytes)", async () => {
    const events: string[] = [];
    const bytes = new Uint8Array([0x01, 0x02]);

    const restore_snapshot = vi.fn((_bytes: Uint8Array) => {
      events.push("restore");
    });

    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "aero/disks/win7.overlay" }];
    });

    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string) => {
      events.push("primary");
    });

    const machine = {
      restore_snapshot,
      take_restored_disk_overlays,
      set_primary_hdd_opfs_cow,
      attach_install_media_opfs_iso: vi.fn(),
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotAndReattachDisks({ api, machine, bytes, logPrefix: "test" });

    expect(restore_snapshot).toHaveBeenCalledWith(bytes);
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith("aero/disks/win7.base", "aero/disks/win7.overlay");
    expect(events).toEqual(["restore", "take", "primary"]);
  });

  it("supplies overlay block size when set_primary_hdd_opfs_cow requires it (legacy wasm signature)", async () => {
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {});

    const take_restored_disk_overlays = vi.fn(() => [
      { disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "aero/disks/win7.overlay" },
    ]);

    // Older wasm-bindgen exports require a third `overlayBlockSizeBytes` argument.
    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string, _blockSizeBytes: number) => {});

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_primary_hdd_opfs_cow,
      attach_install_media_opfs_iso: vi.fn(),
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith(
      "aero/disks/win7.base",
      "aero/disks/win7.overlay",
      1024 * 1024,
    );
  });
});

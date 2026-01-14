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

    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string, _blockSize: number) => {
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
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith("aero/disks/win7.base", "aero/disks/win7.overlay", 0);
    expect(attach_install_media_opfs_iso).toHaveBeenCalledWith("aero/isos/win7.iso");
    expect(events).toEqual(["restore", "take", "primary", "iso"]);
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it("reattaches a base-only primary disk via set_primary_hdd_opfs_existing when available", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "" }];
    });
    const set_primary_hdd_opfs_existing = vi.fn(async (_path: string) => {
      events.push("primary-existing");
    });
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {
      events.push("disk-existing");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_primary_hdd_opfs_existing,
      set_disk_opfs_existing,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(restore_snapshot_from_opfs).toHaveBeenCalledWith("state/test.snap");
    expect(set_primary_hdd_opfs_existing).toHaveBeenCalledWith("aero/disks/win7.base");
    expect(set_disk_opfs_existing).not.toHaveBeenCalled();
    expect(events).toEqual(["restore", "take", "primary-existing"]);
  });

  it("falls back to set_disk_opfs_existing for base-only primary disks when set_primary_hdd_opfs_existing is unavailable", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "" }];
    });
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {
      events.push("disk-existing");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_disk_opfs_existing,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(restore_snapshot_from_opfs).toHaveBeenCalledWith("state/test.snap");
    expect(set_disk_opfs_existing).toHaveBeenCalledWith("aero/disks/win7.base");
    expect(events).toEqual(["restore", "take", "disk-existing"]);
  });

  it("prefers set_disk_opfs_existing_and_set_overlay_ref for base-only primary disks when available", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "" }];
    });
    const set_disk_opfs_existing_and_set_overlay_ref = vi.fn(async (_path: string) => {
      events.push("disk-existing-and-ref");
    });
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {
      events.push("disk-existing");
    });
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {
      events.push("set-ref");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_disk_opfs_existing_and_set_overlay_ref,
      set_disk_opfs_existing,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(set_disk_opfs_existing_and_set_overlay_ref).toHaveBeenCalledWith("aero/disks/win7.base");
    expect(set_disk_opfs_existing).not.toHaveBeenCalled();
    expect(set_ahci_port0_disk_overlay_ref).toHaveBeenCalledWith("aero/disks/win7.base", "");
    expect(events).toEqual(["restore", "take", "disk-existing-and-ref", "set-ref"]);
  });

  it("falls back to stable disk IDs when Machine.disk_id_* helpers are unavailable", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });

    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [
        { disk_id: 0, base_image: "aero/disks/win7.base", overlay_image: "aero/disks/win7.overlay" },
        { disk_id: 1, base_image: "aero/isos/win7.iso", overlay_image: "" },
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

    // Intentionally omit `disk_id_primary_hdd` / `disk_id_install_media`.
    const api = {
      Machine: {},
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(restore_snapshot_from_opfs).toHaveBeenCalledWith("state/test.snap");
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith("aero/disks/win7.base", "aero/disks/win7.overlay", 0);
    expect(attach_install_media_opfs_iso).toHaveBeenCalledWith("aero/isos/win7.iso");
    expect(events).toEqual(["restore", "take", "primary", "iso"]);
  });

  it("uses attach_install_media_iso_opfs_existing when available (back-compat)", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });

    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [
        { disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "aero/disks/win7.overlay" },
        { disk_id: 2, base_image: "aero/isos/win7.iso", overlay_image: "" },
      ];
    });

    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string) => {
      events.push("primary");
    });
    const attach_install_media_iso_opfs_existing = vi.fn(async (_path: string) => {
      events.push("iso");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_primary_hdd_opfs_cow,
      attach_install_media_iso_opfs_existing,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(restore_snapshot_from_opfs).toHaveBeenCalledWith("state/test.snap");
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith("aero/disks/win7.base", "aero/disks/win7.overlay", 0);
    expect(attach_install_media_iso_opfs_existing).toHaveBeenCalledWith("aero/isos/win7.iso");
    expect(events).toEqual(["restore", "take", "primary", "iso"]);
  });

  it("reattaches aerosparse base disks via Machine.set_disk_aerospar_opfs_open when available", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.aerospar", overlay_image: "" }];
    });
    const set_disk_aerospar_opfs_open = vi.fn(async (_path: string) => {
      events.push("aerospar");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_disk_aerospar_opfs_open,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(set_disk_aerospar_opfs_open).toHaveBeenCalledWith("aero/disks/win7.aerospar");
    expect(events).toEqual(["restore", "take", "aerospar"]);
  });

  it("detects aerosparse base disks by header even when the file name has a non-.aerospar suffix", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "" }];
    });
    const set_disk_aerospar_opfs_open = vi.fn(async (_path: string) => {
      events.push("aerospar");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_disk_aerospar_opfs_open,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    const originalNavigatorDesc = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    try {
      const header = new Uint8Array(64);
      // "AEROSPAR"
      header.set([0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52], 0);
      const dv = new DataView(header.buffer);
      dv.setUint32(8, 1, true); // version
      dv.setUint32(12, 64, true); // header size
      dv.setUint32(16, 1024 * 1024, true); // block size
      const file = new Blob([header]);

      const fileHandle = {
        getFile: async () => file,
      };
      const disksDir = {
        getDirectoryHandle: async (_name: string) => {
          throw new Error("unexpected nested directory");
        },
        getFileHandle: async (name: string) => {
          if (name !== "win7.base") throw new Error(`unexpected file request: ${name}`);
          return fileHandle;
        },
      };
      const aeroDir = {
        getDirectoryHandle: async (name: string) => {
          if (name !== "disks") throw new Error(`unexpected directory request: ${name}`);
          return disksDir;
        },
        getFileHandle: async (_name: string) => {
          throw new Error("unexpected file request at aero/");
        },
      };
      const rootDir = {
        getDirectoryHandle: async (name: string) => {
          if (name !== "aero") throw new Error(`unexpected directory request: ${name}`);
          return aeroDir;
        },
        getFileHandle: async (_name: string) => {
          throw new Error("unexpected file request at root");
        },
      };

      Object.defineProperty(globalThis, "navigator", {
        value: {
          storage: {
            getDirectory: async () => rootDir,
          },
        },
        configurable: true,
      });

      await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

      expect(set_disk_aerospar_opfs_open).toHaveBeenCalledWith("aero/disks/win7.base");
      expect(events).toEqual(["restore", "take", "aerospar"]);
    } finally {
      if (originalNavigatorDesc) {
        Object.defineProperty(globalThis, "navigator", originalNavigatorDesc);
      } else {
        delete (globalThis as unknown as { navigator?: unknown }).navigator;
      }
    }
  });

  it("opens aerosparse base disks via set_disk_opfs_existing(base, \"aerospar\") when set_disk_aerospar_opfs_open is unavailable", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "" }];
    });

    const calls: Array<[string, string | undefined]> = [];
    async function set_disk_opfs_existing(path: string, baseFormat?: string): Promise<void> {
      calls.push([path, baseFormat]);
      events.push("disk-existing");
    }

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_disk_opfs_existing,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    const originalNavigatorDesc = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    try {
      const header = new Uint8Array(64);
      // "AEROSPAR"
      header.set([0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52], 0);
      const dv = new DataView(header.buffer);
      dv.setUint32(8, 1, true); // version
      dv.setUint32(12, 64, true); // header size
      dv.setUint32(16, 1024 * 1024, true); // block size
      const file = new Blob([header]);

      const fileHandle = {
        getFile: async () => file,
      };
      const disksDir = {
        getDirectoryHandle: async (_name: string) => {
          throw new Error("unexpected nested directory");
        },
        getFileHandle: async (name: string) => {
          if (name !== "win7.base") throw new Error(`unexpected file request: ${name}`);
          return fileHandle;
        },
      };
      const aeroDir = {
        getDirectoryHandle: async (name: string) => {
          if (name !== "disks") throw new Error(`unexpected directory request: ${name}`);
          return disksDir;
        },
        getFileHandle: async (_name: string) => {
          throw new Error("unexpected file request at aero/");
        },
      };
      const rootDir = {
        getDirectoryHandle: async (name: string) => {
          if (name !== "aero") throw new Error(`unexpected directory request: ${name}`);
          return aeroDir;
        },
        getFileHandle: async (_name: string) => {
          throw new Error("unexpected file request at root");
        },
      };

      Object.defineProperty(globalThis, "navigator", {
        value: {
          storage: {
            getDirectory: async () => rootDir,
          },
        },
        configurable: true,
      });

      await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

      expect(calls).toEqual([["aero/disks/win7.base", "aerospar"]]);
      expect(events).toEqual(["restore", "take", "disk-existing"]);
    } finally {
      if (originalNavigatorDesc) {
        Object.defineProperty(globalThis, "navigator", originalNavigatorDesc);
      } else {
        delete (globalThis as unknown as { navigator?: unknown }).navigator;
      }
    }
  });

  it("prefers Machine.set_disk_cow_opfs_open when available (supports non-raw base images)", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });
    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 1, base_image: "aero/disks/win7.aerospar", overlay_image: "aero/disks/win7.overlay" }];
    });
    const set_disk_cow_opfs_open = vi.fn(async (_base: string, _overlay: string) => {
      events.push("cow");
    });
    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string) => {
      events.push("primary");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      set_disk_cow_opfs_open,
      set_primary_hdd_opfs_cow,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(set_disk_cow_opfs_open).toHaveBeenCalledWith("aero/disks/win7.aerospar", "aero/disks/win7.overlay");
    expect(set_primary_hdd_opfs_cow).not.toHaveBeenCalled();
    expect(events).toEqual(["restore", "take", "cow"]);
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

    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string, _blockSize: number) => {
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
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith("aero/disks/win7.base", "aero/disks/win7.overlay", 0);
    expect(events).toEqual(["restore", "take", "primary"]);
  });

  it("reattaches the IDE primary master disk (disk_id_ide_primary_master) when present", async () => {
    const events: string[] = [];
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {
      events.push("restore");
    });

    const take_restored_disk_overlays = vi.fn(() => {
      events.push("take");
      return [{ disk_id: 3, base_image: "aero/disks/ide.img", overlay_image: "" }];
    });

    const attach_ide_primary_master_disk_opfs_existing = vi.fn(async (_path: string) => {
      events.push("ide");
    });

    const machine = {
      restore_snapshot_from_opfs,
      take_restored_disk_overlays,
      attach_ide_primary_master_disk_opfs_existing,
    } as unknown as InstanceType<WasmApi["Machine"]>;

    const api = {
      Machine: {
        disk_id_primary_hdd: () => 1,
        disk_id_install_media: () => 2,
        disk_id_ide_primary_master: () => 3,
      },
    } as unknown as WasmApi;

    await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path: "state/test.snap", logPrefix: "test" });

    expect(restore_snapshot_from_opfs).toHaveBeenCalledWith("state/test.snap");
    expect(attach_ide_primary_master_disk_opfs_existing).toHaveBeenCalledWith("aero/disks/ide.img");
    expect(events).toEqual(["restore", "take", "ide"]);
  });

  it("passes block size 0 to set_primary_hdd_opfs_cow to infer it from the overlay header", async () => {
    const restore_snapshot_from_opfs = vi.fn(async (_path: string) => {});

    const take_restored_disk_overlays = vi.fn(() => [
      { disk_id: 1, base_image: "aero/disks/win7.base", overlay_image: "aero/disks/win7.overlay" },
    ]);

    const calls: Array<[string, string, number]> = [];
    async function set_primary_hdd_opfs_cow(base: string, overlay: string, blockSizeBytes: number): Promise<void> {
      calls.push([base, overlay, blockSizeBytes]);
    }

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

    expect(calls).toEqual([["aero/disks/win7.base", "aero/disks/win7.overlay", 0]]);
  });
});

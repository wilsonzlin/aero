import { describe, expect, it, vi } from "vitest";

import type { MachineHandle } from "./wasm_loader";
import { attachMachineBootDisk, planMachineBootDiskAttachment } from "./machine_disk_attach";
import type { DiskImageMetadata } from "../storage/metadata";
import { OPFS_DISKS_PATH } from "../storage/metadata";
import { DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES } from "./boot_disks_protocol";

describe("runtime/machine_disk_attach (metadata compatibility)", () => {
  it("rejects IDB-backed disks for machine runtime", () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "d1",
      name: "disk",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    expect(() => planMachineBootDiskAttachment(meta, "hdd")).toThrow(/opfs/i);
  });

  it("rejects unsupported HDD formats for machine runtime", () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "d2",
      name: "disk",
      backend: "opfs",
      kind: "hdd",
      format: "qcow2",
      fileName: "disk.qcow2",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    expect(() => planMachineBootDiskAttachment(meta, "hdd")).toThrow(/raw\/aerospar/i);
  });

  it("rejects remote streaming disks for machine runtime", () => {
    const meta: DiskImageMetadata = {
      source: "remote",
      id: "r1",
      name: "remote",
      kind: "hdd",
      format: "raw",
      sizeBytes: 1024,
      createdAtMs: 0,
      remote: {
        imageId: "img",
        version: "1",
        delivery: "range",
        urls: { url: "/images/img/1" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "opfs",
        fileName: "cache.img",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
      },
    };
    expect(() => planMachineBootDiskAttachment(meta, "hdd")).toThrow(
      "machine runtime does not yet support remote streaming disks",
    );
  });

  it("does not treat local disks as remote-streaming based on inherited Object.prototype.remote", () => {
    const remoteExisting = Object.getOwnPropertyDescriptor(Object.prototype, "remote");
    if (remoteExisting && remoteExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "remote", {
        value: { url: "https://example.com/evil.img" },
        configurable: true,
        writable: true,
      });

      const meta: DiskImageMetadata = {
        source: "local",
        id: "d-local",
        name: "disk",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        fileName: "disk.img",
        sizeBytes: 1024,
        createdAtMs: 0,
      };

      const plan = planMachineBootDiskAttachment(meta, "hdd");
      expect(plan.format).toBe("raw");
      expect(plan.opfsPath).toBe(`${OPFS_DISKS_PATH}/${meta.fileName}`);
    } finally {
      if (remoteExisting) Object.defineProperty(Object.prototype, "remote", remoteExisting);
      else Reflect.deleteProperty(Object.prototype, "remote");
    }
  });

  it("rejects unknown HDD format metadata for machine runtime", () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "d3",
      name: "disk",
      backend: "opfs",
      kind: "hdd",
      format: "unknown",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    expect(() => planMachineBootDiskAttachment(meta, "hdd")).toThrow(/format metadata/i);
  });

  it("rejects unknown ISO format metadata for install media", () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "cd1",
      name: "cd",
      backend: "opfs",
      kind: "cd",
      format: "unknown",
      fileName: "win7.iso",
      sizeBytes: 2048,
      createdAtMs: 0,
    };
    expect(() => planMachineBootDiskAttachment(meta, "cd")).toThrow(/ISO install media/i);
  });
});

describe("runtime/machine_disk_attach (Machine attach method selection)", () => {
  function cdMeta(): DiskImageMetadata {
    return {
      source: "local",
      id: "cd1",
      name: "cd",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "win7.iso",
      sizeBytes: 2048,
      createdAtMs: 0,
    };
  }

  function hddMetaRaw(): DiskImageMetadata {
    return {
      source: "local",
      id: "d1",
      name: "disk",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
  }

  function hddMetaAerospar(): DiskImageMetadata {
    return {
      source: "local",
      id: "d-aero",
      name: "disk",
      backend: "opfs",
      kind: "hdd",
      format: "aerospar",
      fileName: "disk.aerospar",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
  }

  it("prefers set_primary_hdd_opfs_existing when attaching a raw HDD", async () => {
    const meta = hddMetaRaw();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const set_primary_hdd_opfs_existing = vi.fn(async (_path: string) => {});
    const set_disk_opfs_existing_and_set_overlay_ref = vi.fn(async (_path: string) => {});
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {});
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {});

    const machine = {
      set_primary_hdd_opfs_existing,
      set_disk_opfs_existing_and_set_overlay_ref,
      set_disk_opfs_existing,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(set_primary_hdd_opfs_existing).toHaveBeenCalledWith(plan.opfsPath);
    expect(set_disk_opfs_existing_and_set_overlay_ref).not.toHaveBeenCalled();
    expect(set_disk_opfs_existing).not.toHaveBeenCalled();
    expect(set_ahci_port0_disk_overlay_ref).toHaveBeenCalledWith(plan.opfsPath, "");
  });

  it("falls back to set_disk_opfs_existing(path, \"aerospar\") when attaching an aerosparse HDD and dedicated aerospar exports are unavailable", async () => {
    const meta = hddMetaAerospar();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const calls: Array<[string, string | undefined, bigint | undefined]> = [];
    async function set_disk_opfs_existing(
      path: string,
      baseFormat?: string,
      expectedSizeBytes?: bigint,
    ): Promise<void> {
      calls.push([path, baseFormat, expectedSizeBytes]);
    }
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {});

    const machine = {
      set_disk_opfs_existing,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(calls).toEqual([[plan.opfsPath, "aerospar", 1024n]]);
    expect(set_ahci_port0_disk_overlay_ref).toHaveBeenCalledWith(plan.opfsPath, "");
  });

  it("supports camelCase Machine.setDiskOpfsExisting(path, \"aerospar\") for aerosparse HDD attachment", async () => {
    const meta = hddMetaAerospar();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const calls: Array<[string, string | undefined, bigint | undefined]> = [];
    async function setDiskOpfsExisting(
      path: string,
      baseFormat?: string,
      expectedSizeBytes?: bigint,
    ): Promise<void> {
      calls.push([path, baseFormat, expectedSizeBytes]);
    }
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {});

    const machine = {
      setDiskOpfsExisting,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(calls).toEqual([[plan.opfsPath, "aerospar", 1024n]]);
    expect(set_ahci_port0_disk_overlay_ref).toHaveBeenCalledWith(plan.opfsPath, "");
  });

  it("supports camelCase Machine.setAhciPort0DiskOverlayRef for fallback overlay ref setting", async () => {
    const meta = hddMetaRaw();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const calls: string[] = [];
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {
      calls.push("attach");
    });
    const setAhciPort0DiskOverlayRef = vi.fn((_base: string, _overlay: string) => {
      calls.push("set-ref");
    });

    const machine = {
      set_disk_opfs_existing,
      setAhciPort0DiskOverlayRef,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(set_disk_opfs_existing).toHaveBeenCalledWith(plan.opfsPath, undefined, 1024n);
    expect(setAhciPort0DiskOverlayRef).toHaveBeenCalledWith(plan.opfsPath, "");
    expect(calls).toEqual(["attach", "set-ref"]);
  });

  it("falls back to set_disk_opfs_existing_and_set_overlay_ref when set_primary_hdd_opfs_existing is unavailable", async () => {
    const meta = hddMetaRaw();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const set_disk_opfs_existing_and_set_overlay_ref = vi.fn(async (_path: string) => {});
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {});
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {});

    const machine = {
      set_disk_opfs_existing_and_set_overlay_ref,
      set_disk_opfs_existing,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(set_disk_opfs_existing_and_set_overlay_ref).toHaveBeenCalledWith(plan.opfsPath, undefined, 1024n);
    expect(set_disk_opfs_existing).not.toHaveBeenCalled();
    expect(set_ahci_port0_disk_overlay_ref).not.toHaveBeenCalled();
  });

  it("falls back to set_disk_opfs_existing + set_ahci_port0_disk_overlay_ref when only low-level disk attach is available", async () => {
    const meta = hddMetaRaw();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const calls: string[] = [];
    const set_disk_opfs_existing = vi.fn(async (_path: string) => {
      calls.push("set_disk_opfs_existing");
    });
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {
      calls.push("set_ref");
    });

    const machine = {
      set_disk_opfs_existing,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(set_disk_opfs_existing).toHaveBeenCalledWith(plan.opfsPath, undefined, 1024n);
    expect(set_ahci_port0_disk_overlay_ref).toHaveBeenCalledWith(plan.opfsPath, "");
    expect(calls).toEqual(["set_disk_opfs_existing", "set_ref"]);
  });

  it("prefers attach_install_media_iso_opfs_existing_and_set_overlay_ref when present (back-compat)", async () => {
    const meta = cdMeta();
    const plan = planMachineBootDiskAttachment(meta, "cd");

    const attach = vi.fn(async (_path: string) => {});
    const setRef = vi.fn((_base: string, _overlay: string) => {});
    const machine = {
      attach_install_media_iso_opfs_existing_and_set_overlay_ref: attach,
      set_ide_secondary_master_atapi_overlay_ref: setRef,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "cd", meta);

    expect(attach).toHaveBeenCalledWith(plan.opfsPath);
    expect(setRef).not.toHaveBeenCalled();
  });

  it("falls back to attach_install_media_iso_opfs_existing + set_ide_secondary_master_atapi_overlay_ref", async () => {
    const meta = cdMeta();
    const plan = planMachineBootDiskAttachment(meta, "cd");

    const calls: string[] = [];
    let gotPath: string | null = null;
    async function attach_install_media_iso_opfs_existing(path: string): Promise<void> {
      gotPath = path;
      calls.push("attach");
    }
    let gotRef: { base: string; overlay: string } | null = null;
    function set_ide_secondary_master_atapi_overlay_ref(base: string, overlay: string): void {
      gotRef = { base, overlay };
      calls.push("setRef");
    }
    const machine = {
      attach_install_media_iso_opfs_existing,
      set_ide_secondary_master_atapi_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "cd", meta);

    expect(calls).toEqual(["attach", "setRef"]);
    expect(gotPath).toBe(plan.opfsPath);
    expect(gotRef).toEqual({ base: plan.opfsPath, overlay: "" });
    expect(plan.opfsPath).toContain("win7.iso");
  });

  it("prefers set_primary_hdd_opfs_cow for raw HDDs when present", async () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "hdd1",
      name: "Disk 1",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const attach = vi.fn(async (_basePath: string, _overlayPath: string, _blockSizeBytes: number) => {});
    const machine = {
      set_primary_hdd_opfs_cow: attach,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(attach).toHaveBeenCalledWith(
      plan.opfsPath,
      `${OPFS_DISKS_PATH}/${meta.id}.overlay.aerospar`,
      DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES,
    );
  });

  it("prefers set_primary_hdd_opfs_cow for aerosparse HDDs when present", async () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "hdd-aerospar",
      name: "Disk aerospar",
      backend: "opfs",
      kind: "hdd",
      format: "aerospar",
      fileName: "disk.aerospar",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const attach = vi.fn(async (_basePath: string, _overlayPath: string, _blockSizeBytes: number) => {});
    const set_disk_aerospar_opfs_open = vi.fn(async (_path: string) => {});
    const machine = {
      set_primary_hdd_opfs_cow: attach,
      set_disk_aerospar_opfs_open,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    expect(attach).toHaveBeenCalledWith(
      plan.opfsPath,
      `${OPFS_DISKS_PATH}/${meta.id}.overlay.aerospar`,
      DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES,
    );
    expect(set_disk_aerospar_opfs_open).not.toHaveBeenCalled();
  });

  it("sets the AHCI port0 overlay ref after attaching via set_primary_hdd_opfs_cow", async () => {
    const meta = hddMetaRaw();
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const calls: string[] = [];
    const set_primary_hdd_opfs_cow = vi.fn(async (_base: string, _overlay: string, _blockSizeBytes: number) => {
      calls.push("cow");
    });
    const set_ahci_port0_disk_overlay_ref = vi.fn((_base: string, _overlay: string) => {
      calls.push("set-ref");
    });

    const machine = {
      set_primary_hdd_opfs_cow,
      set_ahci_port0_disk_overlay_ref,
    } as unknown as MachineHandle;

    await attachMachineBootDisk(machine, "hdd", meta);

    const expectedOverlay = `${OPFS_DISKS_PATH}/${meta.id}.overlay.aerospar`;
    expect(set_primary_hdd_opfs_cow).toHaveBeenCalledWith(plan.opfsPath, expectedOverlay, DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES);
    expect(set_ahci_port0_disk_overlay_ref).toHaveBeenCalledWith(plan.opfsPath, expectedOverlay);
    expect(calls).toEqual(["cow", "set-ref"]);
  });

  it("uses the aerosparse overlay header block size when available", async () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "hdd2",
      name: "Disk 2",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const overlayHeader = new Uint8Array(64);
    // "AEROSPAR"
    overlayHeader.set([0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52], 0);
    const dv = new DataView(overlayHeader.buffer);
    dv.setUint32(8, 1, true); // version
    dv.setUint32(12, 64, true); // header size
    dv.setUint32(16, 4096, true); // block size
    dv.setBigUint64(24, 4096n, true); // disk size bytes
    dv.setBigUint64(32, 64n, true); // table offset
    dv.setBigUint64(40, 1n, true); // table entries
    dv.setBigUint64(48, 4096n, true); // data offset
    dv.setBigUint64(56, 0n, true); // allocated blocks
    const file = new Blob([overlayHeader, new Uint8Array(4096 - 64)]);

    const originalNavigatorDesc = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    try {
      const fileHandle = {
        getFile: async () => file,
      };
      const disksDir = {
        getDirectoryHandle: async (_name: string) => {
          throw new Error("unexpected nested directory");
        },
        getFileHandle: async (name: string) => {
          if (name !== `${meta.id}.overlay.aerospar`) throw new Error(`unexpected file request: ${name}`);
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

      const attach = vi.fn(async (_basePath: string, _overlayPath: string, _blockSizeBytes: number) => {});
      const machine = {
        set_primary_hdd_opfs_cow: attach,
      } as unknown as MachineHandle;

      await attachMachineBootDisk(machine, "hdd", meta);

      expect(attach).toHaveBeenCalledWith(plan.opfsPath, `${OPFS_DISKS_PATH}/${meta.id}.overlay.aerospar`, 4096);
    } finally {
      if (originalNavigatorDesc) {
        Object.defineProperty(globalThis, "navigator", originalNavigatorDesc);
      } else {
        delete (globalThis as unknown as { navigator?: unknown }).navigator;
      }
    }
  });

  it("ignores truncated aerosparse overlay files even if the header looks valid", async () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "hdd2-truncated",
      name: "Disk 2 (truncated overlay)",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const overlayHeader = new Uint8Array(64);
    // "AEROSPAR"
    overlayHeader.set([0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52], 0);
    const dv = new DataView(overlayHeader.buffer);
    dv.setUint32(8, 1, true); // version
    dv.setUint32(12, 64, true); // header size
    dv.setUint32(16, 4096, true); // block size
    dv.setBigUint64(24, 4096n, true); // disk size bytes
    dv.setBigUint64(32, 64n, true); // table offset
    dv.setBigUint64(40, 1n, true); // table entries
    dv.setBigUint64(48, 4096n, true); // data offset
    // allocated_blocks=1 means the image must contain 1 full block in the data region, but this
    // file is truncated at `data_offset`.
    dv.setBigUint64(56, 1n, true);
    const file = new Blob([overlayHeader, new Uint8Array(4096 - 64)]);

    const originalNavigatorDesc = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    try {
      const fileHandle = {
        getFile: async () => file,
      };
      const disksDir = {
        getDirectoryHandle: async (_name: string) => {
          throw new Error("unexpected nested directory");
        },
        getFileHandle: async (name: string) => {
          if (name !== `${meta.id}.overlay.aerospar`) throw new Error(`unexpected file request: ${name}`);
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

      const attach = vi.fn(async (_basePath: string, _overlayPath: string, _blockSizeBytes: number) => {});
      const machine = {
        set_primary_hdd_opfs_cow: attach,
      } as unknown as MachineHandle;

      await attachMachineBootDisk(machine, "hdd", meta);

      expect(attach).toHaveBeenCalledWith(
        plan.opfsPath,
        `${OPFS_DISKS_PATH}/${meta.id}.overlay.aerospar`,
        DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES,
      );
    } finally {
      if (originalNavigatorDesc) {
        Object.defineProperty(globalThis, "navigator", originalNavigatorDesc);
      } else {
        delete (globalThis as unknown as { navigator?: unknown }).navigator;
      }
    }
  });

  it("ignores invalid aerosparse overlay headers and falls back to the default block size", async () => {
    const meta: DiskImageMetadata = {
      source: "local",
      id: "hdd3",
      name: "Disk 3",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const plan = planMachineBootDiskAttachment(meta, "hdd");

    const overlayHeader = new Uint8Array(64);
    // "AEROSPAR"
    overlayHeader.set([0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52], 0);
    const dv = new DataView(overlayHeader.buffer);
    dv.setUint32(8, 1, true); // version
    dv.setUint32(12, 64, true); // header size
    dv.setUint32(16, 1024 * 1024, true); // block size
    dv.setBigUint64(24, 1024n * 1024n, true); // disk size bytes
    // Corrupt table offset (must be 64).
    dv.setBigUint64(32, 0n, true);
    const file = new Blob([overlayHeader]);

    const originalNavigatorDesc = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    try {
      const fileHandle = {
        getFile: async () => file,
      };
      const disksDir = {
        getDirectoryHandle: async (_name: string) => {
          throw new Error("unexpected nested directory");
        },
        getFileHandle: async (name: string) => {
          if (name !== `${meta.id}.overlay.aerospar`) throw new Error(`unexpected file request: ${name}`);
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

      const attach = vi.fn(async (_basePath: string, _overlayPath: string, _blockSizeBytes: number) => {});
      const machine = {
        set_primary_hdd_opfs_cow: attach,
      } as unknown as MachineHandle;

      await attachMachineBootDisk(machine, "hdd", meta);

      expect(attach).toHaveBeenCalledWith(
        plan.opfsPath,
        `${OPFS_DISKS_PATH}/${meta.id}.overlay.aerospar`,
        DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES,
      );
    } finally {
      if (originalNavigatorDesc) {
        Object.defineProperty(globalThis, "navigator", originalNavigatorDesc);
      } else {
        delete (globalThis as unknown as { navigator?: unknown }).navigator;
      }
    }
  });
});

import { describe, expect, it, vi } from "vitest";

import type { MachineHandle } from "./wasm_loader";
import { attachMachineBootDisk, planMachineBootDiskAttachment } from "./machine_disk_attach";
import type { DiskImageMetadata } from "../storage/metadata";

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

  it("allows unknown ISO format with a warning for install media", () => {
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
    const plan = planMachineBootDiskAttachment(meta, "cd");
    expect(plan.format).toBe("iso");
    expect(plan.warnings.length).toBeGreaterThan(0);
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
});

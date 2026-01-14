import { describe, expect, it } from "vitest";

import type { DiskImageMetadata } from "../storage/metadata";
import { OPFS_DISKS_PATH } from "../storage/metadata";
import {
  DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES,
  emptySetBootDisksMessage,
  machineBootDisksToOpfsSpec,
  type SetBootDisksMessage,
} from "./boot_disks_protocol";

describe("runtime/boot_disks_protocol (machineBootDisksToOpfsSpec)", () => {
  it("derives OPFS paths and bootDrive for local OPFS HDD + no CD", () => {
    const hdd = {
      source: "local",
      id: "disk-1",
      name: "Disk 1",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk-1.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
    const spec = machineBootDisksToOpfsSpec(msg);

    expect(spec.hdd?.basePath).toBe(`${OPFS_DISKS_PATH}/${hdd.fileName}`);
    expect(spec.hdd?.overlayPath).toBe(`${OPFS_DISKS_PATH}/${hdd.id}.overlay.aerospar`);
    expect(spec.hdd?.overlayBlockSizeBytes).toBe(DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES);
    expect(spec.cd).toBeNull();
    expect(spec.bootDrive).toBe(0x80);
  });

  it("accepts aerosparse HDD base images", () => {
    const hdd = {
      source: "local",
      id: "disk-aerospar",
      name: "Disk aerospar",
      backend: "opfs",
      kind: "hdd",
      format: "aerospar",
      fileName: "disk.aerospar",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
    const spec = machineBootDisksToOpfsSpec(msg);

    expect(spec.hdd?.basePath).toBe(`${OPFS_DISKS_PATH}/${hdd.fileName}`);
    expect(spec.hdd?.overlayPath).toBe(`${OPFS_DISKS_PATH}/${hdd.id}.overlay.aerospar`);
    expect(spec.hdd?.overlayBlockSizeBytes).toBe(DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES);
    expect(spec.bootDrive).toBe(0x80);
  });

  it("derives OPFS path and bootDrive for local OPFS CD ISO", () => {
    const cd = {
      source: "local",
      id: "win7-iso",
      name: "Windows 7 ISO",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "win7.iso",
      sizeBytes: 1024,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd: null, cd };
    const spec = machineBootDisksToOpfsSpec(msg);

    expect(spec.hdd).toBeNull();
    expect(spec.cd?.path).toBe(`${OPFS_DISKS_PATH}/${cd.fileName}`);
    expect(spec.bootDrive).toBe(0xe0);
  });

  it("honors explicit bootDevice when both HDD + CD are present", () => {
    const hdd = {
      source: "local",
      id: "disk-1",
      name: "Disk 1",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk-1.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;
    const cd = {
      source: "local",
      id: "win7-iso",
      name: "Windows 7 ISO",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "win7.iso",
      sizeBytes: 1024,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const cdBoot = machineBootDisksToOpfsSpec({ ...emptySetBootDisksMessage(), hdd, cd, bootDevice: "cdrom" });
    expect(cdBoot.bootDrive).toBe(0xe0);

    const hddBoot = machineBootDisksToOpfsSpec({ ...emptySetBootDisksMessage(), hdd, cd, bootDevice: "hdd" });
    expect(hddBoot.bootDrive).toBe(0x80);
  });

  it("rejects remote disks", () => {
    const remote = {
      source: "remote",
      id: "remote-1",
      name: "Remote Disk",
      kind: "hdd",
      format: "raw",
      sizeBytes: 512,
      createdAtMs: 0,
      remote: {
        imageId: "img-1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.com/disk.img" },
      },
      cache: {
        chunkSizeBytes: 1024 * 1024,
        backend: "opfs",
        fileName: "remote-1.cache.aerospar",
        overlayFileName: "remote-1.overlay.aerospar",
        overlayBlockSizeBytes: 1024 * 1024,
      },
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd: remote, cd: null };
    expect(() => machineBootDisksToOpfsSpec(msg)).toThrow(/remote disks are not supported/i);
  });

  it("rejects IndexedDB-backed disks", () => {
    const hdd = {
      source: "local",
      id: "disk-idb",
      name: "Disk IDB",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: "disk-idb.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
    expect(() => machineBootDisksToOpfsSpec(msg)).toThrow(/only OPFS-backed disks are supported/i);
  });

  it("rejects unsupported HDD formats", () => {
    const hdd = {
      source: "local",
      id: "disk-qcow2",
      name: "Disk QCOW2",
      backend: "opfs",
      kind: "hdd",
      format: "qcow2",
      fileName: "disk.qcow2",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
    expect(() => machineBootDisksToOpfsSpec(msg)).toThrow(/unsupported format/i);
  });

  it("rejects unknown HDD format metadata", () => {
    const hdd = {
      source: "local",
      id: "disk-unknown",
      name: "Disk unknown",
      backend: "opfs",
      kind: "hdd",
      format: "unknown",
      fileName: "disk.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
    expect(() => machineBootDisksToOpfsSpec(msg)).toThrow(/requires explicit HDD format metadata/i);
  });

  it("rejects non-ISO CD formats", () => {
    const cd = {
      source: "local",
      id: "cd-raw",
      name: "CD raw",
      backend: "opfs",
      kind: "cd",
      format: "raw",
      fileName: "cd.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd: null, cd };
    expect(() => machineBootDisksToOpfsSpec(msg)).toThrow(/expected \"iso\"/i);
  });

  it("rejects legacy remote-streaming local disks (meta.remote)", () => {
    const hdd = {
      source: "local",
      id: "disk-legacy-remote",
      name: "Disk legacy remote",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 512,
      createdAtMs: 0,
      remote: { url: "https://example.com/disk.img" },
    } satisfies DiskImageMetadata;

    const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
    expect(() => machineBootDisksToOpfsSpec(msg)).toThrow(/remote-streaming disks are not supported/i);
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

      const hdd = {
        source: "local",
        id: "disk-1",
        name: "Disk 1",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        fileName: "disk-1.img",
        sizeBytes: 512,
        createdAtMs: 0,
      } satisfies DiskImageMetadata;

      const msg: SetBootDisksMessage = { ...emptySetBootDisksMessage(), hdd, cd: null };
      const spec = machineBootDisksToOpfsSpec(msg);
      expect(spec.hdd?.basePath).toBe(`${OPFS_DISKS_PATH}/${hdd.fileName}`);
    } finally {
      if (remoteExisting) Object.defineProperty(Object.prototype, "remote", remoteExisting);
      else Reflect.deleteProperty(Object.prototype, "remote");
    }
  });
});

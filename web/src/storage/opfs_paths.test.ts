import { describe, expect, it } from "vitest";

import { OPFS_DISKS_PATH, type DiskImageMetadata } from "./metadata";
import { opfsOverlayPathForCow, opfsPathForDisk } from "./opfs_paths";

describe("opfs_paths", () => {
  it("returns disk path for a local disk with default directory", () => {
    const meta = {
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

    expect(opfsPathForDisk(meta)).toBe(`${OPFS_DISKS_PATH}/${meta.fileName}`);
  });

  it("returns disk path for a local disk with custom opfsDirectory", () => {
    const meta = {
      source: "local",
      id: "disk-2",
      name: "Legacy Disk",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "legacy.img",
      opfsDirectory: "images",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    expect(opfsPathForDisk(meta)).toBe(`images/${meta.fileName}`);
  });

  it("returns remote disk overlay path", () => {
    const meta = {
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

    expect(opfsOverlayPathForCow(meta)).toBe(`${OPFS_DISKS_PATH}/${meta.cache.overlayFileName}`);
  });

  it("returns local disk overlay path in custom opfsDirectory", () => {
    const meta = {
      source: "local",
      id: "disk-legacy",
      name: "Legacy Disk",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "legacy.img",
      opfsDirectory: "images",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    expect(opfsOverlayPathForCow(meta)).toBe(`images/${meta.id}.overlay.aerospar`);
  });

  it("throws on empty fileName", () => {
    const meta = {
      source: "local",
      id: "disk-3",
      name: "Disk 3",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    expect(() => opfsPathForDisk(meta)).toThrow(/fileName must not be empty/);
  });

  it("throws on empty id", () => {
    const meta = {
      source: "local",
      id: "",
      name: "Disk 4",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk-4.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    expect(() => opfsOverlayPathForCow(meta)).toThrow(/id must not be empty/);
  });

  it("throws on invalid opfsDirectory containing '..'", () => {
    const meta = {
      source: "local",
      id: "disk-5",
      name: "Disk 5",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk-5.img",
      opfsDirectory: "aero/../disks",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    expect(() => opfsPathForDisk(meta)).toThrow(/opfsDirectory must not contain/);
  });

  it("throws on fileName containing path separators", () => {
    const meta = {
      source: "local",
      id: "disk-6",
      name: "Disk 6",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "subdir/disk-6.img",
      sizeBytes: 512,
      createdAtMs: 0,
    } satisfies DiskImageMetadata;

    expect(() => opfsPathForDisk(meta)).toThrow(/simple file name/);
  });

  it("throws on remote disks when cache backend is not opfs", () => {
    const meta = {
      source: "remote",
      id: "remote-2",
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
        backend: "idb",
        fileName: "remote-2.cache.aerospar",
        overlayFileName: "remote-2.overlay.aerospar",
        overlayBlockSizeBytes: 1024 * 1024,
      },
    } satisfies DiskImageMetadata;

    expect(() => opfsOverlayPathForCow(meta)).toThrow(/OPFS-backed remote overlay/);
  });
});

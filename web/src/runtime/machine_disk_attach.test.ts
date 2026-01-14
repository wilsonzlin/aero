import { describe, expect, it } from "vitest";

import { planMachineBootDiskAttachment } from "./machine_disk_attach";
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


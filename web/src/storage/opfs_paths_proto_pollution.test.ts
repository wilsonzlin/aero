import { describe, expect, it } from "vitest";

import { OPFS_DISKS_PATH } from "./metadata";
import { opfsOverlayPathForCow, opfsPathForDisk } from "./opfs_paths";

describe("opfs_paths prototype pollution hardening", () => {
  it("does not observe inherited Object.prototype.opfsDirectory when deriving disk paths", () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "opfsDirectory");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "opfsDirectory", {
        value: "images",
        configurable: true,
        writable: true,
      });

      const meta: any = {
        source: "local",
        id: "d1",
        name: "Disk",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        fileName: "disk.img",
        sizeBytes: 512,
        createdAtMs: 0,
      };

      // Should still use the default OPFS_DISKS_PATH, not the inherited "images".
      expect(opfsPathForDisk(meta)).toBe(`${OPFS_DISKS_PATH}/disk.img`);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "opfsDirectory", existing);
      else Reflect.deleteProperty(Object.prototype, "opfsDirectory");
    }
  });

  it("does not use inherited Object.prototype.fileName when fileName is missing", () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "fileName");
    if (existing && existing.configurable === false) {
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "fileName", {
        value: "evil.img",
        configurable: true,
        writable: true,
      });

      const meta: any = {
        source: "local",
        id: "d1",
        name: "Disk",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        // fileName intentionally missing
        sizeBytes: 512,
        createdAtMs: 0,
      };

      expect(() => opfsPathForDisk(meta)).toThrow(/fileName must be a string/i);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "fileName", existing);
      else Reflect.deleteProperty(Object.prototype, "fileName");
    }
  });

  it("does not use inherited Object.prototype.cache when remote cache metadata is missing", () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "cache");
    if (existing && existing.configurable === false) {
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "cache", {
        value: { backend: "opfs", overlayFileName: "evil.overlay.aerospar" },
        configurable: true,
        writable: true,
      });

      const meta: any = {
        source: "remote",
        id: "r1",
        name: "Remote",
        kind: "hdd",
        format: "raw",
        sizeBytes: 512,
        createdAtMs: 0,
        remote: {
          imageId: "img",
          version: "1",
          delivery: "range",
          urls: { url: "/images/img/1" },
        },
        // cache intentionally missing
      };

      expect(() => opfsOverlayPathForCow(meta)).toThrow(/remote disk cache metadata/i);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "cache", existing);
      else Reflect.deleteProperty(Object.prototype, "cache");
    }
  });
});


import { describe, expect, it } from "vitest";

import { planLegacyOpfsImageAdoptions } from "./legacy_images";
import type { DiskImageMetadata } from "./metadata";

describe("planLegacyOpfsImageAdoptions prototype pollution hardening", () => {
  it("does not observe inherited Object.prototype.fileName when matching adopted legacy images", () => {
    const fileNameExisting = Object.getOwnPropertyDescriptor(Object.prototype, "fileName");
    if (fileNameExisting && fileNameExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "fileName", {
        value: "already.img",
        configurable: true,
        writable: true,
      });

      // Simulate a corrupt persisted metadata entry that claims to be a legacy-adopted disk
      // (`opfsDirectory: "images"`) but is missing an own `fileName`.
      const corruptExisting = {
        source: "local",
        id: "existing",
        name: "corrupt",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        opfsDirectory: "images",
        sizeBytes: 123,
        createdAtMs: 1,
      } as unknown as DiskImageMetadata;

      const next = planLegacyOpfsImageAdoptions({
        existingDisks: [corruptExisting],
        legacyFiles: [{ name: "already.img", sizeBytes: 123, lastModifiedMs: 111 }],
        nowMs: 999,
        newId: (() => {
          let i = 0;
          return () => `id_${++i}`;
        })(),
      });

      // Even with `Object.prototype.fileName` polluted, a missing own `fileName` must not cause the
      // corrupt disk record to block adopting `already.img` from the legacy images directory.
      expect(next).toHaveLength(1);
      expect(next[0]).toMatchObject({
        source: "local",
        name: "already.img",
        backend: "opfs",
        fileName: "already.img",
        opfsDirectory: "images",
        sizeBytes: 123,
        createdAtMs: 111,
      });
    } finally {
      if (fileNameExisting) Object.defineProperty(Object.prototype, "fileName", fileNameExisting);
      else Reflect.deleteProperty(Object.prototype, "fileName");
    }
  });
});


import { describe, expect, it } from "vitest";

import { planLegacyOpfsImageAdoptions } from "./legacy_images";
import type { DiskImageMetadata } from "./metadata";

describe("planLegacyOpfsImageAdoptions", () => {
  it("creates metadata for legacy OPFS images not yet adopted", () => {
    const existing: DiskImageMetadata[] = [
      {
        source: "local",
        id: "existing",
        name: "already.img",
        backend: "opfs",
        kind: "hdd",
        format: "raw",
        fileName: "already.img",
        opfsDirectory: "images",
        sizeBytes: 123,
        createdAtMs: 1,
      },
    ];

    const next = planLegacyOpfsImageAdoptions({
      existingDisks: existing,
      legacyFiles: [
        { name: "already.img", sizeBytes: 123, lastModifiedMs: 111 },
        { name: "win7.img", sizeBytes: 456, lastModifiedMs: 222 },
        { name: "installer.iso", sizeBytes: 789, lastModifiedMs: 333 },
      ],
      nowMs: 999,
      newId: (() => {
        let i = 0;
        return () => `id_${++i}`;
      })(),
    });

    expect(next).toHaveLength(2);
    expect(next[0]).toMatchObject({
      source: "local",
      id: "id_1",
      name: "win7.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "win7.img",
      opfsDirectory: "images",
      sizeBytes: 456,
      createdAtMs: 222,
    });
    expect(next[1]).toMatchObject({
      source: "local",
      id: "id_2",
      name: "installer.iso",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "installer.iso",
      opfsDirectory: "images",
      sizeBytes: 789,
      createdAtMs: 333,
    });
  });
});

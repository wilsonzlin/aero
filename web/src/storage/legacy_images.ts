import { inferFormatFromFileName, inferKindFromFileName, OPFS_LEGACY_IMAGES_DIR, OPFS_DISKS_PATH, type DiskImageMetadata } from "./metadata";

export type LegacyOpfsFile = {
  name: string;
  sizeBytes: number;
  lastModifiedMs?: number;
};

function opfsDirFor(meta: DiskImageMetadata): string {
  if (meta.source !== "local") return OPFS_DISKS_PATH;
  return meta.opfsDirectory ?? OPFS_DISKS_PATH;
}

export function planLegacyOpfsImageAdoptions(options: {
  existingDisks: DiskImageMetadata[];
  legacyFiles: LegacyOpfsFile[];
  nowMs: number;
  newId: () => string;
}): DiskImageMetadata[] {
  const existingKeys = new Set<string>();
  for (const d of options.existingDisks) {
    if (d.source !== "local") continue;
    if (d.backend !== "opfs") continue;
    if (opfsDirFor(d) !== OPFS_LEGACY_IMAGES_DIR) continue;
    existingKeys.add(`${OPFS_LEGACY_IMAGES_DIR}/${d.fileName}`);
  }

  const out: DiskImageMetadata[] = [];
  for (const f of options.legacyFiles) {
    const key = `${OPFS_LEGACY_IMAGES_DIR}/${f.name}`;
    if (existingKeys.has(key)) continue;

    const format = inferFormatFromFileName(f.name);
    const kind = inferKindFromFileName(f.name);
    out.push({
      source: "local",
      id: options.newId(),
      name: f.name,
      backend: "opfs",
      kind,
      format,
      fileName: f.name,
      opfsDirectory: OPFS_LEGACY_IMAGES_DIR,
      sizeBytes: f.sizeBytes,
      createdAtMs: f.lastModifiedMs ?? options.nowMs,
      lastUsedAtMs: undefined,
      checksum: undefined,
      sourceFileName: f.name,
    });
  }

  return out;
}

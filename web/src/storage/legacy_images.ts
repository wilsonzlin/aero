import { inferFormatFromFileName, inferKindFromFileName, OPFS_LEGACY_IMAGES_DIR, OPFS_DISKS_PATH, type DiskImageMetadata } from "./metadata";

export type LegacyOpfsFile = {
  name: string;
  sizeBytes: number;
  lastModifiedMs?: number;
};

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function ownString(obj: object, key: string): string | undefined {
  const rec = obj as Record<string, unknown>;
  const value = hasOwn(rec, key) ? rec[key] : undefined;
  return typeof value === "string" ? value : undefined;
}

function opfsDirFor(meta: DiskImageMetadata): string {
  // Treat metadata as untrusted: ignore inherited discriminants/fields (prototype pollution).
  const rec = meta as unknown as Record<string, unknown>;
  const source = hasOwn(rec, "source") ? rec.source : undefined;
  if (source !== "local") return OPFS_DISKS_PATH;
  const dirRaw = hasOwn(rec, "opfsDirectory") ? (rec as { opfsDirectory?: unknown }).opfsDirectory : undefined;
  const dir = typeof dirRaw === "string" && dirRaw.trim() ? dirRaw : undefined;
  return dir ?? OPFS_DISKS_PATH;
}

export function planLegacyOpfsImageAdoptions(options: {
  existingDisks: DiskImageMetadata[];
  legacyFiles: LegacyOpfsFile[];
  nowMs: number;
  newId: () => string;
}): DiskImageMetadata[] {
  const existingKeys = new Set<string>();
  for (const d of options.existingDisks) {
    // Treat metadata as untrusted: ignore inherited fields (prototype pollution).
    const rec = d as unknown as Record<string, unknown>;
    const source = hasOwn(rec, "source") ? rec.source : undefined;
    if (source !== "local") continue;
    const backend = hasOwn(rec, "backend") ? rec.backend : undefined;
    if (backend !== "opfs") continue;
    if (opfsDirFor(d) !== OPFS_LEGACY_IMAGES_DIR) continue;
    const fileName = ownString(rec, "fileName");
    if (!fileName) continue;
    existingKeys.add(`${OPFS_LEGACY_IMAGES_DIR}/${fileName}`);
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

import type { DiskImageMetadata, MountConfig } from "../storage/metadata";

/**
 * `postMessage` payload used to configure the boot disks for the VM workers.
 *
 * Kept in a shared module so multiple worker implementations (legacy IO worker,
 * machine CPU worker, etc.) can share a single schema definition.
 */
export type SetBootDisksMessage = {
  type: "setBootDisks";
  mounts: MountConfig;
  hdd: DiskImageMetadata | null;
  cd: DiskImageMetadata | null;
};

/**
 * Best-effort parser for untrusted `postMessage` data.
 *
 * - Accepts missing/invalid fields (normalizes to `{ mounts: {}, hdd: null, cd: null }`).
 * - Does not deeply validate DiskImageMetadata (the schema is large); it only ensures objects are
 *   object-like so downstream code doesn't accidentally treat e.g. a string as metadata.
 */
export function normalizeSetBootDisksMessage(msg: unknown): SetBootDisksMessage | null {
  if (!msg || typeof msg !== "object") return null;
  const rec = msg as Partial<SetBootDisksMessage> & { type?: unknown };
  if (rec.type !== "setBootDisks") return null;

  const mountsRaw = rec.mounts;
  const mounts = (mountsRaw && typeof mountsRaw === "object" ? mountsRaw : {}) as MountConfig;

  const hddRaw = (rec as { hdd?: unknown }).hdd;
  const cdRaw = (rec as { cd?: unknown }).cd;
  const hdd = (hddRaw && typeof hddRaw === "object" ? (hddRaw as DiskImageMetadata) : null) as DiskImageMetadata | null;
  const cd = (cdRaw && typeof cdRaw === "object" ? (cdRaw as DiskImageMetadata) : null) as DiskImageMetadata | null;

  return { type: "setBootDisks", mounts, hdd, cd };
}

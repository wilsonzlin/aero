import type { DiskImageMetadata, MountConfig } from "../storage/metadata";
import { opfsOverlayPathForCow, opfsPathForDisk } from "../storage/opfs_paths";

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
  /**
   * Optional boot-device preference for the canonical machine runtime.
   *
   * This allows keeping install media mounted while still booting from HDD after the first install
   * reboot.
   *
   * Note: this represents the *requested policy* ("try to boot from CD" vs "boot from HDD"). The
   * firmware may still fall back (for example, if the CD is unbootable under the "CD-first when
   * present" policy). Runtimes that need to know what firmware actually booted from should use the
   * machine runtime's active-boot-device reporting instead.
   */
  bootDevice?: "hdd" | "cdrom";
};

export function emptySetBootDisksMessage(): SetBootDisksMessage {
  // Use a null-prototype mounts record so callers never observe inherited IDs (e.g. if
  // `Object.prototype.hddId` is polluted).
  return { type: "setBootDisks", mounts: Object.create(null) as MountConfig, hdd: null, cd: null };
}

function isObjectLikeRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

/**
 * Best-effort parser for untrusted `postMessage` data.
 *
 * - Accepts missing/invalid fields (normalizes to `{ mounts: {}, hdd: null, cd: null }`).
 * - Sanitizes mount IDs to strings and copies them into a fresh `{}` object to avoid prototype
 *   pollution / unexpected value types.
 * - Does not deeply validate DiskImageMetadata (the schema is large); it only applies minimal
 *   shape checks (`source`, `id`, and `kind`) so downstream code doesn't accidentally treat
 *   unrelated objects as metadata.
  */
export function normalizeSetBootDisksMessage(msg: unknown): SetBootDisksMessage | null {
  if (!isObjectLikeRecord(msg)) return null;
  const rec = msg as Record<string, unknown>;
  if (!hasOwn(rec, "type")) return null;
  if (rec.type !== "setBootDisks") return null;

  // Mount IDs are the only fields used outside the disk metadata. Normalize to a plain object and
  // accept only string values so downstream code can treat them as opaque IDs without re-validating.
  const mountsRaw = hasOwn(rec, "mounts") ? rec.mounts : undefined;
  // Null prototype prevents inherited IDs from being observed if the global prototype is polluted.
  const mounts: MountConfig = Object.create(null) as MountConfig;
  const sanitizeMountId = (value: unknown): string | undefined => {
    if (typeof value !== "string") return undefined;
    const trimmed = value.trim();
    return trimmed ? trimmed : undefined;
  };
  if (isObjectLikeRecord(mountsRaw)) {
    const raw = mountsRaw as Record<string, unknown>;
    const hddId = hasOwn(raw, "hddId") ? sanitizeMountId(raw.hddId) : undefined;
    if (hddId) mounts.hddId = hddId;
    const cdId = hasOwn(raw, "cdId") ? sanitizeMountId(raw.cdId) : undefined;
    if (cdId) mounts.cdId = cdId;
  }

  const normalizeDiskMeta = (raw: unknown, expectedKind: "hdd" | "cd"): DiskImageMetadata | null => {
    if (!isObjectLikeRecord(raw)) return null;
    const meta = raw as Record<string, unknown>;
    const source = hasOwn(meta, "source") ? meta.source : undefined;
    if (source !== "local" && source !== "remote") return null;
    const id = hasOwn(meta, "id") ? meta.id : undefined;
    if (typeof id !== "string" || !id.trim()) return null;
    const kind = hasOwn(meta, "kind") ? meta.kind : undefined;
    if (kind !== expectedKind) return null;
    return raw as DiskImageMetadata;
  };

  const hddRaw = hasOwn(rec, "hdd") ? rec.hdd : undefined;
  const cdRaw = hasOwn(rec, "cd") ? rec.cd : undefined;
  const hdd = normalizeDiskMeta(hddRaw, "hdd");
  const cd = normalizeDiskMeta(cdRaw, "cd");

  const bootDeviceRaw = hasOwn(rec, "bootDevice") ? rec.bootDevice : undefined;
  const bootDevice = bootDeviceRaw === "hdd" || bootDeviceRaw === "cdrom" ? bootDeviceRaw : undefined;

  return bootDevice ? { type: "setBootDisks", mounts, hdd, cd, bootDevice } : { type: "setBootDisks", mounts, hdd, cd };
}

// -----------------------------------------------------------------------------
// Machine-runtime boot disk validation (pure, unit-testable)
// -----------------------------------------------------------------------------

// Default aerosparse block size used when creating primary HDD copy-on-write overlays.
//
// This matches the RuntimeDiskWorker default (`runtime_disk_worker_impl.ts`) so overlays created
// by legacy/IO paths remain compatible with the wasm `Machine.set_primary_hdd_opfs_cow` helper.
export const DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES = 1024 * 1024; // 1 MiB

export type MachineBootDisksOpfsSpec = Readonly<{
  /**
   * Canonical primary HDD attachment spec (`disk_id=0`, AHCI port 0).
   *
   * When null, the machine should detach/replace the primary HDD with an empty in-memory disk.
   */
  hdd:
    | Readonly<{ meta: DiskImageMetadata; basePath: string; overlayPath: string; overlayBlockSizeBytes: number }>
    | null;
  /**
   * Canonical install media attachment spec (`disk_id=1`, IDE secondary master ATAPI).
   *
   * When null, the machine should eject/detach the install media.
   */
  cd: Readonly<{ meta: DiskImageMetadata; path: string }> | null;
  /**
   * BIOS boot drive number (`DL`) to use after applying the disk attachments.
   *
   * - `0xE0`: CD-ROM boot (El Torito)
   * - `0x80`: primary HDD boot
   */
  bootDrive: number;
}>;

function formatDiskMeta(meta: DiskImageMetadata): string {
  const anyMeta = meta as unknown as {
    id?: unknown;
    name?: unknown;
    backend?: unknown;
    source?: unknown;
    kind?: unknown;
    format?: unknown;
  };
  const id = typeof anyMeta.id === "string" ? anyMeta.id : "?";
  const name = typeof anyMeta.name === "string" ? anyMeta.name : "?";
  const source = typeof anyMeta.source === "string" ? anyMeta.source : "?";
  const backend = typeof anyMeta.backend === "string" ? anyMeta.backend : "?";
  const kind = typeof anyMeta.kind === "string" ? anyMeta.kind : "?";
  const format = typeof anyMeta.format === "string" ? anyMeta.format : "?";
  return `id=${id}, name=${name}, source=${source}, backend=${backend}, kind=${kind}, format=${format}`;
}

function assertMachineRuntimeLocalOpfsDisk(meta: DiskImageMetadata, label: string): void {
  if (!isObjectLikeRecord(meta)) {
    throw new Error(`${label}: invalid disk metadata (expected an object)`);
  }
  const rec = meta as unknown as Record<string, unknown>;
  const source = hasOwn(rec, "source") ? rec.source : undefined;
  if (source === "remote") {
    throw new Error(`${label}: remote disks are not supported in machine runtime (${formatDiskMeta(meta)})`);
  }
  if (source !== "local") {
    throw new Error(`${label}: expected a local disk (${formatDiskMeta(meta)})`);
  }
  // Legacy local-disk schema allowed remote streaming via `meta.remote`. Reject to avoid opening
  // network-backed disks in machine runtime until explicit support is implemented. Treat metadata
  // as untrusted: only observe `remote` when it is an own property (ignore `Object.prototype.remote`
  // pollution).
  const legacyRemote = hasOwn(rec, "remote") ? rec.remote : undefined;
  if (legacyRemote) {
    // Legacy local-disk schema allowed remote streaming via `meta.remote`. Reject to avoid opening
    // network-backed disks in machine runtime until explicit support is implemented.
    throw new Error(`${label}: remote-streaming disks are not supported in machine runtime (${formatDiskMeta(meta)})`);
  }
  const backend = hasOwn(rec, "backend") ? rec.backend : undefined;
  if (backend !== "opfs") {
    throw new Error(
      `${label}: only OPFS-backed disks are supported in machine runtime (${formatDiskMeta(meta)})`,
    );
  }
}

/**
 * Validate a `SetBootDisksMessage` for the canonical machine runtime and return the OPFS path
 * strings needed by the wasm `api.Machine` attachment APIs.
 *
 * This function is intentionally pure (no OPFS APIs) so it can be unit-tested.
 */
export function machineBootDisksToOpfsSpec(
  msg: SetBootDisksMessage,
  opts: { overlayBlockSizeBytes?: number } = {},
): MachineBootDisksOpfsSpec {
  const overlayBlockSizeBytes =
    typeof opts.overlayBlockSizeBytes === "number" && Number.isFinite(opts.overlayBlockSizeBytes) && opts.overlayBlockSizeBytes > 0
      ? (opts.overlayBlockSizeBytes >>> 0)
      : DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES;

  let hdd: MachineBootDisksOpfsSpec["hdd"] = null;
  if (msg.hdd) {
    const meta = msg.hdd;
    const label = "bootDisks.hdd";
    assertMachineRuntimeLocalOpfsDisk(meta, label);
    const metaRec = meta as unknown as Record<string, unknown>;
    const kind = hasOwn(metaRec, "kind") ? metaRec.kind : undefined;
    if (kind !== "hdd") {
      throw new Error(`${label}: expected kind=\"hdd\" (${formatDiskMeta(meta)})`);
    }
    const format = hasOwn(metaRec, "format") ? metaRec.format : undefined;
    if (format === "unknown") {
      throw new Error(`${label}: requires explicit HDD format metadata (disk format=unknown) (${formatDiskMeta(meta)})`);
    }
    if (format !== "raw" && format !== "aerospar") {
      // The machine runtime supports OPFS-backed base images that can be opened synchronously by
      // Rust storage controllers. Today that is:
      // - raw sector files, and
      // - `aero_storage` aerosparse images (`.aerospar`).
      throw new Error(`${label}: unsupported format (expected \"raw\" or \"aerospar\") (${formatDiskMeta(meta)})`);
    }
    const basePath = opfsPathForDisk(meta);
    const overlayPath = opfsOverlayPathForCow(meta);
    hdd = { meta, basePath, overlayPath, overlayBlockSizeBytes };
  }

  let cd: MachineBootDisksOpfsSpec["cd"] = null;
  if (msg.cd) {
    const meta = msg.cd;
    const label = "bootDisks.cd";
    assertMachineRuntimeLocalOpfsDisk(meta, label);
    const metaRec = meta as unknown as Record<string, unknown>;
    const kind = hasOwn(metaRec, "kind") ? metaRec.kind : undefined;
    if (kind !== "cd") {
      throw new Error(`${label}: expected kind=\"cd\" (${formatDiskMeta(meta)})`);
    }
    const format = hasOwn(metaRec, "format") ? metaRec.format : undefined;
    if (format !== "iso") {
      throw new Error(`${label}: unsupported format (expected \"iso\") (${formatDiskMeta(meta)})`);
    }
    cd = { meta, path: opfsPathForDisk(meta) };
  }

  const bootDrive =
    msg.bootDevice === "cdrom" && cd
      ? 0xe0
      : msg.bootDevice === "hdd" && hdd
        ? 0x80
        : cd
          ? 0xe0
          : 0x80;
  return { hdd, cd, bootDrive };
}

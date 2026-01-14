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
};

export function emptySetBootDisksMessage(): SetBootDisksMessage {
  return { type: "setBootDisks", mounts: {}, hdd: null, cd: null };
}

function isObjectLikeRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

/**
 * Best-effort parser for untrusted `postMessage` data.
 *
 * - Accepts missing/invalid fields (normalizes to `{ mounts: {}, hdd: null, cd: null }`).
 * - Does not deeply validate DiskImageMetadata (the schema is large); it only ensures objects are
 *   object-like so downstream code doesn't accidentally treat e.g. a string as metadata.
 */
export function normalizeSetBootDisksMessage(msg: unknown): SetBootDisksMessage | null {
  if (!isObjectLikeRecord(msg)) return null;
  const rec = msg as Partial<SetBootDisksMessage> & { type?: unknown };
  if (rec.type !== "setBootDisks") return null;

  // Mount IDs are the only fields used outside the disk metadata. Normalize to a plain object and
  // accept only string values so downstream code can treat them as opaque IDs without re-validating.
  const mountsRaw = (rec as { mounts?: unknown }).mounts;
  const mounts: MountConfig = {};
  if (isObjectLikeRecord(mountsRaw)) {
    const raw = mountsRaw as { hddId?: unknown; cdId?: unknown };
    if (typeof raw.hddId === "string") mounts.hddId = raw.hddId;
    if (typeof raw.cdId === "string") mounts.cdId = raw.cdId;
  }

  const hddRaw = (rec as { hdd?: unknown }).hdd;
  const cdRaw = (rec as { cd?: unknown }).cd;
  const hdd = (isObjectLikeRecord(hddRaw) ? (hddRaw as DiskImageMetadata) : null) as DiskImageMetadata | null;
  const cd = (isObjectLikeRecord(cdRaw) ? (cdRaw as DiskImageMetadata) : null) as DiskImageMetadata | null;

  return { type: "setBootDisks", mounts, hdd, cd };
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
  const anyMeta = meta as unknown as { source?: unknown; backend?: unknown; remote?: unknown };
  if (anyMeta.source === "remote") {
    throw new Error(`${label}: remote disks are not supported in machine runtime (${formatDiskMeta(meta)})`);
  }
  if (anyMeta.source !== "local") {
    throw new Error(`${label}: expected a local disk (${formatDiskMeta(meta)})`);
  }
  if (anyMeta.remote) {
    // Legacy local-disk schema allowed remote streaming via `meta.remote`. Reject to avoid opening
    // network-backed disks in machine runtime until explicit support is implemented.
    throw new Error(`${label}: remote-streaming disks are not supported in machine runtime (${formatDiskMeta(meta)})`);
  }
  if (anyMeta.backend !== "opfs") {
    throw new Error(`${label}: only OPFS-backed disks are supported in machine runtime (${formatDiskMeta(meta)})`);
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
    const anyMeta = meta as unknown as { kind?: unknown; format?: unknown };
    if (anyMeta.kind !== "hdd") {
      throw new Error(`${label}: expected kind=\"hdd\" (${formatDiskMeta(meta)})`);
    }
    if (anyMeta.format !== "raw" && anyMeta.format !== "aerospar" && anyMeta.format !== "unknown") {
      // The machine runtime supports OPFS-backed base images that can be opened synchronously by
      // Rust storage controllers. Today that is:
      // - raw sector files, and
      // - `aero_storage` aerosparse images (`.aerospar`).
      //
      // Treat `format="unknown"` as "assume raw" for back-compat with older metadata schemas.
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
    const anyMeta = meta as unknown as { kind?: unknown; format?: unknown };
    if (anyMeta.kind !== "cd") {
      throw new Error(`${label}: expected kind=\"cd\" (${formatDiskMeta(meta)})`);
    }
    if (anyMeta.format !== "iso") {
      throw new Error(`${label}: unsupported format (expected \"iso\") (${formatDiskMeta(meta)})`);
    }
    cd = { meta, path: opfsPathForDisk(meta) };
  }

  const bootDrive = cd ? 0xe0 : 0x80;
  return { hdd, cd, bootDrive };
}

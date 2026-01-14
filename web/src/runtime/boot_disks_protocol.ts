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
    if (anyMeta.format !== "raw") {
      // `Machine.set_primary_hdd_opfs_cow` uses `RawDisk::open` for the base image; reject other
      // formats rather than guessing.
      throw new Error(`${label}: unsupported format (expected \"raw\") (${formatDiskMeta(meta)})`);
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

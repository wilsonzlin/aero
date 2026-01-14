import type { WasmApi } from "./wasm_loader";

export type MachineSnapshotDiskOverlayRef = Readonly<{
  disk_id: number;
  base_image: string;
  overlay_image: string;
}>;

const DEFAULT_COW_OVERLAY_BLOCK_SIZE_BYTES = 1024 * 1024;
const AEROSPARSE_HEADER_SIZE_BYTES = 64;
const AEROSPARSE_MAGIC = [0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52] as const; // "AEROSPAR"

function formatPrefix(prefix: string | undefined): string {
  if (!prefix) return "[machine.snapshot]";
  if (prefix.startsWith("[")) return prefix;
  return `[${prefix}]`;
}

function isPowerOfTwo(n: number): boolean {
  return n > 0 && (n & (n - 1)) === 0;
}

async function tryReadAerosparseBlockSizeBytesFromOpfs(path: string, prefix: string): Promise<number | null> {
  if (!path) return null;
  // In CI/unit tests there is no `navigator` / OPFS environment. Treat this as best-effort.
  const storage = (globalThis as unknown as { navigator?: unknown }).navigator as { storage?: unknown } | undefined;
  const getDirectory = (storage?.storage as { getDirectory?: unknown } | undefined)?.getDirectory;
  if (typeof getDirectory !== "function") return null;

  // Overlay refs are expected to be relative OPFS paths. Refuse to interpret `..` to avoid path traversal.
  const parts = path.split("/").filter((p) => p && p !== ".");
  if (parts.length === 0 || parts.some((p) => p === "..")) return null;

  try {
    let dir = (await (getDirectory as () => Promise<FileSystemDirectoryHandle>)()) as FileSystemDirectoryHandle;
    for (const part of parts.slice(0, -1)) {
      dir = await dir.getDirectoryHandle(part, { create: false });
    }
    const file = await dir.getFileHandle(parts[parts.length - 1]!, { create: false }).then((h) => h.getFile());
    if (file.size < AEROSPARSE_HEADER_SIZE_BYTES) return null;
    const buf = await file.slice(0, AEROSPARSE_HEADER_SIZE_BYTES).arrayBuffer();
    if (buf.byteLength < AEROSPARSE_HEADER_SIZE_BYTES) return null;
    const bytes = new Uint8Array(buf);
    for (let i = 0; i < AEROSPARSE_MAGIC.length; i += 1) {
      if (bytes[i] !== AEROSPARSE_MAGIC[i]) return null;
    }
    const dv = new DataView(buf);
    const version = dv.getUint32(8, true);
    const headerSize = dv.getUint32(12, true);
    const blockSizeBytes = dv.getUint32(16, true);
    if (version !== 1 || headerSize !== AEROSPARSE_HEADER_SIZE_BYTES) return null;
    // Mirror the Rust-side aerosparse header validation (looser, but enough to avoid nonsense).
    if (blockSizeBytes === 0 || blockSizeBytes % 512 !== 0 || !isPowerOfTwo(blockSizeBytes) || blockSizeBytes > 64 * 1024 * 1024) {
      return null;
    }
    return blockSizeBytes;
  } catch (err) {
    console.warn(`${prefix} Failed to read aerosparse overlay header from OPFS path=${path}:`, err);
    return null;
  }
}

function isMachineSnapshotDiskOverlayRef(value: unknown): value is MachineSnapshotDiskOverlayRef {
  if (!value || typeof value !== "object") return false;
  const rec = value as { disk_id?: unknown; base_image?: unknown; overlay_image?: unknown };
  return typeof rec.disk_id === "number" && typeof rec.base_image === "string" && typeof rec.overlay_image === "string";
}

async function callMaybeAsync(fn: (...args: unknown[]) => unknown, thisArg: unknown, args: unknown[]): Promise<void> {
  // Support both sync and async wasm-bindgen bindings.
  await Promise.resolve(fn.apply(thisArg, args));
}

/**
 * Reattach OPFS-backed disks referenced by a restored `aero-snapshot` `DISKS` section.
 *
 * Snapshot restore intentionally drops host-side disk backends (e.g. open OPFS sync access handles)
 * because they cannot be serialized. The snapshot file only persists *overlay refs* (strings),
 * which the host runtime must interpret and reopen.
 *
 * Overlay ref contract (web runtime):
 * - `base_image` and `overlay_image` are OPFS-relative paths (e.g. `"aero/disks/win7.base"`),
 *   suitable for passing directly to the Machine's `*_opfs_*` attachment APIs.
 * - The strings are *paths*, not opaque IDs. They are relative to `navigator.storage.getDirectory()`.
 * - Empty strings indicate "no backend configured" and should be ignored (the snapshot format may
 *   still emit placeholder entries for canonical disk slots).
 */
export async function reattachMachineSnapshotDisks(opts: {
  api: WasmApi;
  machine: InstanceType<WasmApi["Machine"]>;
  logPrefix?: string;
}): Promise<void> {
  const prefix = formatPrefix(opts.logPrefix);
  const machine = opts.machine;

  // Prefer the Rust-side helper when available: it understands the canonical Win7 `disk_id` mapping,
  // can reopen both raw disk images and COW overlays, and does not require JS to know the
  // `overlayBlockSizeBytes` used when creating an aerosparse overlay.
  const reattachFromOpfs = (machine as unknown as { reattach_restored_disks_from_opfs?: unknown }).reattach_restored_disks_from_opfs;
  if (typeof reattachFromOpfs === "function") {
    await callMaybeAsync(reattachFromOpfs as (...args: unknown[]) => unknown, machine, []);
    return;
  }

  const take = (machine as unknown as { take_restored_disk_overlays?: unknown }).take_restored_disk_overlays;
  if (typeof take !== "function") return;

  const raw = (take as () => unknown).call(machine);
  if (raw == null) return;
  if (!Array.isArray(raw) || raw.length === 0) return;

  const diskIdPrimaryRaw = opts.api.Machine.disk_id_primary_hdd?.();
  const diskIdInstallRaw = opts.api.Machine.disk_id_install_media?.();
  // Older WASM builds may not expose `Machine.disk_id_*` helpers yet. The snapshot format's disk_id
  // mapping for the canonical Win7 storage topology is stable (0=primary HDD, 1=install media,
  // 2=IDE primary master), so fall back to those values when needed.
  const diskIdPrimary = typeof diskIdPrimaryRaw === "number" ? diskIdPrimaryRaw : 0;
  const diskIdInstall = typeof diskIdInstallRaw === "number" ? diskIdInstallRaw : 1;
  const diskIdIdeRaw = opts.api.Machine.disk_id_ide_primary_master?.();
  // `disk_id_ide_primary_master` was added later than the other helpers. If it is unavailable,
  // fall back to the stable contract value (2) when it does not conflict with the other IDs.
  const diskIdIde =
    typeof diskIdIdeRaw === "number"
      ? diskIdIdeRaw
      : (diskIdPrimary >>> 0) !== 2 && (diskIdInstall >>> 0) !== 2
        ? 2
        : null;

  const setPrimary =
    (machine as unknown as { set_primary_hdd_opfs_cow?: unknown }).set_primary_hdd_opfs_cow ??
    // Back-compat shim: some builds may expose a different spelling; keep this list small and
    // update alongside the wasm-bindgen surface.
    (machine as unknown as { setPrimaryHddOpfsCow?: unknown }).setPrimaryHddOpfsCow;
  const setPrimaryNeedsBlockSize = typeof setPrimary === "function" && setPrimary.length >= 3;

  const setDiskExisting =
    (machine as unknown as { set_disk_opfs_existing?: unknown }).set_disk_opfs_existing ??
    (machine as unknown as { setDiskOpfsExisting?: unknown }).setDiskOpfsExisting;

  const attachIso =
    // Prefer restore-aware helpers when available; they preserve guest-visible ATAPI media state.
    (machine as unknown as { attach_install_media_iso_opfs_for_restore?: unknown }).attach_install_media_iso_opfs_for_restore ??
    (machine as unknown as { attachInstallMediaIsoOpfsForRestore?: unknown }).attachInstallMediaIsoOpfsForRestore ??
    // Back-compat: some builds expose a dedicated `_existing` helper for the same restore-aware ISO attachment path.
    (machine as unknown as { attach_install_media_iso_opfs_existing?: unknown }).attach_install_media_iso_opfs_existing ??
    (machine as unknown as { attach_install_media_iso_opfs_existing_and_set_overlay_ref?: unknown })
      .attach_install_media_iso_opfs_existing_and_set_overlay_ref ??
    (machine as unknown as { attach_install_media_iso_opfs_for_restore_and_set_overlay_ref?: unknown })
      .attach_install_media_iso_opfs_for_restore_and_set_overlay_ref ??
    (machine as unknown as { attachInstallMediaIsoOpfsForRestoreAndSetOverlayRef?: unknown })
      .attachInstallMediaIsoOpfsForRestoreAndSetOverlayRef ??
    // Generic attach helpers (new + legacy spellings).
    (machine as unknown as { attach_install_media_iso_opfs?: unknown }).attach_install_media_iso_opfs ??
    (machine as unknown as { attachInstallMediaIsoOpfs?: unknown }).attachInstallMediaIsoOpfs ??
    (machine as unknown as { attach_install_media_opfs_iso?: unknown }).attach_install_media_opfs_iso ??
    (machine as unknown as { attachInstallMediaOpfsIso?: unknown }).attachInstallMediaOpfsIso;
  const attachIdePrimaryMaster =
    (machine as unknown as { attach_ide_primary_master_disk_opfs_existing?: unknown }).attach_ide_primary_master_disk_opfs_existing ??
    (machine as unknown as { attachIdePrimaryMasterDiskOpfsExisting?: unknown }).attachIdePrimaryMasterDiskOpfsExisting ??
    (machine as unknown as { attach_ide_primary_master_disk_opfs_existing_and_set_overlay_ref?: unknown })
      .attach_ide_primary_master_disk_opfs_existing_and_set_overlay_ref ??
    (machine as unknown as { attachIdePrimaryMasterDiskOpfsExistingAndSetOverlayRef?: unknown })
      .attachIdePrimaryMasterDiskOpfsExistingAndSetOverlayRef;

  for (const entry of raw) {
    if (!isMachineSnapshotDiskOverlayRef(entry)) {
      console.warn(`${prefix} Ignoring malformed restored disk overlay ref entry:`, entry);
      continue;
    }

    const diskId = entry.disk_id >>> 0;
    const base = entry.base_image;
    const overlay = entry.overlay_image;

    // Treat empty `{base_image, overlay_image}` as "slot unused" instead of as an error.
    // The canonical machine snapshot format emits placeholder entries for some disk_ids.
    if (!base) {
      if (!overlay) continue;
      console.warn(
        `${prefix} Ignoring restored disk overlay ref for disk_id=${diskId} with empty base_image and non-empty overlay_image=${overlay}.`,
      );
      continue;
    }

    if (diskId === (diskIdPrimary >>> 0)) {
      if (typeof setPrimary !== "function") {
        throw new Error(
          "Snapshot restore requires Machine.set_primary_hdd_opfs_cow(base_image, overlay_image) but it is unavailable in this WASM build.",
        );
      }
      if (!overlay) {
        if (typeof setDiskExisting !== "function") {
          throw new Error(
            "Snapshot restore requires Machine.set_disk_opfs_existing(path) to reattach a base-only primary disk, but it is unavailable in this WASM build.",
          );
        }
        await callMaybeAsync(setDiskExisting as (...args: unknown[]) => unknown, machine, [base]);
        continue;
      }
      const args: unknown[] = [base, overlay];
      if (setPrimaryNeedsBlockSize) {
        const blockSizeBytes =
          (await tryReadAerosparseBlockSizeBytesFromOpfs(overlay, prefix)) ?? DEFAULT_COW_OVERLAY_BLOCK_SIZE_BYTES;
        args.push(blockSizeBytes);
      }
      await callMaybeAsync(setPrimary as (...args: unknown[]) => unknown, machine, args);
      continue;
    }

    if (diskId === (diskIdInstall >>> 0)) {
      if (typeof attachIso !== "function") {
        throw new Error(
          "Snapshot restore requires an install-media ISO OPFS attach export (e.g. Machine.attach_install_media_iso_opfs, Machine.attach_install_media_iso_opfs_existing, Machine.attach_install_media_opfs_iso, or *_for_restore variants) but none are available in this WASM build.",
        );
      }
      if (overlay) {
        // The canonical install-media ISO is read-only. Preserve the `DISKS` overlay ref entry for
        // forward compatibility, but ignore `overlay_image` when using the ISO attach helper.
        console.warn(
          `${prefix} Ignoring overlay_image for install media disk_id=${diskId} (base_image=${base}, overlay_image=${overlay}).`,
        );
      }
      await callMaybeAsync(attachIso as (...args: unknown[]) => unknown, machine, [base]);
      continue;
    }

    if (diskIdIde != null && diskId === (diskIdIde >>> 0)) {
      if (typeof attachIdePrimaryMaster !== "function") {
        throw new Error(
          "Snapshot restore requires Machine.attach_ide_primary_master_disk_opfs_existing(path) to reattach the IDE primary master disk, but it is unavailable in this WASM build.",
        );
      }
      if (overlay) {
        // Unlike install-media (read-only ISO), IDE primary master overlays must be preserved for correctness.
        // Older wasm builds without `reattach_restored_disks_from_opfs()` do not expose a JS attachment API for
        // IDE primary master COW overlays, so fail loudly rather than silently dropping guest writes.
        throw new Error(
          `${prefix} Snapshot restore reported an IDE primary master overlay for disk_id=${diskId}, but this WASM build cannot reattach it (missing Machine.reattach_restored_disks_from_opfs).`,
        );
      }
      await callMaybeAsync(attachIdePrimaryMaster as (...args: unknown[]) => unknown, machine, [base]);
      continue;
    }

    console.warn(
      `${prefix} Snapshot restore reported an unknown disk_id=${diskId} (base_image=${base}, overlay_image=${overlay}); ignoring.`,
    );
  }
}

/**
 * Restore a snapshot from OPFS and reattach any disks referenced by its `DISKS` section.
 */
export async function restoreMachineSnapshotFromOpfsAndReattachDisks(opts: {
  api: WasmApi;
  machine: InstanceType<WasmApi["Machine"]>;
  path: string;
  logPrefix?: string;
}): Promise<void> {
  const prefix = formatPrefix(opts.logPrefix);
  const machine = opts.machine;

  const restore = (machine as unknown as { restore_snapshot_from_opfs?: unknown }).restore_snapshot_from_opfs;
  if (typeof restore !== "function") {
    throw new Error(`${prefix} Machine.restore_snapshot_from_opfs(path) is unavailable in this WASM build.`);
  }

  await callMaybeAsync(restore as (...args: unknown[]) => unknown, machine, [opts.path]);
  await reattachMachineSnapshotDisks(opts);
}

/**
 * Restore a snapshot from bytes and reattach any disks referenced by its `DISKS` section.
 */
export async function restoreMachineSnapshotAndReattachDisks(opts: {
  api: WasmApi;
  machine: InstanceType<WasmApi["Machine"]>;
  bytes: Uint8Array;
  logPrefix?: string;
}): Promise<void> {
  const prefix = formatPrefix(opts.logPrefix);
  const machine = opts.machine;

  const restore = (machine as unknown as { restore_snapshot?: unknown }).restore_snapshot;
  if (typeof restore !== "function") {
    throw new Error(`${prefix} Machine.restore_snapshot(bytes) is unavailable in this WASM build.`);
  }

  (restore as (bytes: Uint8Array) => void).call(machine, opts.bytes);
  await reattachMachineSnapshotDisks(opts);
}

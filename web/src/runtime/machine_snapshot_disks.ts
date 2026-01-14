import type { WasmApi } from "./wasm_loader";

export type MachineSnapshotDiskOverlayRef = Readonly<{
  disk_id: number;
  base_image: string;
  overlay_image: string;
}>;

function formatPrefix(prefix: string | undefined): string {
  if (!prefix) return "[machine.snapshot]";
  if (prefix.startsWith("[")) return prefix;
  return `[${prefix}]`;
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
 */
export async function reattachMachineSnapshotDisks(opts: {
  api: WasmApi;
  machine: InstanceType<WasmApi["Machine"]>;
  logPrefix?: string;
}): Promise<void> {
  const prefix = formatPrefix(opts.logPrefix);
  const machine = opts.machine;

  const take = (machine as unknown as { take_restored_disk_overlays?: unknown }).take_restored_disk_overlays;
  if (typeof take !== "function") return;

  const raw = (take as () => unknown).call(machine);
  if (raw == null) return;
  if (!Array.isArray(raw) || raw.length === 0) return;

  const diskIdPrimary = opts.api.Machine.disk_id_primary_hdd?.();
  const diskIdInstall = opts.api.Machine.disk_id_install_media?.();
  if (typeof diskIdPrimary !== "number" || typeof diskIdInstall !== "number") {
    throw new Error(
      "Machine snapshot restore produced DISKS overlay refs but Machine.disk_id_primary_hdd()/disk_id_install_media() are unavailable.",
    );
  }

  const setPrimary =
    (machine as unknown as { set_primary_hdd_opfs_cow?: unknown }).set_primary_hdd_opfs_cow ??
    // Back-compat shim: some builds may expose a different spelling; keep this list small and
    // update alongside the wasm-bindgen surface.
    (machine as unknown as { setPrimaryHddOpfsCow?: unknown }).setPrimaryHddOpfsCow;
  const attachIso =
    (machine as unknown as { attach_install_media_opfs_iso?: unknown }).attach_install_media_opfs_iso ??
    (machine as unknown as { attachInstallMediaOpfsIso?: unknown }).attachInstallMediaOpfsIso;

  for (const entry of raw) {
    if (!isMachineSnapshotDiskOverlayRef(entry)) {
      console.warn(`${prefix} Ignoring malformed restored disk overlay ref entry:`, entry);
      continue;
    }

    const diskId = entry.disk_id >>> 0;

    if (diskId === (diskIdPrimary >>> 0)) {
      if (typeof setPrimary !== "function") {
        throw new Error(
          "Snapshot restore requires Machine.set_primary_hdd_opfs_cow(base_image, overlay_image) but it is unavailable in this WASM build.",
        );
      }
      await callMaybeAsync(setPrimary as (...args: unknown[]) => unknown, machine, [entry.base_image, entry.overlay_image]);
      continue;
    }

    if (diskId === (diskIdInstall >>> 0)) {
      if (typeof attachIso !== "function") {
        throw new Error(
          "Snapshot restore requires Machine.attach_install_media_opfs_iso(path) but it is unavailable in this WASM build.",
        );
      }
      if (entry.overlay_image && entry.overlay_image !== entry.base_image) {
        // The canonical install-media ISO is read-only. Preserve the `DISKS` overlay ref entry for
        // forward compatibility, but ignore `overlay_image` when using the ISO attach helper.
        console.warn(
          `${prefix} Ignoring overlay_image for install media disk_id=${diskId} (base_image=${entry.base_image}, overlay_image=${entry.overlay_image}).`,
        );
      }
      await callMaybeAsync(attachIso as (...args: unknown[]) => unknown, machine, [entry.base_image]);
      continue;
    }

    console.warn(
      `${prefix} Snapshot restore reported an unknown disk_id=${diskId} (base_image=${entry.base_image}, overlay_image=${entry.overlay_image}); ignoring.`,
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

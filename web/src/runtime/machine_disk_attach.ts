import type { MachineHandle } from "./wasm_loader";
import type { DiskImageMetadata } from "../storage/metadata";
import { opfsOverlayPathForCow, opfsPathForDisk } from "../storage/opfs_paths";
import { DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES } from "./boot_disks_protocol";

export type MachineBootDiskRole = "hdd" | "cd";

export type MachineBootDiskPlan = {
  /** OPFS path string relative to OPFS root (e.g. "aero/disks/foo.img"). */
  opfsPath: string;
  /**
   * Format selected for the machine runtime disk attach call.
   *
   * Note: This is intentionally narrower than `DiskImageMetadata["format"]` because the machine
   * runtime currently only supports a subset of formats for synchronous Rust storage controllers.
   */
  format: "raw" | "aerospar" | "iso";
  /** Non-fatal warnings (e.g. unknown format assumptions). */
  warnings: string[];
};

const AEROSPARSE_HEADER_SIZE_BYTES = 64;
const AEROSPARSE_MAGIC = [0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52] as const; // "AEROSPAR"

function isPowerOfTwo(n: number): boolean {
  return n > 0 && (n & (n - 1)) === 0;
}

function alignUpBigInt(value: bigint, alignment: bigint): bigint {
  if (alignment <= 0n) return value;
  return ((value + alignment - 1n) / alignment) * alignment;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

async function callMaybeAsync(fn: (...args: unknown[]) => unknown, thisArg: unknown, args: unknown[]): Promise<void> {
  // Support both sync and async wasm-bindgen bindings.
  await Promise.resolve(fn.apply(thisArg, args));
}

function setAhciPort0DiskOverlayRef(machine: MachineHandle, base: string, overlay: string): void {
  const setRef =
    machine.set_ahci_port0_disk_overlay_ref ??
    (machine as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
  if (typeof setRef !== "function") return;
  (setRef as (base: string, overlay: string) => void).call(machine, base, overlay);
}

async function tryReadAerosparseBlockSizeBytesFromOpfs(path: string): Promise<number | null> {
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
    const diskSizeBytes = dv.getBigUint64(24, true);
    if (diskSizeBytes === 0n || diskSizeBytes % 512n !== 0n) return null;
    if (
      blockSizeBytes === 0 ||
      blockSizeBytes % 512 !== 0 ||
      !isPowerOfTwo(blockSizeBytes) ||
      blockSizeBytes > 64 * 1024 * 1024
    ) {
      return null;
    }

    const tableOffset = dv.getBigUint64(32, true);
    if (tableOffset !== 64n) return null;
    const tableEntries = dv.getBigUint64(40, true);
    const blockSizeBig = BigInt(blockSizeBytes);
    const expectedTableEntries = (diskSizeBytes + blockSizeBig - 1n) / blockSizeBig;
    if (tableEntries !== expectedTableEntries) return null;
    const dataOffset = dv.getBigUint64(48, true);
    const expectedDataOffset = alignUpBigInt(64n + tableEntries * 8n, blockSizeBig);
    if (dataOffset !== expectedDataOffset) return null;
    const allocatedBlocks = dv.getBigUint64(56, true);
    if (allocatedBlocks > tableEntries) return null;
    // Ensure the file is large enough to contain the advertised data region.
    // (Mirrors the Rust-side `AeroSparseDisk::open` truncation checks.)
    const fileSize = file.size;
    if (typeof fileSize === "number" && Number.isFinite(fileSize) && fileSize >= 0 && Number.isSafeInteger(fileSize)) {
      const expectedMinLen = expectedDataOffset + allocatedBlocks * blockSizeBig;
      if (BigInt(fileSize) < expectedMinLen) return null;
    }

    return blockSizeBytes;
  } catch {
    return null;
  }
}

function diskLabel(meta: DiskImageMetadata): string {
  const name = (meta as { name?: unknown }).name;
  const id = (meta as { id?: unknown }).id;
  const n = typeof name === "string" && name.trim() ? name.trim() : "unnamed";
  const i = typeof id === "string" && id.trim() ? id.trim() : "unknown-id";
  return `${n} (id=${i})`;
}

/**
 * Validate that a selected disk is usable by the browser machine runtime.
 *
 * The canonical `api.Machine` storage controllers expect synchronous Rust storage backends; today
 * that means OPFS-backed media only (no IndexedDB async backends, no remote streaming).
 */
export function planMachineBootDiskAttachment(meta: DiskImageMetadata, role: MachineBootDiskRole): MachineBootDiskPlan {
  // Remote streaming disks require async network I/O, which the synchronous Rust storage controllers
  // cannot perform today.
  if (!isRecord(meta)) {
    throw new Error("machine runtime received invalid disk metadata (expected an object)");
  }
  const metaRec = meta as Record<string, unknown>;
  const source = hasOwn(metaRec, "source") ? metaRec.source : undefined;
  if (source === "remote") {
    throw new Error("machine runtime does not yet support remote streaming disks");
  }

  if (source !== "local") {
    throw new Error(`machine runtime received unexpected disk source=${String(source)}`);
  }

  // Local metadata can still represent a remote-streaming disk via `meta.remote`. Reject those for
  // now as well: the base bytes are fetched on-demand and therefore async.
  // Treat `meta` as untrusted: do not observe inherited `remote` values (prototype pollution).
  const legacyRemote = hasOwn(metaRec, "remote") ? metaRec.remote : undefined;
  if (legacyRemote) {
    throw new Error("machine runtime does not yet support remote streaming disks");
  }

  const backend = hasOwn(metaRec, "backend") ? metaRec.backend : undefined;
  if (backend !== "opfs") {
    throw new Error(
      `machine runtime currently requires OPFS-backed disks (disk=${diskLabel(meta)} backend=${String(backend)})`,
    );
  }

  const warnings: string[] = [];

  if (role === "hdd") {
    const kind = hasOwn(metaRec, "kind") ? metaRec.kind : undefined;
    if (kind !== "hdd") {
      throw new Error(`machine runtime expected an HDD disk, got kind=${String(kind)} (disk=${diskLabel(meta)})`);
    }
    const format = hasOwn(metaRec, "format") ? metaRec.format : undefined;
    if (format === "unknown") {
      throw new Error(
        `machine runtime requires explicit HDD format metadata (disk=${diskLabel(meta)} format=unknown)`,
      );
    }
    if (format !== "raw" && format !== "aerospar") {
      throw new Error(
        `machine runtime only supports raw/aerospar HDD images for now (disk=${diskLabel(meta)} format=${String(format)})`,
      );
    }
    return { opfsPath: opfsPathForDisk(meta), format: format === "aerospar" ? "aerospar" : "raw", warnings };
  }

  const kind = hasOwn(metaRec, "kind") ? metaRec.kind : undefined;
  if (kind !== "cd") {
    throw new Error(`machine runtime expected a CD disk, got kind=${String(kind)} (disk=${diskLabel(meta)})`);
  }

  const format = hasOwn(metaRec, "format") ? metaRec.format : undefined;
  if (format !== "iso") {
    throw new Error(
      `machine runtime only supports ISO install media for now (disk=${diskLabel(meta)} format=${String(format)})`,
    );
  }

  return { opfsPath: opfsPathForDisk(meta), format: "iso", warnings };
}

async function attachHdd(machine: MachineHandle, plan: MachineBootDiskPlan, meta: DiskImageMetadata): Promise<void> {
  const expectedSizeBytes =
    typeof meta.sizeBytes === "number" && Number.isSafeInteger(meta.sizeBytes) && meta.sizeBytes > 0 ? BigInt(meta.sizeBytes) : undefined;

  // Prefer copy-on-write overlays whenever possible, regardless of whether the base disk bytes are
  // raw or aerosparse. This mirrors the legacy runtime disk worker behaviour: base images remain
  // immutable; guest writes persist in a derived `*.overlay.aerospar` file.
  const setPrimaryCow =
    machine.set_primary_hdd_opfs_cow ??
    (machine as unknown as { setPrimaryHddOpfsCow?: unknown }).setPrimaryHddOpfsCow;
  if (typeof setPrimaryCow === "function") {
    const overlayPath = opfsOverlayPathForCow(meta);
    const blockSizeBytes =
      (await tryReadAerosparseBlockSizeBytesFromOpfs(overlayPath)) ?? DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES;
    await callMaybeAsync(setPrimaryCow as (...args: unknown[]) => unknown, machine, [plan.opfsPath, overlayPath, blockSizeBytes]);
    // Best-effort overlay ref: ensure snapshots record the base/overlay paths even if
    // `set_primary_hdd_opfs_cow` does not populate `DISKS` overlay refs in this WASM build.
    setAhciPort0DiskOverlayRef(machine, plan.opfsPath, overlayPath);
    return;
  }

  if (plan.format === "aerospar") {
    const aerosparOpenAndSetRef =
      machine.set_disk_aerospar_opfs_open_and_set_overlay_ref ??
      (machine as unknown as { setDiskAerosparOpfsOpenAndSetOverlayRef?: unknown }).setDiskAerosparOpfsOpenAndSetOverlayRef;
    if (typeof aerosparOpenAndSetRef === "function") {
      await callMaybeAsync(aerosparOpenAndSetRef as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
      return;
    }
    const aerosparOpen =
      machine.set_disk_aerospar_opfs_open ??
      (machine as unknown as { setDiskAerosparOpfsOpen?: unknown }).setDiskAerosparOpfsOpen;
    if (typeof aerosparOpen === "function") {
      await callMaybeAsync(aerosparOpen as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
      setAhciPort0DiskOverlayRef(machine, plan.opfsPath, "");
      return;
    }
    // Newer WASM builds can open aerosparse disks via the generic OPFS existing open path when an
    // explicit base format is provided.
    const diskExistingAndSetRef =
      machine.set_disk_opfs_existing_and_set_overlay_ref ??
      (machine as unknown as { setDiskOpfsExistingAndSetOverlayRef?: unknown }).setDiskOpfsExistingAndSetOverlayRef;
    if (typeof diskExistingAndSetRef === "function" && diskExistingAndSetRef.length >= 2) {
      await callMaybeAsync(diskExistingAndSetRef as (...args: unknown[]) => unknown, machine, [
        plan.opfsPath,
        "aerospar",
        expectedSizeBytes,
      ]);
      return;
    }
    const diskExisting =
      machine.set_disk_opfs_existing ?? (machine as unknown as { setDiskOpfsExisting?: unknown }).setDiskOpfsExisting;
    if (typeof diskExisting === "function" && diskExisting.length >= 2) {
      await callMaybeAsync(diskExisting as (...args: unknown[]) => unknown, machine, [
        plan.opfsPath,
        "aerospar",
        expectedSizeBytes,
      ]);
      setAhciPort0DiskOverlayRef(machine, plan.opfsPath, "");
      return;
    }
    throw new Error(
      "WASM build missing Machine.set_disk_aerospar_opfs_open* exports (and does not support Machine.set_disk_opfs_existing(path, \"aerospar\")).",
    );
  }

  // Prefer the explicit canonical primary HDD helper when available.
  const setPrimaryExisting =
    machine.set_primary_hdd_opfs_existing ??
    (machine as unknown as { setPrimaryHddOpfsExisting?: unknown }).setPrimaryHddOpfsExisting;
  if (typeof setPrimaryExisting === "function") {
    await callMaybeAsync(setPrimaryExisting as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    // Best-effort overlay ref: ensure snapshots record a stable base_image for disk_id=0 even if
    // older WASM builds did not set it inside `set_primary_hdd_opfs_existing`.
    setAhciPort0DiskOverlayRef(machine, plan.opfsPath, "");
    return;
  }

  const diskExistingAndSetRef =
    machine.set_disk_opfs_existing_and_set_overlay_ref ??
    (machine as unknown as { setDiskOpfsExistingAndSetOverlayRef?: unknown }).setDiskOpfsExistingAndSetOverlayRef;
  if (typeof diskExistingAndSetRef === "function") {
    await callMaybeAsync(diskExistingAndSetRef as (...args: unknown[]) => unknown, machine, [
      plan.opfsPath,
      undefined,
      expectedSizeBytes,
    ]);
    return;
  }
  const diskExisting =
    machine.set_disk_opfs_existing ?? (machine as unknown as { setDiskOpfsExisting?: unknown }).setDiskOpfsExisting;
  if (typeof diskExisting === "function") {
    await callMaybeAsync(diskExisting as (...args: unknown[]) => unknown, machine, [
      plan.opfsPath,
      undefined,
      expectedSizeBytes,
    ]);
    setAhciPort0DiskOverlayRef(machine, plan.opfsPath, "");
    return;
  }
  throw new Error(
    "WASM build missing raw HDD OPFS attach exports (expected Machine.set_primary_hdd_opfs_cow, Machine.set_primary_hdd_opfs_existing, or Machine.set_disk_opfs_existing*).",
  );
}

async function attachCd(machine: MachineHandle, plan: MachineBootDiskPlan): Promise<void> {
  // Prefer the canonical, explicit IDE secondary master naming (matches `disk_id=1`).
  const attachIdeAndSetRef =
    machine.attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref ??
    (machine as unknown as { attachIdeSecondaryMasterIsoOpfsExistingAndSetOverlayRef?: unknown }).attachIdeSecondaryMasterIsoOpfsExistingAndSetOverlayRef;
  if (typeof attachIdeAndSetRef === "function") {
    await callMaybeAsync(attachIdeAndSetRef as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    return;
  }
  const attachIde =
    machine.attach_ide_secondary_master_iso_opfs_existing ??
    (machine as unknown as { attachIdeSecondaryMasterIsoOpfsExisting?: unknown }).attachIdeSecondaryMasterIsoOpfsExisting;
  if (typeof attachIde === "function") {
    await callMaybeAsync(attachIde as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    const setRef =
      machine.set_ide_secondary_master_atapi_overlay_ref ??
      (machine as unknown as { setIdeSecondaryMasterAtapiOverlayRef?: unknown }).setIdeSecondaryMasterAtapiOverlayRef;
    if (typeof setRef === "function") {
      setRef.call(machine, plan.opfsPath, "");
    }
    return;
  }

  // Back-compat: some builds expose the install-media naming.
  const attachInstallExistingAndSetRef =
    machine.attach_install_media_iso_opfs_existing_and_set_overlay_ref ??
    (machine as unknown as { attachInstallMediaIsoOpfsExistingAndSetOverlayRef?: unknown }).attachInstallMediaIsoOpfsExistingAndSetOverlayRef;
  if (typeof attachInstallExistingAndSetRef === "function") {
    await callMaybeAsync(attachInstallExistingAndSetRef as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    return;
  }
  const attachInstallExisting =
    machine.attach_install_media_iso_opfs_existing ??
    (machine as unknown as { attachInstallMediaIsoOpfsExisting?: unknown }).attachInstallMediaIsoOpfsExisting;
  if (typeof attachInstallExisting === "function") {
    await callMaybeAsync(attachInstallExisting as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    const setRef =
      machine.set_ide_secondary_master_atapi_overlay_ref ??
      (machine as unknown as { setIdeSecondaryMasterAtapiOverlayRef?: unknown }).setIdeSecondaryMasterAtapiOverlayRef;
    if (typeof setRef === "function") {
      setRef.call(machine, plan.opfsPath, "");
    }
    return;
  }
  const attachInstallAndSetRef =
    machine.attach_install_media_iso_opfs_and_set_overlay_ref ??
    (machine as unknown as { attachInstallMediaIsoOpfsAndSetOverlayRef?: unknown }).attachInstallMediaIsoOpfsAndSetOverlayRef;
  if (typeof attachInstallAndSetRef === "function") {
    await callMaybeAsync(attachInstallAndSetRef as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    return;
  }
  const attachInstall =
    machine.attach_install_media_iso_opfs ??
    (machine as unknown as { attachInstallMediaIsoOpfs?: unknown }).attachInstallMediaIsoOpfs ??
    machine.attach_install_media_opfs_iso ??
    (machine as unknown as { attachInstallMediaOpfsIso?: unknown }).attachInstallMediaOpfsIso;
  if (typeof attachInstall === "function") {
    await callMaybeAsync(attachInstall as (...args: unknown[]) => unknown, machine, [plan.opfsPath]);
    const setRef =
      machine.set_ide_secondary_master_atapi_overlay_ref ??
      (machine as unknown as { setIdeSecondaryMasterAtapiOverlayRef?: unknown }).setIdeSecondaryMasterAtapiOverlayRef;
    if (typeof setRef === "function") {
      setRef.call(machine, plan.opfsPath, "");
    }
    return;
  }

  throw new Error("WASM build missing Machine install-media ISO OPFS attach exports");
}

export async function attachMachineBootDisk(
  machine: MachineHandle,
  role: MachineBootDiskRole,
  meta: DiskImageMetadata,
): Promise<{ warnings: string[] }> {
  const plan = planMachineBootDiskAttachment(meta, role);
  if (role === "hdd") {
    await attachHdd(machine, plan, meta);
  } else {
    await attachCd(machine, plan);
  }
  return { warnings: plan.warnings };
}

export async function attachMachineBootDisks(
  machine: MachineHandle,
  selection: { hdd: DiskImageMetadata | null; cd: DiskImageMetadata | null },
): Promise<{ warnings: string[] }> {
  const warnings: string[] = [];
  if (selection.hdd) {
    warnings.push(...(await attachMachineBootDisk(machine, "hdd", selection.hdd)).warnings);
  }
  if (selection.cd) {
    warnings.push(...(await attachMachineBootDisk(machine, "cd", selection.cd)).warnings);
  }
  return { warnings };
}

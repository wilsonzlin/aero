import type { MachineHandle } from "./wasm_loader";
import type { DiskImageMetadata } from "../storage/metadata";
import { opfsPathForDisk } from "../storage/opfs_paths";

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
  if (meta.source === "remote") {
    throw new Error("machine runtime does not yet support remote streaming disks");
  }

  if (meta.source !== "local") {
    throw new Error(`machine runtime received unexpected disk source=${String((meta as any).source)}`);
  }

  // Local metadata can still represent a remote-streaming disk via `meta.remote`. Reject those for
  // now as well: the base bytes are fetched on-demand and therefore async.
  if (meta.remote) {
    throw new Error("machine runtime does not yet support remote streaming disks");
  }

  if (meta.backend !== "opfs") {
    throw new Error(
      `machine runtime currently requires OPFS-backed disks (disk=${diskLabel(meta)} backend=${meta.backend})`,
    );
  }

  const warnings: string[] = [];

  if (role === "hdd") {
    if (meta.kind !== "hdd") {
      throw new Error(`machine runtime expected an HDD disk, got kind=${meta.kind} (disk=${diskLabel(meta)})`);
    }
    if (meta.format === "unknown") {
      throw new Error(
        `machine runtime requires explicit HDD format metadata (disk=${diskLabel(meta)} format=unknown)`,
      );
    }
    if (meta.format !== "raw" && meta.format !== "aerospar") {
      throw new Error(
        `machine runtime only supports raw/aerospar HDD images for now (disk=${diskLabel(meta)} format=${meta.format})`,
      );
    }
    return { opfsPath: opfsPathForDisk(meta), format: meta.format === "aerospar" ? "aerospar" : "raw", warnings };
  }

  if (meta.kind !== "cd") {
    throw new Error(`machine runtime expected a CD disk, got kind=${meta.kind} (disk=${diskLabel(meta)})`);
  }

  if (meta.format !== "iso") {
    throw new Error(
      `machine runtime only supports ISO install media for now (disk=${diskLabel(meta)} format=${meta.format})`,
    );
  }

  return { opfsPath: opfsPathForDisk(meta), format: "iso", warnings };
}

async function attachHdd(machine: MachineHandle, plan: MachineBootDiskPlan): Promise<void> {
  if (plan.format === "aerospar") {
    if (typeof machine.set_disk_aerospar_opfs_open_and_set_overlay_ref === "function") {
      await machine.set_disk_aerospar_opfs_open_and_set_overlay_ref(plan.opfsPath);
      return;
    }
    if (typeof machine.set_disk_aerospar_opfs_open === "function") {
      await machine.set_disk_aerospar_opfs_open(plan.opfsPath);
      machine.set_ahci_port0_disk_overlay_ref?.(plan.opfsPath, "");
      return;
    }
    throw new Error("WASM build missing Machine.set_disk_aerospar_opfs_open* exports");
  }

  if (typeof machine.set_disk_opfs_existing_and_set_overlay_ref === "function") {
    await machine.set_disk_opfs_existing_and_set_overlay_ref(plan.opfsPath);
    return;
  }
  if (typeof machine.set_disk_opfs_existing === "function") {
    await machine.set_disk_opfs_existing(plan.opfsPath);
    machine.set_ahci_port0_disk_overlay_ref?.(plan.opfsPath, "");
    return;
  }
  throw new Error("WASM build missing Machine.set_disk_opfs_existing* exports");
}

async function attachCd(machine: MachineHandle, plan: MachineBootDiskPlan): Promise<void> {
  // Prefer the canonical, explicit IDE secondary master naming (matches `disk_id=1`).
  if (typeof machine.attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref === "function") {
    await machine.attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref(plan.opfsPath);
    return;
  }
  if (typeof machine.attach_ide_secondary_master_iso_opfs_existing === "function") {
    await machine.attach_ide_secondary_master_iso_opfs_existing(plan.opfsPath);
    machine.set_ide_secondary_master_atapi_overlay_ref?.(plan.opfsPath, "");
    return;
  }

  // Back-compat: some builds expose the install-media naming.
  if (typeof machine.attach_install_media_iso_opfs_existing_and_set_overlay_ref === "function") {
    await machine.attach_install_media_iso_opfs_existing_and_set_overlay_ref(plan.opfsPath);
    return;
  }
  if (typeof machine.attach_install_media_iso_opfs_existing === "function") {
    await machine.attach_install_media_iso_opfs_existing(plan.opfsPath);
    machine.set_ide_secondary_master_atapi_overlay_ref?.(plan.opfsPath, "");
    return;
  }
  if (typeof machine.attach_install_media_iso_opfs_and_set_overlay_ref === "function") {
    await machine.attach_install_media_iso_opfs_and_set_overlay_ref(plan.opfsPath);
    return;
  }
  if (typeof machine.attach_install_media_iso_opfs === "function") {
    await machine.attach_install_media_iso_opfs(plan.opfsPath);
    machine.set_ide_secondary_master_atapi_overlay_ref?.(plan.opfsPath, "");
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
    await attachHdd(machine, plan);
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

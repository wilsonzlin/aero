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


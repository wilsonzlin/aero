import type { AeroConfig, AeroVmRuntime } from "../config/aero_config";
import type { DiskImageMetadata, MountConfig } from "../storage/metadata";
import type { SetBootDisksMessage } from "./boot_disks_protocol";

/**
 * Minimal shared view of the boot disk selection state.
 *
 * This is intentionally decoupled from legacy config surfaces like `activeDiskImage`;
 * boot disk mounts are the canonical "is a boot disk attached?" signal.
 */
export type BootDisksLike =
  | Pick<SetBootDisksMessage, "mounts" | "hdd" | "cd">
  | {
      mounts?: MountConfig | null;
      hdd?: DiskImageMetadata | null;
      cd?: DiskImageMetadata | null;
    }
  | null
  | undefined;

export function resolveVmRuntime(config?: Pick<AeroConfig, "vmRuntime"> | null): AeroVmRuntime {
  return config?.vmRuntime === "machine" ? "machine" : "legacy";
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

export function hasBootDisks(bootDisks?: BootDisksLike): boolean {
  if (!bootDisks) return false;
  const mounts = (bootDisks as { mounts?: MountConfig | null }).mounts ?? null;
  const hdd = (bootDisks as { hdd?: DiskImageMetadata | null }).hdd ?? null;
  const cd = (bootDisks as { cd?: DiskImageMetadata | null }).cd ?? null;
  if (hdd || cd) return true;
  if (!mounts || typeof mounts !== "object") return false;
  // Treat mount IDs as untrusted; ignore inherited values (prototype pollution).
  const rec = mounts as Record<string, unknown>;
  const hddId = hasOwn(rec, "hddId") ? rec.hddId : undefined;
  const cdId = hasOwn(rec, "cdId") ? rec.cdId : undefined;
  return (
    (typeof hddId === "string" && hddId.trim().length > 0) || (typeof cdId === "string" && cdId.trim().length > 0)
  );
}

/**
 * Returns true when the user selected a VM runtime that should execute a guest.
 *
 * - `vmRuntime="machine"` always requests a VM (even with no disks).
 * - `vmRuntime="legacy"` requests a VM only when boot disks are mounted; otherwise
 *   the runtime should run legacy demo/no-disk behaviour.
 */
export function isVmRequested(args: { config?: Pick<AeroConfig, "vmRuntime"> | null; bootDisks?: BootDisksLike }): boolean {
  const vmRuntime = resolveVmRuntime(args.config);
  if (vmRuntime === "machine") return true;
  return hasBootDisks(args.bootDisks);
}

/**
 * Returns true when the legacy runtime should run demo/no-disk behaviour.
 *
 * This is the only state where legacy demo-only subsystems (CPU demo framebuffer,
 * audio test tone, etc.) should run.
 */
export function shouldRunLegacyDemoMode(args: { config?: Pick<AeroConfig, "vmRuntime"> | null; bootDisks?: BootDisksLike }): boolean {
  const vmRuntime = resolveVmRuntime(args.config);
  return vmRuntime === "legacy" && !hasBootDisks(args.bootDisks);
}

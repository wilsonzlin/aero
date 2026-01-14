import { OPFS_DISKS_PATH, type DiskImageMetadata } from "./metadata";

function normalizeOpfsRelPath(path: string, field: string): string {
  const trimmed = path.trim();
  if (!trimmed) {
    throw new Error(`${field} must not be empty`);
  }
  const parts = trimmed
    .split("/")
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
  if (parts.length === 0) {
    throw new Error(`${field} must not be empty`);
  }
  for (const p of parts) {
    if (p === "." || p === "..") {
      throw new Error(`${field} must not contain "." or ".."`);
    }
    if (p.includes("\0")) {
      throw new Error(`${field} must not contain NUL bytes`);
    }
  }
  return parts.join("/");
}

function normalizeOpfsFileName(name: string, field: string): string {
  if (name.trim().length === 0) {
    throw new Error(`${field} must not be empty`);
  }
  // OPFS file names are path components; reject separators to avoid confusion about directories.
  if (name.includes("/") || name.includes("\\") || name.includes("\0")) {
    throw new Error(`${field} must be a simple file name (no path separators)`);
  }
  if (name === "." || name === "..") {
    throw new Error(`${field} must not be "." or ".."`);
  }
  return name;
}

function joinOpfsPath(
  dirPath: string,
  opts: { dirField: string; fileName: string; fileField: string },
): string {
  const dir = normalizeOpfsRelPath(dirPath, opts.dirField);
  const file = normalizeOpfsFileName(opts.fileName, opts.fileField);
  return `${dir}/${file}`;
}

/**
 * Derive a canonical OPFS path (relative to the OPFS root) for a local disk image.
 *
 * This is intended for consumers that need a stable path string to open the same
 * disk from multiple runtimes (e.g. JS and Rust `aero_opfs`).
 */
export function opfsPathForDisk(meta: DiskImageMetadata): string {
  if (meta.source !== "local") {
    throw new Error(`opfsPathForDisk requires local disk metadata (got source=${meta.source})`);
  }
  if (meta.backend !== "opfs") {
    throw new Error(`opfsPathForDisk requires an OPFS-backed disk (got backend=${meta.backend})`);
  }
  const dir = meta.opfsDirectory ?? OPFS_DISKS_PATH;
  return joinOpfsPath(dir, { dirField: "opfsDirectory", fileName: meta.fileName, fileField: "fileName" });
}

/**
 * Derive a canonical OPFS path (relative to the OPFS root) for the copy-on-write overlay used
 * when opening the disk in "cow" mode.
 */
export function opfsOverlayPathForCow(meta: DiskImageMetadata): string {
  if (meta.source === "remote") {
    if (meta.cache.backend !== "opfs") {
      throw new Error(
        `opfsOverlayPathForCow requires an OPFS-backed remote overlay (got cache.backend=${meta.cache.backend})`,
      );
    }
    return joinOpfsPath(OPFS_DISKS_PATH, {
      dirField: "OPFS_DISKS_PATH",
      fileName: meta.cache.overlayFileName,
      fileField: "overlayFileName",
    });
  }

  if (meta.backend !== "opfs") {
    throw new Error(`opfsOverlayPathForCow requires an OPFS-backed disk (got backend=${meta.backend})`);
  }
  if (meta.kind !== "hdd") {
    throw new Error(`opfsOverlayPathForCow is only valid for local HDD disks (got kind=${meta.kind})`);
  }

  const id = normalizeOpfsFileName(meta.id, "id");
  const overlayFileName = `${id}.overlay.aerospar`;
  const dir = meta.opfsDirectory ?? OPFS_DISKS_PATH;
  return joinOpfsPath(dir, { dirField: "opfsDirectory", fileName: overlayFileName, fileField: "overlayFileName" });
}

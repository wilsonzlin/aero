import { OPFS_DISKS_PATH, type DiskImageMetadata } from "./metadata";

/**
 * Helpers for mapping {@link DiskImageMetadata} to OPFS-relative path strings.
 *
 * These paths are **relative to the OPFS root** returned by `navigator.storage.getDirectory()`,
 * and are suitable for passing to Rust/WASM backends (e.g. `aero_opfs`) that expect OPFS paths.
 */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

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
 * This is intended for consumers that need a stable path string to open the same disk from
 * multiple runtimes (e.g. JS and Rust `aero_opfs`).
 */
export function opfsPathForDisk(meta: DiskImageMetadata): string {
  // Treat metadata as untrusted: do not observe inherited fields (prototype pollution) when
  // selecting the directory or file name.
  if (!isRecord(meta)) {
    throw new Error("opfsPathForDisk requires disk metadata (expected an object)");
  }
  const rec = meta as Record<string, unknown>;
  const source = hasOwn(rec, "source") ? rec.source : undefined;
  if (source !== "local") {
    throw new Error(`opfsPathForDisk requires local disk metadata (got source=${String(source)})`);
  }
  const backend = hasOwn(rec, "backend") ? rec.backend : undefined;
  if (backend !== "opfs") {
    throw new Error(`opfsPathForDisk requires an OPFS-backed disk (got backend=${String(backend)})`);
  }
  const fileNameRaw = hasOwn(rec, "fileName") ? rec.fileName : undefined;
  if (typeof fileNameRaw !== "string") {
    throw new Error("fileName must be a string");
  }
  const dirRaw = hasOwn(rec, "opfsDirectory") ? (rec as { opfsDirectory?: unknown }).opfsDirectory : undefined;
  const dir = typeof dirRaw === "string" ? dirRaw : OPFS_DISKS_PATH;
  return joinOpfsPath(dir, { dirField: "opfsDirectory", fileName: fileNameRaw, fileField: "fileName" });
}

/**
 * Derive a canonical OPFS path (relative to the OPFS root) for the copy-on-write overlay used
 * when opening the disk in "cow" mode.
 */
export function opfsOverlayPathForCow(meta: DiskImageMetadata): string {
  // Treat metadata as untrusted: do not observe inherited fields (prototype pollution) when
  // selecting overlay/cache file names.
  if (!isRecord(meta)) {
    throw new Error("opfsOverlayPathForCow requires disk metadata (expected an object)");
  }
  const rec = meta as Record<string, unknown>;
  const source = hasOwn(rec, "source") ? rec.source : undefined;
  if (source === "remote") {
    const cacheRaw = hasOwn(rec, "cache") ? rec.cache : undefined;
    if (!isRecord(cacheRaw)) {
      throw new Error("opfsOverlayPathForCow requires remote disk cache metadata");
    }
    const cacheRec = cacheRaw as Record<string, unknown>;
    const cacheBackend = hasOwn(cacheRec, "backend") ? cacheRec.backend : undefined;
    if (cacheBackend !== "opfs") {
      throw new Error(
        `opfsOverlayPathForCow requires an OPFS-backed remote overlay (got cache.backend=${String(cacheBackend)})`,
      );
    }
    const overlayFileNameRaw = hasOwn(cacheRec, "overlayFileName") ? cacheRec.overlayFileName : undefined;
    if (typeof overlayFileNameRaw !== "string") {
      throw new Error("overlayFileName must be a string");
    }
    return joinOpfsPath(OPFS_DISKS_PATH, {
      dirField: "OPFS_DISKS_PATH",
      fileName: overlayFileNameRaw,
      fileField: "overlayFileName",
    });
  }

  if (source !== "local") {
    throw new Error(`opfsOverlayPathForCow requires disk metadata with source=\"local\" (got source=${String(source)})`);
  }
  const backend = hasOwn(rec, "backend") ? rec.backend : undefined;
  if (backend !== "opfs") {
    throw new Error(`opfsOverlayPathForCow requires an OPFS-backed disk (got backend=${String(backend)})`);
  }
  const kind = hasOwn(rec, "kind") ? rec.kind : undefined;
  if (kind !== "hdd") {
    throw new Error(`opfsOverlayPathForCow is only valid for local HDD disks (got kind=${String(kind)})`);
  }

  const idRaw = hasOwn(rec, "id") ? rec.id : undefined;
  if (typeof idRaw !== "string") {
    throw new Error("id must be a string");
  }
  const id = normalizeOpfsFileName(idRaw, "id");
  const overlayFileName = `${id}.overlay.aerospar`;
  const dirRaw = hasOwn(rec, "opfsDirectory") ? (rec as { opfsDirectory?: unknown }).opfsDirectory : undefined;
  const dir = typeof dirRaw === "string" ? dirRaw : OPFS_DISKS_PATH;
  return joinOpfsPath(dir, { dirField: "opfsDirectory", fileName: overlayFileName, fileField: "overlayFileName" });
}

export type OpfsCowPaths = {
  /**
   * OPFS path (relative to the OPFS root) to the immutable base image.
   */
  basePath: string;
  /**
   * OPFS path (relative to the OPFS root) to the overlay image (copy-on-write).
   */
  overlayPath: string;
  /**
   * Optional overlay block size hint (bytes). Used by some backends (e.g. remote disk overlays).
   */
  overlayBlockSizeBytes?: number;
};

/**
 * Map a disk metadata record to its base OPFS file path (relative to the OPFS root).
 *
 * Returns `null` when the disk is not backed by OPFS (e.g. IndexedDB cache backend).
 */
export function diskMetaToOpfsBasePath(meta: DiskImageMetadata): string | null {
  if (!isRecord(meta)) {
    throw new Error("diskMetaToOpfsBasePath requires disk metadata (expected an object)");
  }
  const rec = meta as Record<string, unknown>;
  const source = hasOwn(rec, "source") ? rec.source : undefined;
  if (source === "local") {
    const backend = hasOwn(rec, "backend") ? rec.backend : undefined;
    if (backend !== "opfs") return null;
    return opfsPathForDisk(meta);
  }
  if (source !== "remote") {
    throw new Error(`diskMetaToOpfsBasePath: unexpected disk source=${String(source)}`);
  }

  const cacheRaw = hasOwn(rec, "cache") ? rec.cache : undefined;
  if (!isRecord(cacheRaw)) {
    throw new Error("diskMetaToOpfsBasePath requires remote disk cache metadata");
  }
  const cacheRec = cacheRaw as Record<string, unknown>;
  const backend = hasOwn(cacheRec, "backend") ? cacheRec.backend : undefined;
  if (backend !== "opfs") return null;
  const fileNameRaw = hasOwn(cacheRec, "fileName") ? cacheRec.fileName : undefined;
  if (typeof fileNameRaw !== "string") {
    throw new Error("cache.fileName must be a string");
  }
  return joinOpfsPath(OPFS_DISKS_PATH, {
    dirField: "OPFS_DISKS_PATH",
    fileName: fileNameRaw,
    fileField: "cache.fileName",
  });
}

/**
 * Map a disk metadata record to its copy-on-write (base + overlay) OPFS file paths.
 *
 * Returns `null` when the disk cannot be expressed as OPFS paths (e.g. IDB-backed).
 */
export function diskMetaToOpfsCowPaths(meta: DiskImageMetadata): OpfsCowPaths | null {
  if (!isRecord(meta)) {
    throw new Error("diskMetaToOpfsCowPaths requires disk metadata (expected an object)");
  }
  const rec = meta as Record<string, unknown>;
  const source = hasOwn(rec, "source") ? rec.source : undefined;
  if (source === "local") {
    const backend = hasOwn(rec, "backend") ? rec.backend : undefined;
    const kind = hasOwn(rec, "kind") ? rec.kind : undefined;
    if (backend !== "opfs") return null;
    if (kind !== "hdd") return null;
    const basePath = diskMetaToOpfsBasePath(meta);
    if (!basePath) return null;
    const overlayPath = opfsOverlayPathForCow(meta);
    return { basePath, overlayPath };
  }

  if (source !== "remote") {
    throw new Error(`diskMetaToOpfsCowPaths: unexpected disk source=${String(source)}`);
  }
  const cacheRaw = hasOwn(rec, "cache") ? rec.cache : undefined;
  if (!isRecord(cacheRaw)) {
    throw new Error("diskMetaToOpfsCowPaths requires remote disk cache metadata");
  }
  const cacheRec = cacheRaw as Record<string, unknown>;
  const backend = hasOwn(cacheRec, "backend") ? cacheRec.backend : undefined;
  if (backend !== "opfs") return null;

  const overlayBlockSizeBytesRaw = hasOwn(cacheRec, "overlayBlockSizeBytes") ? cacheRec.overlayBlockSizeBytes : undefined;
  if (typeof overlayBlockSizeBytesRaw !== "number" || !Number.isFinite(overlayBlockSizeBytesRaw) || overlayBlockSizeBytesRaw <= 0) {
    throw new Error("cache.overlayBlockSizeBytes must be a positive number");
  }
  const overlayBlockSizeBytes = overlayBlockSizeBytesRaw >>> 0;

  const basePath = diskMetaToOpfsBasePath(meta);
  if (!basePath) return null;
  const overlayPath = opfsOverlayPathForCow(meta);
  return { basePath, overlayPath, overlayBlockSizeBytes };
}

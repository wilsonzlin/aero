export interface DiskImageInfo {
  name: string;
  size: number;
  lastModified: number;
}

export interface DiskImageImportProgress {
  loaded: number;
  total: number;
}

export type DiskImageImportProgressCallback = (progress: DiskImageImportProgress) => void;

// Keep this in sync with `web/src/platform/opfs.ts` which reserves the "images" directory for
// disk image blobs used by the emulator.
export const DEFAULT_OPFS_DISK_IMAGES_DIRECTORY = "images";

export type OpfsWorkerOpenToken = {
  kind: "opfs";
  directory: string;
  name: string;
};

export type MemoryWorkerOpenToken = {
  kind: "memory";
  name: string;
  blob: Blob;
};

export type WorkerOpenToken = OpfsWorkerOpenToken | MemoryWorkerOpenToken;

// Kept for the DiskImageStore interface signature – we currently always return WorkerOpenToken.
export type TransferableHandle = unknown;

export interface DiskImageStore {
  list(): Promise<DiskImageInfo[]>;
  import(
    file: File,
    name?: string,
    onProgress?: DiskImageImportProgressCallback,
  ): Promise<DiskImageInfo>;
  delete(name: string): Promise<void>;
  export(name: string): Promise<Blob>;
  openForWorker(name: string): Promise<WorkerOpenToken | TransferableHandle>;
}

export function formatByteSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes === 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let n = bytes;
  let unit = 0;
  while (n >= 1024 && unit < units.length - 1) {
    n /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : n >= 100 ? 0 : n >= 10 ? 1 : 2;
  return `${n.toFixed(digits)} ${units[unit]}`;
}

export function sanitizeDiskImageName(input: string): string {
  const trimmed = input.trim();
  const withoutSeparators = trimmed.replaceAll(/[\\/]/g, "_");
  const withoutNull = withoutSeparators.replaceAll("\0", "");
  const cleaned = withoutNull.replaceAll(/\s+/g, " ");
  return cleaned.length > 0 ? cleaned : "disk.img";
}

export async function resolveUniqueName(
  desiredName: string,
  isTaken: (candidate: string) => boolean | Promise<boolean>,
): Promise<string> {
  const sanitized = sanitizeDiskImageName(desiredName);
  if (!(await isTaken(sanitized))) return sanitized;

  const dotIdx = sanitized.lastIndexOf(".");
  const hasExtension = dotIdx > 0 && dotIdx < sanitized.length - 1;
  const base = hasExtension ? sanitized.slice(0, dotIdx) : sanitized;
  const ext = hasExtension ? sanitized.slice(dotIdx) : "";

  for (let i = 1; ; i += 1) {
    const candidate = `${base} (${i})${ext}`;
    if (!(await isTaken(candidate))) return candidate;
  }
}

export async function readBlobAsArrayBuffer(blob: Blob): Promise<ArrayBuffer> {
  // Not all DOM shims implement `Blob.arrayBuffer()` (e.g. some jsdom versions).
  const anyBlob = blob as Blob & { arrayBuffer?: () => Promise<ArrayBuffer> };
  if (typeof anyBlob.arrayBuffer === "function") {
    return await anyBlob.arrayBuffer();
  }

  // `Response` is widely available (browsers + Node/undici) and can decode a Blob.
  if (typeof Response !== "undefined") {
    return await new Response(blob).arrayBuffer();
  }

  // Last-resort fallback for older environments.
  return await new Promise<ArrayBuffer>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read Blob"));
    reader.onload = () => resolve(reader.result as ArrayBuffer);
    reader.readAsArrayBuffer(blob);
  });
}

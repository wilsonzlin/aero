export type OpfsProgress = {
  writtenBytes: number;
  totalBytes: number;
};

export type OpfsImportProgressCallback = (progress: OpfsProgress) => void;

export type OpenFileHandleOptions = {
  create?: boolean;
};

export class OpfsUnavailableError extends Error {
  override name = "OpfsUnavailableError";
}

function getStorageGetDirectory():
  | ((this: StorageManager) => Promise<FileSystemDirectoryHandle>)
  | undefined {
  if (typeof navigator === "undefined") return undefined;
  const storage = navigator.storage;
  if (!storage) return undefined;
  return (storage as StorageManager & { getDirectory?: unknown }).getDirectory as
    | ((this: StorageManager) => Promise<FileSystemDirectoryHandle>)
    | undefined;
}

function assertOpfsSupported(): asserts true {
  if (!getStorageGetDirectory()) {
    throw new OpfsUnavailableError(
      "OPFS is not available in this browser/context (navigator.storage.getDirectory is missing).",
    );
  }
}

function splitOpfsPath(path: string): string[] {
  const trimmed = path.trim();
  const parts = trimmed.split("/").filter((p) => p.length > 0);
  if (parts.length === 0) {
    throw new Error("OPFS path must not be empty.");
  }
  for (const part of parts) {
    if (part === "." || part === "..") {
      throw new Error('OPFS path must not contain "." or "..".');
    }
  }
  return parts;
}

async function getDirectoryHandleForPath(
  root: FileSystemDirectoryHandle,
  dirParts: string[],
  create: boolean,
): Promise<FileSystemDirectoryHandle> {
  let dir = root;
  for (const part of dirParts) {
    dir = await dir.getDirectoryHandle(part, { create });
  }
  return dir;
}

export async function getOpfsRoot(): Promise<FileSystemDirectoryHandle> {
  assertOpfsSupported();
  const getDirectory = getStorageGetDirectory();
  if (!getDirectory) {
    // Defensive: `assertOpfsSupported` should have thrown.
    throw new OpfsUnavailableError("OPFS is not available (navigator.storage.getDirectory missing).");
  }
  return await getDirectory.call(navigator.storage);
}

export async function getOpfsImagesDir(): Promise<FileSystemDirectoryHandle> {
  const root = await getOpfsRoot();
  return await root.getDirectoryHandle("images", { create: true });
}

export async function getOpfsStateDir(): Promise<FileSystemDirectoryHandle> {
  const root = await getOpfsRoot();
  return await root.getDirectoryHandle("state", { create: true });
}

export async function openFileHandle(
  path: string,
  options: OpenFileHandleOptions = {},
): Promise<FileSystemFileHandle> {
  const parts = splitOpfsPath(path);
  const filename = parts.pop();
  if (!filename) {
    throw new Error("OPFS path must include a filename.");
  }

  const root = await getOpfsRoot();
  const parentDir = await getDirectoryHandleForPath(root, parts, options.create === true);
  return await parentDir.getFileHandle(filename, { create: options.create === true });
}

export async function importFileToOpfs(
  file: File,
  destPath: string,
  onProgress?: OpfsImportProgressCallback,
): Promise<FileSystemFileHandle> {
  const handle = await openFileHandle(destPath, { create: true });
  const writable = await handle.createWritable();

  const reader = file.stream().getReader();
  let writtenBytes = 0;
  const totalBytes = file.size;

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value) {
        await writable.write(value);
        writtenBytes += value.byteLength;
        onProgress?.({ writtenBytes, totalBytes });
      }
    }
  } catch (err) {
    try {
      await writable.abort();
    } catch {
      // ignore
    }
    throw err;
  }

  await writable.close();
  onProgress?.({ writtenBytes, totalBytes });
  return handle;
}

function isDedicatedWorkerGlobalScope(): boolean {
  const ctor = (globalThis as typeof globalThis & { DedicatedWorkerGlobalScope?: unknown })
    .DedicatedWorkerGlobalScope;
  if (!ctor) return false;
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return globalThis instanceof (ctor as any);
  } catch {
    // Some runtimes don't expose a usable constructor; fall back to a heuristic.
    return typeof (globalThis as typeof globalThis & { document?: unknown }).document === "undefined";
  }
}

/**
 * Worker-only helper: `FileSystemFileHandle.createSyncAccessHandle()` is only
 * available in Dedicated Workers.
 */
export async function createSyncAccessHandleInDedicatedWorker(
  fileHandle: FileSystemFileHandle,
): Promise<FileSystemSyncAccessHandle> {
  if (!isDedicatedWorkerGlobalScope()) {
    throw new Error(
      "OPFS sync access handles are only available in Dedicated Workers. " +
        "Call this from a DedicatedWorkerGlobalScope, or use createWritable() on the main thread.",
    );
  }

  const fn = (fileHandle as FileSystemFileHandle & { createSyncAccessHandle?: unknown })
    .createSyncAccessHandle as ((this: FileSystemFileHandle) => Promise<FileSystemSyncAccessHandle>) | undefined;
  if (!fn) {
    throw new OpfsUnavailableError(
      "OPFS sync access handles are not supported in this browser (createSyncAccessHandle missing).",
    );
  }
  return await fn.call(fileHandle);
}

/**
 * Convenience wrapper for worker threads performing high-frequency I/O.
 */
export async function openSyncAccessHandleInDedicatedWorker(
  path: string,
  options: OpenFileHandleOptions = {},
): Promise<FileSystemSyncAccessHandle> {
  const handle = await openFileHandle(path, options);
  return await createSyncAccessHandleInDedicatedWorker(handle);
}


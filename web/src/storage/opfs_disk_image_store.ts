import type {
  DiskImageImportProgressCallback,
  DiskImageInfo,
  DiskImageStore,
  WorkerOpenToken,
} from "./disk_image_store";
import { DEFAULT_OPFS_DISK_IMAGES_DIRECTORY, resolveUniqueName } from "./disk_image_store";

export type OpfsSupportStatus =
  | { supported: true }
  | {
      supported: false;
      reason: string;
    };

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

export function getOpfsSupportStatus(): OpfsSupportStatus {
  if (typeof navigator === "undefined") {
    return { supported: false, reason: "Not running in a browser context." };
  }
  if (typeof globalThis.isSecureContext === "boolean" && !globalThis.isSecureContext) {
    return { supported: false, reason: "Not a secure context (https or localhost required)." };
  }
  if (!getStorageGetDirectory()) {
    return { supported: false, reason: "navigator.storage.getDirectory() is unavailable." };
  }
  return { supported: true };
}

export class OpfsDiskImageStore implements DiskImageStore {
  #dirPromise: Promise<FileSystemDirectoryHandle> | null = null;

  async #getImagesDir(): Promise<FileSystemDirectoryHandle> {
    if (!this.#dirPromise) {
      this.#dirPromise = (async () => {
        const getDirectory = getStorageGetDirectory();
        if (!getDirectory) {
          throw new Error("OPFS is unavailable (navigator.storage.getDirectory is missing).");
        }
        const root = await getDirectory.call(navigator.storage);
        return root.getDirectoryHandle(DEFAULT_OPFS_DISK_IMAGES_DIRECTORY, { create: true });
      })();
    }
    return this.#dirPromise;
  }

  async list(): Promise<DiskImageInfo[]> {
    const dir = await this.#getImagesDir();
    const infos: DiskImageInfo[] = [];
    for await (const [name, handle] of dir.entries()) {
      if (handle.kind !== "file") continue;
      const file = await (handle as FileSystemFileHandle).getFile();
      infos.push({ name, size: file.size, lastModified: file.lastModified });
    }
    infos.sort((a, b) => a.name.localeCompare(b.name));
    return infos;
  }

  async import(
    file: File,
    name?: string,
    onProgress?: DiskImageImportProgressCallback,
  ): Promise<DiskImageInfo> {
    const dir = await this.#getImagesDir();
    const resolvedName = await resolveUniqueName(name ?? file.name, async (candidate) => {
      try {
        await dir.getFileHandle(candidate);
        return true;
      } catch {
        return false;
      }
    });

    const handle = await dir.getFileHandle(resolvedName, { create: true });
    const writable = await handle.createWritable({ keepExistingData: false });

    const total = file.size;
    let loaded = 0;
    onProgress?.({ loaded, total });

    const reader = file.stream().getReader();
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value) {
          await writable.write(value);
          loaded += value.byteLength;
          onProgress?.({ loaded, total });
        }
      }
      await writable.close();
    } catch (err) {
      try {
        await writable.abort(err);
      } catch {
        // ignore best-effort abort failures
      }
      throw err;
    }

    const stored = await handle.getFile();
    return { name: resolvedName, size: stored.size, lastModified: stored.lastModified };
  }

  async delete(name: string): Promise<void> {
    const dir = await this.#getImagesDir();
    await dir.removeEntry(name);
  }

  async export(name: string): Promise<Blob> {
    const dir = await this.#getImagesDir();
    const handle = await dir.getFileHandle(name);
    return handle.getFile();
  }

  async openForWorker(name: string): Promise<WorkerOpenToken> {
    const dir = await this.#getImagesDir();
    await dir.getFileHandle(name);
    return { kind: "opfs", directory: DEFAULT_OPFS_DISK_IMAGES_DIRECTORY, name };
  }
}

import type {
  DiskImageImportProgressCallback,
  DiskImageInfo,
  DiskImageStore,
  WorkerOpenToken,
} from "./disk_image_store";
import { readBlobAsArrayBuffer, resolveUniqueName } from "./disk_image_store";

export class MemoryDiskImageStore implements DiskImageStore {
  readonly #images = new Map<string, { blob: Blob; lastModified: number }>();

  async list(): Promise<DiskImageInfo[]> {
    const infos: DiskImageInfo[] = [];
    for (const [name, entry] of this.#images) {
      infos.push({ name, size: entry.blob.size, lastModified: entry.lastModified });
    }
    infos.sort((a, b) => a.name.localeCompare(b.name));
    return infos;
  }

  async import(
    file: File,
    name?: string,
    onProgress?: DiskImageImportProgressCallback,
  ): Promise<DiskImageInfo> {
    const resolvedName = await resolveUniqueName(name ?? file.name, (candidate) =>
      this.#images.has(candidate),
    );

    const buf = await readBlobAsArrayBuffer(file);
    onProgress?.({ loaded: buf.byteLength, total: buf.byteLength });

    const blob = new Blob([buf], { type: file.type || "application/octet-stream" });
    const lastModified = Date.now();
    this.#images.set(resolvedName, { blob, lastModified });
    return { name: resolvedName, size: blob.size, lastModified };
  }

  async delete(name: string): Promise<void> {
    if (!this.#images.delete(name)) {
      throw new Error(`Disk image not found: ${name}`);
    }
  }

  async export(name: string): Promise<Blob> {
    const entry = this.#images.get(name);
    if (!entry) throw new Error(`Disk image not found: ${name}`);
    return entry.blob;
  }

  async openForWorker(name: string): Promise<WorkerOpenToken> {
    const blob = await this.export(name);
    return { kind: "memory", name, blob };
  }
}

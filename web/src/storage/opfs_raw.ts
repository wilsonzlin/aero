import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { opfsGetDisksDir } from "./metadata.ts";

type SyncAccessHandle = {
  read(buffer: ArrayBufferView, options?: { at: number }): number;
  write(buffer: ArrayBufferView, options?: { at: number }): number;
  flush(): void;
  close(): void;
  getSize(): number;
  truncate(size: number): void;
};

type FileHandle = {
  createSyncAccessHandle?: () => Promise<SyncAccessHandle>;
  createWritable(options?: FileSystemCreateWritableOptions): Promise<FileSystemWritableFileStream>;
  getFile(): Promise<File>;
};

type DirectoryHandle = {
  getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<DirectoryHandle>;
  getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
};

async function getOpfsDisksDir(): Promise<DirectoryHandle> {
  return (await opfsGetDisksDir()) as unknown as DirectoryHandle;
}

export class OpfsRawDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;

  private constructor(
    private readonly access:
      | { kind: "sync"; sync: SyncAccessHandle }
      | { kind: "async"; file: FileHandle },
    capacityBytes: number,
  ) {
    this.capacityBytes = capacityBytes;
  }

  static async open(
    fileName: string,
    opts: { create?: boolean; sizeBytes?: number } = {},
  ): Promise<OpfsRawDisk> {
    const dir = await getOpfsDisksDir();
    const file = await dir.getFileHandle(fileName, { create: opts.create ?? false });
    const syncFactory = file.createSyncAccessHandle;
    const sync = syncFactory ? await syncFactory.call(file) : undefined;

    if (typeof opts.sizeBytes === "number") {
      if (!Number.isSafeInteger(opts.sizeBytes) || opts.sizeBytes <= 0) {
        sync?.close();
        throw new Error(`invalid sizeBytes=${opts.sizeBytes}`);
      }
      if (sync) {
        const current = sync.getSize();
        if (current === 0 && opts.sizeBytes > 0) {
          sync.truncate(opts.sizeBytes);
        } else if (current !== opts.sizeBytes) {
          sync.close();
          throw new Error(`disk size mismatch: expected=${opts.sizeBytes} actual=${current}`);
        }
      } else {
        const current = (await file.getFile()).size;
        if (current === 0 && opts.sizeBytes > 0) {
          const writable = await file.createWritable({ keepExistingData: false });
          await writable.truncate(opts.sizeBytes);
          await writable.close();
        } else if (current !== opts.sizeBytes) {
          throw new Error(`disk size mismatch: expected=${opts.sizeBytes} actual=${current}`);
        }
      }
    }

    if (sync) {
      return new OpfsRawDisk({ kind: "sync", sync }, sync.getSize());
    }

    // Fallback: async file access (slower; no SyncAccessHandle support).
    const size = (await file.getFile()).size;
    return new OpfsRawDisk({ kind: "async", file }, size);
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }
    if (this.access.kind === "sync") {
      const read = this.access.sync.read(buffer, { at: offset });
      if (read !== buffer.byteLength) {
        throw new Error(`short read: expected=${buffer.byteLength} actual=${read}`);
      }
      return;
    }

    const file = await this.access.file.getFile();
    const ab = await file.slice(offset, offset + buffer.byteLength).arrayBuffer();
    const view = new Uint8Array(ab);
    if (view.byteLength !== buffer.byteLength) {
      throw new Error(`short read: expected=${buffer.byteLength} actual=${view.byteLength}`);
    }
    buffer.set(view);
  }

  async writeSectors(lba: number, data: Uint8Array): Promise<void> {
    assertSectorAligned(data.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, data.byteLength, this.sectorSize);
    if (offset + data.byteLength > this.capacityBytes) {
      throw new Error("write past end of disk");
    }
    if (this.access.kind === "sync") {
      const written = this.access.sync.write(data, { at: offset });
      if (written !== data.byteLength) {
        throw new Error(`short write: expected=${data.byteLength} actual=${written}`);
      }
      return;
    }

    const writable = await this.access.file.createWritable({ keepExistingData: true });
    // `FileSystemWritableFileStream` does not currently accept views backed by
    // SharedArrayBuffer, so ensure the payload is ArrayBuffer-backed.
    await writable.write({ type: "write", position: offset, data: new Uint8Array(data) });
    await writable.close();
  }

  async flush(): Promise<void> {
    if (this.access.kind === "sync") {
      this.access.sync.flush();
    }
  }

  async close(): Promise<void> {
    if (this.access.kind === "sync") {
      this.access.sync.close();
    }
  }
}

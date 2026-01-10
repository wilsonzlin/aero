import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";

type SyncAccessHandle = {
  read(buffer: ArrayBufferView, options?: { at: number }): number;
  write(buffer: ArrayBufferView, options?: { at: number }): number;
  flush(): void;
  close(): void;
  getSize(): number;
  truncate(size: number): void;
};

type FileHandle = {
  createSyncAccessHandle(): Promise<SyncAccessHandle>;
  createWritable(): Promise<FileSystemWritableFileStream>;
};

type DirectoryHandle = {
  getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<DirectoryHandle>;
  getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
};

async function getOpfsRoot(): Promise<DirectoryHandle> {
  if (!navigator.storage?.getDirectory) {
    throw new Error("OPFS is not available (navigator.storage.getDirectory missing)");
  }
  return (await navigator.storage.getDirectory()) as unknown as DirectoryHandle;
}

async function getOpfsImagesDir(): Promise<DirectoryHandle> {
  const root = await getOpfsRoot();
  return await root.getDirectoryHandle("images", { create: true });
}

export async function importFileToOpfs(
  file: File,
  opts: {
    destName?: string;
    onProgress?: (writtenBytes: number, totalBytes: number) => void;
  } = {},
): Promise<string> {
  const images = await getOpfsImagesDir();
  const destName = opts.destName ?? file.name;
  const handle = await images.getFileHandle(destName, { create: true });
  const writable = await handle.createWritable();

  const reader = file.stream().getReader();
  let written = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    await writable.write(value);
    written += value.byteLength;
    opts.onProgress?.(written, file.size);
  }
  await writable.close();
  return destName;
}

export class OpfsRawDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;

  private constructor(
    private readonly sync: SyncAccessHandle,
    capacityBytes: number,
  ) {
    this.capacityBytes = capacityBytes;
  }

  static async open(
    name: string,
    opts: { create?: boolean; sizeBytes?: number } = {},
  ): Promise<OpfsRawDisk> {
    const images = await getOpfsImagesDir();
    const file = await images.getFileHandle(name, { create: opts.create ?? false });
    const sync = await file.createSyncAccessHandle();

    if (typeof opts.sizeBytes === "number") {
      if (!Number.isSafeInteger(opts.sizeBytes) || opts.sizeBytes <= 0) {
        sync.close();
        throw new Error(`invalid sizeBytes=${opts.sizeBytes}`);
      }
      const current = sync.getSize();
      if (current === 0 && opts.sizeBytes > 0) {
        sync.truncate(opts.sizeBytes);
      } else if (current !== opts.sizeBytes) {
        sync.close();
        throw new Error(`disk size mismatch: expected=${opts.sizeBytes} actual=${current}`);
      }
    }

    return new OpfsRawDisk(sync, sync.getSize());
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength);
    const offset = checkedOffset(lba, buffer.byteLength);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }
    const read = this.sync.read(buffer, { at: offset });
    if (read !== buffer.byteLength) {
      throw new Error(`short read: expected=${buffer.byteLength} actual=${read}`);
    }
  }

  async writeSectors(lba: number, data: Uint8Array): Promise<void> {
    assertSectorAligned(data.byteLength);
    const offset = checkedOffset(lba, data.byteLength);
    if (offset + data.byteLength > this.capacityBytes) {
      throw new Error("write past end of disk");
    }
    const written = this.sync.write(data, { at: offset });
    if (written !== data.byteLength) {
      throw new Error(`short write: expected=${data.byteLength} actual=${written}`);
    }
  }

  async flush(): Promise<void> {
    this.sync.flush();
  }

  async close(): Promise<void> {
    this.sync.close();
  }
}


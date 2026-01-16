const originalNavigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");

function restoreNavigator(): void {
  if (originalNavigatorDescriptor) {
    Object.defineProperty(globalThis, "navigator", originalNavigatorDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as unknown as { navigator?: unknown }, "navigator");
  }
}

function notFound(): DOMException {
  return new DOMException("NotFound", "NotFoundError");
}

class MemoryFile {
  constructor(
    private readonly data: Uint8Array,
    readonly lastModified: number,
  ) {}

  get size(): number {
    return this.data.byteLength;
  }

  // Provide a minimal `Blob`/`File` slice API so code under test can safely read prefixes.
  //
  // Real `FileSystemFileHandle.getFile()` returns a `File` which implements `slice()`. Some
  // production code (e.g. aerospar header sniffing) relies on this to avoid reading whole
  // multi-gigabyte disk images into memory. Our in-memory OPFS test double should match that
  // contract.
  slice(start?: number, end?: number): MemoryFile {
    const size = this.data.byteLength;
    const clamp = (n: number): number => Math.max(0, Math.min(size, n));
    const s = start === undefined ? 0 : start < 0 ? clamp(size + start) : clamp(start);
    const e = end === undefined ? size : end < 0 ? clamp(size + end) : clamp(end);
    return new MemoryFile(this.data.subarray(s, e).slice(), this.lastModified);
  }

  async text(): Promise<string> {
    return new TextDecoder().decode(this.data);
  }

  async arrayBuffer(): Promise<ArrayBuffer> {
    return this.data.slice().buffer;
  }
}

class MemoryWritable {
  private readonly chunks: Uint8Array[] = [];
  private closed = false;

  constructor(
    private readonly onCommit: (data: Uint8Array) => void,
    baseData?: Uint8Array,
  ) {
    if (baseData && baseData.byteLength > 0) {
      this.chunks.push(baseData);
    }
  }

  async write(data: string | Uint8Array): Promise<void> {
    if (this.closed) throw new Error("writable already closed");
    if (typeof data === "string") {
      this.chunks.push(new TextEncoder().encode(data));
      return;
    }
    this.chunks.push(data);
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    const total = this.chunks.reduce((sum, c) => sum + c.byteLength, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const c of this.chunks) {
      out.set(c, off);
      off += c.byteLength;
    }
    this.onCommit(out);
  }

  async abort(): Promise<void> {
    this.closed = true;
  }
}

export class MemoryFileHandle {
  readonly kind = "file" as const;
  private lastModifiedMs = Date.now();

  constructor(
    readonly name: string,
    private data: Uint8Array = new Uint8Array(),
  ) {}

  async getFile(): Promise<MemoryFile> {
    return new MemoryFile(this.data, this.lastModifiedMs);
  }

  async createSyncAccessHandle(): Promise<FileSystemSyncAccessHandle> {
    let closed = false;
    const assertOpen = () => {
      if (closed) throw new Error("SyncAccessHandle closed");
    };
    const touch = () => {
      this.lastModifiedMs = Date.now();
    };

    return {
      read: (buffer: ArrayBufferView, options?: { at?: number }) => {
        assertOpen();
        const at = options?.at ?? 0;
        if (!Number.isSafeInteger(at) || at < 0) throw new Error("invalid read offset");
        const dst = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
        if (at >= this.data.byteLength) return 0;
        const src = this.data.subarray(at, at + dst.byteLength);
        dst.set(src);
        return src.byteLength;
      },
      write: (buffer: ArrayBufferView, options?: { at?: number }) => {
        assertOpen();
        const at = options?.at ?? 0;
        if (!Number.isSafeInteger(at) || at < 0) throw new Error("invalid write offset");
        const src = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
        const end = at + src.byteLength;
        if (!Number.isSafeInteger(end) || end < 0) throw new Error("write overflow");
        if (end > this.data.byteLength) {
          const next = new Uint8Array(end);
          next.set(this.data);
          this.data = next;
        }
        this.data.set(src, at);
        touch();
        return src.byteLength;
      },
      flush: () => {
        assertOpen();
      },
      close: () => {
        closed = true;
      },
      getSize: () => {
        assertOpen();
        return this.data.byteLength;
      },
      truncate: (newSize: number) => {
        assertOpen();
        if (!Number.isSafeInteger(newSize) || newSize < 0) throw new Error("invalid truncate size");
        if (newSize === this.data.byteLength) return;
        if (newSize < this.data.byteLength) {
          this.data = this.data.subarray(0, newSize).slice();
          touch();
          return;
        }
        const next = new Uint8Array(newSize);
        next.set(this.data);
        this.data = next;
        touch();
      },
    } satisfies FileSystemSyncAccessHandle;
  }

  async createWritable(options?: { keepExistingData?: boolean }): Promise<MemoryWritable> {
    const base = options?.keepExistingData ? this.data : undefined;
    return new MemoryWritable(
      (out) => {
        this.data = out;
        this.lastModifiedMs = Date.now();
      },
      base,
    );
  }
}

export class MemoryDirectoryHandle {
  readonly kind = "directory" as const;
  private readonly dirs = new Map<string, MemoryDirectoryHandle>();
  private readonly files = new Map<string, MemoryFileHandle>();

  readonly name: string;

  constructor(name: string) {
    this.name = name;
  }

  async getDirectoryHandle(name: string, options: { create?: boolean } = {}): Promise<MemoryDirectoryHandle> {
    const existing = this.dirs.get(name);
    if (existing) return existing;
    if (!options.create) throw notFound();
    const dir = new MemoryDirectoryHandle(name);
    this.dirs.set(name, dir);
    return dir;
  }

  async getFileHandle(name: string, options: { create?: boolean } = {}): Promise<MemoryFileHandle> {
    const existing = this.files.get(name);
    if (existing) return existing;
    if (!options.create) throw notFound();
    const file = new MemoryFileHandle(name);
    this.files.set(name, file);
    return file;
  }

  async removeEntry(name: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (this.files.delete(name)) return;
    const dir = this.dirs.get(name);
    if (!dir) throw notFound();
    if (!options.recursive && (dir.files.size > 0 || dir.dirs.size > 0)) {
      throw new DOMException("Directory not empty", "InvalidModificationError");
    }
    this.dirs.delete(name);
  }

  async *entries(): AsyncGenerator<[string, MemoryDirectoryHandle | MemoryFileHandle]> {
    // Deterministic iteration to keep tests stable.
    const names: string[] = [...this.dirs.keys(), ...this.files.keys()].sort();
    for (const name of names) {
      const dir = this.dirs.get(name);
      if (dir) {
        yield [name, dir];
        continue;
      }
      const file = this.files.get(name);
      if (file) yield [name, file];
    }
  }
}

export function installMemoryOpfs(
  root: MemoryDirectoryHandle = new MemoryDirectoryHandle("root"),
): { root: MemoryDirectoryHandle; restore: () => void } {
  Object.defineProperty(globalThis, "navigator", {
    value: { storage: { getDirectory: async () => root } },
    configurable: true,
    enumerable: true,
    writable: true,
  });
  return { root, restore: restoreNavigator };
}

export async function getDir(root: MemoryDirectoryHandle, parts: string[]): Promise<MemoryDirectoryHandle> {
  let dir = root;
  for (const part of parts) {
    dir = await dir.getDirectoryHandle(part, { create: false });
  }
  return dir;
}

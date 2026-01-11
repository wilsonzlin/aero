export class MemFileSystemWritableFileStream {
  private parts: Uint8Array[] = [];
  private aborted = false;
  private readonly file: MemFileSystemFileHandle;

  constructor(file: MemFileSystemFileHandle, keepExistingData: boolean) {
    this.file = file;
    if (keepExistingData) {
      this.parts.push(file.data.slice());
    }
  }

  async write(chunk: unknown): Promise<void> {
    if (this.aborted) throw new Error("write after abort");

    if (typeof chunk === "string") {
      this.parts.push(new TextEncoder().encode(chunk));
      return;
    }

    if (chunk && typeof chunk === "object") {
      // Support the common `{ type: "write", data: ... }` form.
      const maybeParams = chunk as { type?: unknown; data?: unknown };
      if (maybeParams.type === "write" && maybeParams.data !== undefined) {
        await this.write(maybeParams.data);
        return;
      }
    }

    if (chunk instanceof Uint8Array) {
      this.parts.push(chunk.slice());
      return;
    }
    if (chunk instanceof ArrayBuffer) {
      this.parts.push(new Uint8Array(chunk));
      return;
    }
    if (ArrayBuffer.isView(chunk)) {
      this.parts.push(new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength));
      return;
    }

    throw new Error(`unsupported write() chunk type: ${typeof chunk}`);
  }

  async close(): Promise<void> {
    if (this.aborted) return;
    const total = this.parts.reduce((sum, p) => sum + p.byteLength, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const p of this.parts) {
      out.set(p, off);
      off += p.byteLength;
    }
    this.file.data = out;
    this.file.lastModified = Date.now();
  }

  async abort(): Promise<void> {
    this.aborted = true;
  }
}

export class MemFileSystemFileHandle {
  readonly kind = "file" as const;
  readonly name: string;
  data: Uint8Array = new Uint8Array(0);
  lastModified = Date.now();

  constructor(name: string) {
    this.name = name;
  }

  async getFile(): Promise<File> {
    return new File([this.data], this.name, { lastModified: this.lastModified });
  }

  async createWritable(opts: { keepExistingData?: boolean } = {}): Promise<MemFileSystemWritableFileStream> {
    return new MemFileSystemWritableFileStream(this, opts.keepExistingData !== false);
  }
}

export class MemFileSystemDirectoryHandle {
  readonly kind = "directory" as const;
  readonly name: string;
  private readonly children = new Map<string, MemFileSystemDirectoryHandle | MemFileSystemFileHandle>();

  constructor(name: string) {
    this.name = name;
  }

  async getDirectoryHandle(name: string, opts: { create?: boolean } = {}): Promise<MemFileSystemDirectoryHandle> {
    const existing = this.children.get(name);
    if (existing) {
      if (existing.kind !== "directory") throw new DOMException("Not a directory", "TypeMismatchError");
      return existing;
    }
    if (!opts.create) throw new DOMException("Not found", "NotFoundError");
    const dir = new MemFileSystemDirectoryHandle(name);
    this.children.set(name, dir);
    return dir;
  }

  async getFileHandle(name: string, opts: { create?: boolean } = {}): Promise<MemFileSystemFileHandle> {
    const existing = this.children.get(name);
    if (existing) {
      if (existing.kind !== "file") throw new DOMException("Not a file", "TypeMismatchError");
      return existing;
    }
    if (!opts.create) throw new DOMException("Not found", "NotFoundError");
    const file = new MemFileSystemFileHandle(name);
    this.children.set(name, file);
    return file;
  }

  async removeEntry(name: string, opts: { recursive?: boolean } = {}): Promise<void> {
    const existing = this.children.get(name);
    if (!existing) throw new DOMException("Not found", "NotFoundError");
    if (existing.kind === "directory") {
      if (!opts.recursive && existing.children.size > 0) {
        throw new DOMException("Directory not empty", "InvalidModificationError");
      }
    }
    this.children.delete(name);
  }

  async *entries(): AsyncIterableIterator<[string, MemFileSystemDirectoryHandle | MemFileSystemFileHandle]> {
    for (const entry of this.children.entries()) {
      yield entry;
    }
  }
}

export function installOpfsMock(): MemFileSystemDirectoryHandle {
  const root = new MemFileSystemDirectoryHandle("opfs-root");
  // Node.js defines a getter-only `navigator`; add a `storage.getDirectory` method to it.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const nav = globalThis.navigator as any;
  nav.storage = {
    getDirectory: async () => root,
  };
  return root;
}

export async function getDir(
  root: MemFileSystemDirectoryHandle,
  parts: string[],
  opts: { create: boolean },
): Promise<MemFileSystemDirectoryHandle> {
  let dir = root;
  for (const part of parts) {
    dir = await dir.getDirectoryHandle(part, { create: opts.create });
  }
  return dir;
}


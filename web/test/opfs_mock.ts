export class MemFileSystemWritableFileStream {
  private data: Uint8Array;
  private position = 0;
  private aborted = false;
  private readonly file: MemFileSystemFileHandle;

  constructor(file: MemFileSystemFileHandle, keepExistingData: boolean) {
    this.file = file;
    this.data = keepExistingData ? file.data.slice() : new Uint8Array(0);
  }

  private writeBytes(bytes: Uint8Array): void {
    if (bytes.byteLength === 0) return;
    const end = this.position + bytes.byteLength;
    if (end > this.data.byteLength) {
      const next = new Uint8Array(end);
      next.set(this.data);
      this.data = next;
    }
    this.data.set(bytes, this.position);
    this.position = end;
  }

  async write(chunk: unknown): Promise<void> {
    if (this.aborted) throw new Error("write after abort");

    if (typeof chunk === "string") {
      this.writeBytes(new TextEncoder().encode(chunk));
      return;
    }

    if (chunk && typeof chunk === "object") {
      // Support the standard `{ type: "write", position, data }` form used by OPFS streams.
      const maybeParams = chunk as { type?: unknown; data?: unknown; position?: unknown; size?: unknown };
      if (maybeParams.type === "write" && maybeParams.data !== undefined) {
        if (typeof maybeParams.position === "number") {
          this.position = maybeParams.position;
        }
        await this.write(maybeParams.data);
        return;
      }
      if (maybeParams.type === "seek" && typeof maybeParams.position === "number") {
        this.position = maybeParams.position;
        return;
      }
      if (maybeParams.type === "truncate" && typeof maybeParams.size === "number") {
        await this.truncate(maybeParams.size);
        return;
      }
    }

    if (chunk instanceof Uint8Array) {
      this.writeBytes(chunk);
      return;
    }
    if (chunk instanceof ArrayBuffer) {
      this.writeBytes(new Uint8Array(chunk));
      return;
    }
    if (ArrayBuffer.isView(chunk)) {
      this.writeBytes(new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength));
      return;
    }

    throw new Error(`unsupported write() chunk type: ${typeof chunk}`);
  }

  async close(): Promise<void> {
    if (this.aborted) return;
    this.file.data = this.data;
    this.file.lastModified = Date.now();
  }

  async truncate(size: number): Promise<void> {
    if (this.aborted) throw new Error("truncate after abort");
    if (!Number.isSafeInteger(size) || size < 0) {
      throw new Error(`invalid truncate size: ${size}`);
    }
    if (size === 0) {
      this.data = new Uint8Array(0);
      this.position = 0;
      return;
    }
    if (size === this.data.byteLength) return;

    if (size < this.data.byteLength) {
      this.data = this.data.slice(0, size);
    } else {
      const out = new Uint8Array(size);
      out.set(this.data);
      this.data = out;
    }
    this.position = Math.min(this.position, size);
  }

  async abort(_reason?: unknown): Promise<void> {
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
    // `BlobPart` types only accept ArrayBuffer-backed views; `Uint8Array` is generic over
    // `ArrayBufferLike` and may be backed by `SharedArrayBuffer`. Copy when needed so TypeScript
    // (and spec compliance) are happy.
    const bytesForIo: Uint8Array<ArrayBuffer> =
      this.data.buffer instanceof ArrayBuffer
        ? (this.data as Uint8Array<ArrayBuffer>)
        : (new Uint8Array(this.data) as Uint8Array<ArrayBuffer>);
    return new File([bytesForIo], this.name, { lastModified: this.lastModified });
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

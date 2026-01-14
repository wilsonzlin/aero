import "../../test/fake_indexeddb_auto.ts";

import { afterEach, describe, expect, it, vi } from "vitest";

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";

import {
  MAX_REMOTE_CHUNK_COUNT,
  MAX_REMOTE_CHUNK_INDEX_WIDTH,
  MAX_REMOTE_MANIFEST_JSON_BYTES,
  RemoteChunkedDisk,
  type BinaryStore,
} from "./remote_chunked_disk";
import { clearIdb, OPFS_AERO_DIR, OPFS_DISKS_DIR, OPFS_REMOTE_CACHE_DIR } from "./metadata";
import { remoteChunkedDeliveryType, RemoteCacheManager } from "./remote_cache_manager";

class TestMemoryStore implements BinaryStore {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

class CountingMetaWritesStore implements BinaryStore {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();
  metaWrites = 0;

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    if (path.endsWith("/meta.json")) this.metaWrites += 1;
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

class QuotaChunkWriteStore implements BinaryStore {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();
  chunkWrites = 0;
  lastChunkWritePath: string | null = null;

  constructor(private readonly quotaErrorName: string = "QuotaExceededError") {}

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    if (path.includes("/chunks/") && path.endsWith(".bin")) {
      this.chunkWrites += 1;
      this.lastChunkWritePath = path;
      // Simulate an OPFS-style partially written/truncated file before failing.
      this.files.set(path, data.slice(0, Math.min(16, data.byteLength)));
      throw new DOMException("quota", this.quotaErrorName);
    }
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

class QuotaChunkReadStore implements BinaryStore {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();
  chunkReads = 0;
  throwOnChunkReads = false;

  constructor(private readonly quotaErrorName: string = "QuotaExceededError") {}

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    if (this.throwOnChunkReads && path.includes("/chunks/") && path.endsWith(".bin")) {
      this.chunkReads += 1;
      throw new DOMException("quota", this.quotaErrorName);
    }
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

class QuotaMetaWriteStore implements BinaryStore {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();
  metaWrites = 0;
  throwOnMetaWrites = false;

  constructor(private readonly quotaErrorName: string = "QuotaExceededError") {}

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    if (path.endsWith("/meta.json")) {
      this.metaWrites += 1;
      if (this.throwOnMetaWrites) {
        throw new DOMException("quota", this.quotaErrorName);
      }
    }
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

function toArrayBufferUint8(data: Uint8Array): Uint8Array<ArrayBuffer> {
  return data.buffer instanceof ArrayBuffer ? (data as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(data);
}

class BlockingRemoveStore implements BinaryStore {
  private readonly files = new Map<string, Uint8Array<ArrayBuffer>>();
  private readonly started: Promise<void>;
  private readonly released: Promise<void>;
  private startedResolve: (() => void) | null = null;
  private releasedResolve: (() => void) | null = null;
  private blockRecursiveRemove = false;

  constructor() {
    this.started = new Promise<void>((resolve) => {
      this.startedResolve = resolve;
    });
    this.released = new Promise<void>((resolve) => {
      this.releasedResolve = resolve;
    });
  }

  waitForRecursiveRemove(): Promise<void> {
    return this.started;
  }

  armRecursiveRemoveBlock(): void {
    this.blockRecursiveRemove = true;
  }

  releaseRecursiveRemove(): void {
    if (!this.blockRecursiveRemove) return;
    this.blockRecursiveRemove = false;
    this.releasedResolve?.();
  }

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive && this.blockRecursiveRemove) {
      this.startedResolve?.();
      await this.released;
    }

    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", toArrayBufferUint8(data));
  const bytes = new Uint8Array(digest);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function buildTestImageBytes(totalSize: number): Uint8Array {
  const bytes = new Uint8Array(totalSize);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = i & 0xff;
  return bytes;
}

async function withServer(handler: (req: IncomingMessage, res: ServerResponse) => void): Promise<{
  baseUrl: string;
  hits: Map<string, number>;
  close: () => Promise<void>;
}> {
  const hits = new Map<string, number>();
  const server = createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    hits.set(url.pathname, (hits.get(url.pathname) ?? 0) + 1);
    res.setHeader("cache-control", "no-transform");
    handler(req, res);
  });

  await new Promise<void>((resolve) => server.listen(0, resolve));
  const addr = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${addr.port}`;

  return {
    baseUrl,
    hits,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

describe("RemoteChunkedDisk", () => {
  let closeServer: (() => Promise<void>) | null = null;
  afterEach(async () => {
    if (closeServer) await closeServer();
    closeServer = null;
  });

  it("rejects manifests that only provide required fields via prototype pollution", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    // Intentionally omit `schema`. A polluted prototype should not be able to smuggle it in.
    const manifest = {
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "schema");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "schema", { value: "aero.chunked-disk-image.v1", configurable: true });

      const { baseUrl, close } = await withServer((_req, res) => {
        const url = new URL(_req.url ?? "/", "http://localhost");
        if (url.pathname === "/manifest.json") {
          res.statusCode = 200;
          res.setHeader("content-type", "application/json");
          res.end(JSON.stringify(manifest));
          return;
        }
        res.statusCode = 404;
        res.end("not found");
      });
      closeServer = close;

      await expect(
        RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
          store: new TestMemoryStore(),
        }),
      ).rejects.toThrow(/schema/i);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "schema", existing);
      else Reflect.deleteProperty(Object.prototype, "schema");
    }
  });

  it("rejects manifests with too many chunks", async () => {
    const chunkSize = 512;
    const chunkCount = MAX_REMOTE_CHUNK_COUNT + 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: String(chunkCount - 1).length,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
      }),
    ).rejects.toThrow(/chunkCount.*max/i);
  });

  it("rejects manifests with non-identity Content-Encoding", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("content-encoding", "gzip");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
      }),
    ).rejects.toThrow(/content-encoding/i);
  });

  it("rejects chunks with non-identity Content-Encoding", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const chunk0 = new Uint8Array(chunkSize).fill(0x11);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      if (url.pathname === "/chunks/0.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.setHeader("content-encoding", "gzip");
        res.end(Buffer.from(chunk0));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store: new TestMemoryStore(),
    });
    try {
      const buf = new Uint8Array(chunkSize);
      await expect(disk.readSectors(0, buf)).rejects.toThrow(/content-encoding/i);
    } finally {
      await disk.close();
    }
  });

  it("rejects manifests without Cache-Control: no-transform", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("cache-control", "public, max-age=60");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
      }),
    ).rejects.toThrow(/no-transform/i);
  });

  it("rejects chunk responses without Cache-Control: no-transform", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const chunk0 = new Uint8Array(chunkSize).fill(0x11);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      if (url.pathname === "/chunks/0.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.setHeader("cache-control", "public, max-age=60");
        res.end(Buffer.from(chunk0));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store: new TestMemoryStore(),
    });
    try {
      const buf = new Uint8Array(chunkSize);
      await expect(disk.readSectors(0, buf)).rejects.toThrow(/no-transform/i);
    } finally {
      await disk.close();
    }
  });

  it("rejects manifests with chunk sizes larger than 64MiB", async () => {
    const chunkSize = 128 * 1024 * 1024;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
      }),
    ).rejects.toThrow(/chunkSize.*max/i);
  });

  it("rejects manifests with unreasonably large chunkIndexWidth", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: MAX_REMOTE_CHUNK_INDEX_WIDTH + 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
      }),
    ).rejects.toThrow(/chunkIndexWidth.*max/i);
  });

  it("tolerates null size/sha256 entries in chunks[]", async () => {
    const chunkSize = 512;
    const chunkCount = 2;
    const totalSize = chunkSize * chunkCount;

    const chunk0 = new Uint8Array(chunkSize).fill(0x11);
    const chunk1 = new Uint8Array(chunkSize).fill(0x22);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
      chunks: [
        { size: null, sha256: null },
        { size: null, sha256: null },
      ],
    };

    const { baseUrl, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      if (url.pathname === "/chunks/0.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(Buffer.from(chunk0));
        return;
      }
      if (url.pathname === "/chunks/1.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(Buffer.from(chunk1));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store: new TestMemoryStore(),
    });
    try {
      const buf0 = new Uint8Array(chunkSize);
      await disk.readSectors(0, buf0);
      expect(buf0).toEqual(chunk0);

      const buf1 = new Uint8Array(chunkSize);
      await disk.readSectors(1, buf1);
      expect(buf1).toEqual(chunk1);
    } finally {
      await disk.close();
    }
  });

  it("rejects manifests with Content-Length larger than MAX_REMOTE_MANIFEST_JSON_BYTES", async () => {
    const fetchFn = vi
      .fn<[RequestInfo | URL, RequestInit?], Promise<Response>>()
      .mockResolvedValue(
        new Response("{}", {
          status: 200,
          headers: {
            "content-type": "application/json",
            "content-length": String(MAX_REMOTE_MANIFEST_JSON_BYTES + 1),
            "cache-control": "no-transform",
          },
        }),
      );

    const originalFetch = globalThis.fetch;
    (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = fetchFn as unknown as typeof fetch;
    try {
      await expect(
        RemoteChunkedDisk.open("https://example.invalid/manifest.json", { store: new TestMemoryStore() }),
      ).rejects.toThrow(/manifest\.json.*too large/i);
    } finally {
      (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = originalFetch;
    }
  });

  it("rejects chunk responses with Content-Length larger than the manifest size", async () => {
    const chunkSize = 512;
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;
    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const fetchFn = vi.fn<[RequestInfo | URL, RequestInit?], Promise<Response>>().mockImplementation(async (input) => {
      const url = String(input);
      if (url.includes("manifest.json")) {
        return new Response(JSON.stringify(manifest), {
          status: 200,
          headers: { "content-type": "application/json", "cache-control": "no-transform" },
        });
      }
      if (url.includes("/chunks/0.bin")) {
        return new Response(new Uint8Array(chunkSize), {
          status: 200,
          headers: { "content-length": String(chunkSize + 1), "cache-control": "no-transform" },
        });
      }
      return new Response("not found", { status: 404 });
    });

    const originalFetch = globalThis.fetch;
    (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = fetchFn as unknown as typeof fetch;
    try {
      const disk = await RemoteChunkedDisk.open("https://example.invalid/manifest.json", { store: new TestMemoryStore() });

      await expect(disk.readSectors(0, new Uint8Array(chunkSize))).rejects.toHaveProperty("name", "ResponseTooLargeError");

      const chunkCalls = fetchFn.mock.calls.filter(([arg]) => String(arg).includes("/chunks/0.bin")).length;
      expect(chunkCalls).toBe(1);
    } finally {
      (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = originalFetch;
    }
  });

  it("ignores inherited credentials option (prototype pollution)", async () => {
    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize: 512,
      chunkSize: 512,
      chunkCount: 1,
      chunkIndexWidth: 1,
    };

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "credentials");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const fetchFn = vi.fn<[RequestInfo | URL, RequestInit?], Promise<Response>>().mockImplementation(async (_input, init) => {
      expect(init?.credentials).toBe("same-origin");
      return new Response(JSON.stringify(manifest), {
        status: 200,
        headers: { "content-type": "application/json", "cache-control": "no-transform" },
      });
    });

    const prevFetch = globalThis.fetch;
    globalThis.fetch = fetchFn as unknown as typeof fetch;
    try {
      Object.defineProperty(Object.prototype, "credentials", { value: "include", configurable: true, writable: true });
      const disk = await RemoteChunkedDisk.open("https://example.invalid/manifest.json", { store: new TestMemoryStore() });
      await disk.close();
    } finally {
      globalThis.fetch = prevFetch;
      if (existing) Object.defineProperty(Object.prototype, "credentials", existing);
      else delete (Object.prototype as any).credentials;
    }
  });

  it("rejects excessive prefetchSequentialChunks", async () => {
    await expect(
      RemoteChunkedDisk.open("https://example.invalid/manifest.json", {
        store: new TestMemoryStore(),
        prefetchSequentialChunks: 1025,
      }),
    ).rejects.toThrow(/prefetchSequentialChunks.*max/i);
  });

  it("rejects excessive maxAttempts", async () => {
    await expect(
      RemoteChunkedDisk.open("https://example.invalid/manifest.json", {
        store: new TestMemoryStore(),
        maxAttempts: 33,
      }),
    ).rejects.toThrow(/maxAttempts.*max/i);
  });

  it("rejects excessive maxConcurrentFetches count", async () => {
    await expect(
      RemoteChunkedDisk.open("https://example.invalid/manifest.json", {
        store: new TestMemoryStore(),
        maxConcurrentFetches: 129,
      }),
    ).rejects.toThrow(/maxConcurrentFetches.*max/i);
  });

  it("rejects excessive maxConcurrentFetches byte volume", async () => {
    const chunkSize = 8 * 1024 * 1024; // 8 MiB
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
        maxConcurrentFetches: 65, // 65 * 8 MiB = 520 MiB > 512 MiB cap
        prefetchSequentialChunks: 0,
      }),
    ).rejects.toThrow(/inflight bytes too large/i);
  });

  it("rejects excessive prefetchSequentialChunks byte volume", async () => {
    const chunkSize = 64 * 1024 * 1024; // 64 MiB
    const chunkCount = 1;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    await expect(
      RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
        store: new TestMemoryStore(),
        maxConcurrentFetches: 1,
        prefetchSequentialChunks: 9, // 9 * 64 MiB = 576 MiB > 512 MiB cap
      }),
    ).rejects.toThrow(/prefetch bytes too large/i);
  });

  it("maps byte offsets to chunk indexes and serves data from cache on repeat reads", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = 2560; // 2 full chunks + 1 half chunk
    const chunkCount = 3;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, 1024), img.slice(1024, 2048), img.slice(2048, 2560)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: 1024, sha256: await sha256Hex(chunks[0]!) },
        { size: 1024, sha256: await sha256Hex(chunks[1]!) },
        { size: 512, sha256: await sha256Hex(chunks[2]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const manifestUrl1 = `${baseUrl}/manifest.json?sig=aaa`;
    const manifestUrl2 = `${baseUrl}/manifest.json?sig=bbb`;

    const disk = await RemoteChunkedDisk.open(manifestUrl1, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });

    // Read spanning chunks 0 and 1: offset=512..2048.
    const buf = new Uint8Array(1536);
    await disk.readSectors(1, buf);
    expect(buf).toEqual(img.slice(512, 2048));

    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);

    const t1 = disk.getTelemetrySnapshot();
    expect(t1.totalSize).toBe(totalSize);
    expect(t1.cachedBytes).toBe(2048);
    expect(t1.blockRequests).toBe(2);
    expect(t1.cacheHits).toBe(0);
    expect(t1.cacheMisses).toBe(2);
    expect(t1.requests).toBe(2);
    expect(t1.bytesDownloaded).toBe(2048);
    expect(t1.lastFetchMs).not.toBeNull();

    // Re-read: should be served from cache with no additional chunk GETs.
    const buf2 = new Uint8Array(1536);
    await disk.readSectors(1, buf2);
    expect(buf2).toEqual(img.slice(512, 2048));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);

    const t2 = disk.getTelemetrySnapshot();
    expect(t2.cachedBytes).toBe(2048);
    expect(t2.blockRequests).toBe(4);
    expect(t2.cacheHits).toBe(2);
    expect(t2.cacheMisses).toBe(2);
    expect(t2.requests).toBe(2);
    expect(t2.bytesDownloaded).toBe(2048);

    // Cache metadata should not store the signed manifest URL (querystring secrets).
    const metaKey = Array.from(store.files.keys()).find((k) => k.endsWith("/meta.json"));
    expect(metaKey).toBeTruthy();
    const metaRaw = await store.read(metaKey!);
    expect(metaRaw).toBeTruthy();
    const meta = JSON.parse(new TextDecoder().decode(metaRaw!)) as Record<string, unknown>;
    expect(meta.version).toBe(1);
    expect(meta.imageId).toBe("test");
    expect(meta.imageVersion).toBe("v1");
    expect(meta.manifestUrl).toBeUndefined();
    expect(JSON.stringify(meta)).not.toContain("sig=aaa");

    await disk.close();

    // Re-open with the same store: should still hit cache (no extra chunk GETs).
    const disk2 = await RemoteChunkedDisk.open(manifestUrl2, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });

    const buf3 = new Uint8Array(1024);
    await disk2.readSectors(3, buf3);
    expect(buf3).toEqual(img.slice(1536, 2560));
    expect(hits.get("/chunks/00000001.bin")).toBe(1);
    expect(hits.get("/chunks/00000002.bin")).toBe(1);

    const t3 = disk2.getTelemetrySnapshot();
    expect(t3.totalSize).toBe(totalSize);
    expect(t3.cachedBytes).toBe(totalSize);
    expect(t3.cacheHits).toBe(1);
    expect(t3.cacheMisses).toBe(1);
    expect(t3.requests).toBe(1);
    expect(t3.bytesDownloaded).toBe(512);

    await disk2.close();
  });

  it("falls back to IndexedDB cache when OPFS cache init fails", async () => {
    await clearIdb();

    const chunkSize = 512 * 1024; // IDB cache requires >= 512KiB chunks
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
    };

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const nav = globalThis.navigator as unknown as Record<string, unknown>;
    const originalStorageDescriptor = Object.getOwnPropertyDescriptor(nav, "storage");

    // Force OPFS feature detection to pass, but throw when OPFS operations are actually invoked.
    Object.defineProperty(nav, "storage", {
      value: {
        getDirectory: () => {
          throw new Error("OPFS unavailable");
        },
      },
      configurable: true,
    });

    try {
      const common = {
        cacheBackend: "opfs" as const,
        cacheLimitBytes: null,
        maxConcurrentFetches: 1,
        prefetchSequentialChunks: 0,
        retryBaseDelayMs: 0,
      };

      const disk1 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, common);
      const buf1 = new Uint8Array(512);
      await disk1.readSectors(0, buf1);
      expect(buf1).toEqual(img.slice(0, 512));
      expect(hits.get("/chunks/00000000.bin")).toBe(1);
      await disk1.close();

      // Re-open: should still succeed and hit the persistent IDB cache (no extra chunk fetch).
      const disk2 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, common);
      const buf2 = new Uint8Array(512);
      await disk2.readSectors(0, buf2);
      expect(buf2).toEqual(img.slice(0, 512));
      expect(hits.get("/chunks/00000000.bin")).toBe(1);
      await disk2.close();
    } finally {
      if (originalStorageDescriptor) {
        Object.defineProperty(nav, "storage", originalStorageDescriptor);
      } else {
        delete (nav as { storage?: unknown }).storage;
      }
      await clearIdb();
    }
  });

  it("does not persist chunks when cacheLimitBytes is 0 (cache disabled)", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: 0,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(store.files.size).toBe(0);

    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // Cache disabled: must re-fetch.
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    expect(store.files.size).toBe(0);

    const t = disk.getTelemetrySnapshot();
    expect(t.cachedBytes).toBe(0);
    expect(t.cacheHits).toBe(0);

    await disk.close();
  });

  it("tolerates store quota errors when persisting fetched chunks (disables caching)", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunk0) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new QuotaChunkWriteStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 8,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(chunkSize * 8);

    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(store.chunkWrites).toBe(1);

    // Persistent caching should be disabled after the first quota error.
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // Best-effort: ensure no orphaned partially written chunk file remains.
    expect(store.lastChunkWritePath).toBeTruthy();
    expect(store.files.has(store.lastChunkWritePath!)).toBe(false);

    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // With caching disabled, this must re-fetch the chunk.
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    // And should not attempt to persist again.
    expect(store.chunkWrites).toBe(1);

    const t = disk.getTelemetrySnapshot();
    expect(t.cacheHits).toBe(0);
    expect(t.cachedBytes).toBe(0);

    await disk.close();
  });

  it("tolerates quota errors when reading cached chunks (treat as cache miss + disable caching)", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunk0) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new QuotaChunkReadStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 8,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    // Prime the cache (successful write).
    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Force the cache read path to throw a quota error.
    store.throwOnChunkReads = true;

    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // Cache read failed, so we must re-fetch.
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    expect(store.chunkReads).toBe(1);
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // With caching disabled, subsequent reads should not attempt further cache reads.
    const buf3 = new Uint8Array(512);
    await disk.readSectors(0, buf3);
    expect(buf3).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(3);
    expect(store.chunkReads).toBe(1);

    await disk.close();
  });

  it("disables caching when flush hits quota (and continues serving reads)", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunk0) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new QuotaMetaWriteStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 8,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    // Prime the cache (chunk 0 is cached successfully).
    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Ensure the flush path hits a quota error while persisting meta.json.
    const baseMetaWrites = store.metaWrites;
    store.throwOnMetaWrites = true;
    await expect(disk.flush()).resolves.toBeUndefined();
    expect(store.metaWrites).toBe(baseMetaWrites + 1);

    // Quota during flush should disable caching for the remainder of the disk lifetime.
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // Subsequent reads should still succeed, but will re-fetch since caching is disabled.
    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    await disk.close();
  });

  it("disables caching when clearCache hits quota (and continues serving reads)", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunk0) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new QuotaMetaWriteStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 8,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    // Prime the cache.
    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Simulate quota failure during clearCache (which re-creates meta.json).
    const baseMetaWrites = store.metaWrites;
    store.throwOnMetaWrites = true;
    await expect(disk.clearCache()).resolves.toBeUndefined();
    expect(store.metaWrites).toBe(baseMetaWrites + 1);
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // Disk should remain usable after the quota-induced cache disable.
    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    await disk.close();
  });

  it("tolerates Firefox quota error names when persisting fetched chunks (disables caching)", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunk0) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new QuotaChunkWriteStore("NS_ERROR_DOM_QUOTA_REACHED");
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 8,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(store.chunkWrites).toBe(1);

    // Persistent caching should be disabled after the first quota error.
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // Best-effort: ensure no orphaned partially written chunk file remains.
    expect(store.lastChunkWritePath).toBeTruthy();
    expect(store.files.has(store.lastChunkWritePath!)).toBe(false);

    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // With caching disabled, this must re-fetch the chunk.
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    // And should not attempt to persist again.
    expect(store.chunkWrites).toBe(1);

    const t = disk.getTelemetrySnapshot();
    expect(t.cacheHits).toBe(0);
    expect(t.cachedBytes).toBe(0);

    await disk.close();
  });

  it("ignores previously persisted chunks when cacheLimitBytes is 0", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const manifestUrl = `${baseUrl}/manifest.json`;

    // Prime the persistent (store-backed) cache.
    const store = new TestMemoryStore();
    const disk1 = await RemoteChunkedDisk.open(manifestUrl, {
      store,
      cacheLimitBytes: null,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });
    await disk1.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    const metaKey = Array.from(store.files.keys()).find((k) => k.endsWith("/meta.json"));
    expect(metaKey).toBeTruthy();
    const metaBefore = await store.read(metaKey!);
    expect(metaBefore).toBeTruthy();
    const storeSizeBefore = store.files.size;

    // Re-open with caching disabled: should still fetch from network and avoid meta updates.
    const disk2 = await RemoteChunkedDisk.open(manifestUrl, {
      store,
      cacheLimitBytes: 0,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });
    await disk2.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    expect(store.files.size).toBe(storeSizeBefore);
    const metaAfter = await store.read(metaKey!);
    expect(metaAfter).toEqual(metaBefore);

    const t = disk2.getTelemetrySnapshot();
    expect(t.cacheLimitBytes).toBe(0);
    expect(t.cachedBytes).toBe(0);
    expect(t.cacheHits).toBe(0);

    await disk2.close();
  });

  it("supports relative manifest URLs by resolving against global location.href", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunks[0]!) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const globals = globalThis as unknown as { location?: unknown };
    const prevLocation = globals.location;
    const prevFetch = globalThis.fetch;
    globals.location = { href: `${baseUrl}/` };
    globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const resolved: RequestInfo | URL = typeof input === "string" && input.startsWith("/") ? `${baseUrl}${input}` : input;
      return prevFetch(resolved, init);
    }) as typeof fetch;

    try {
      const disk = await RemoteChunkedDisk.open("/manifest.json", {
        store: new TestMemoryStore(),
        prefetchSequentialChunks: 0,
        retryBaseDelayMs: 0,
      });
      const buf = new Uint8Array(512);
      await disk.readSectors(0, buf);
      expect(buf).toEqual(img.slice(0, 512));
      expect(hits.get("/chunks/00000000.bin")).toBe(1);
      await disk.close();
    } finally {
      globalThis.fetch = prevFetch;
      globals.location = prevLocation;
    }
  });

  it("evicts least-recently-used cached chunks when the cache limit is exceeded", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize * 3;
    const chunkCount = 3;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize, chunkSize * 2), img.slice(chunkSize * 2)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[2]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 2,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    // Populate cache with chunks 0 and 1.
    await disk.readSectors(0, new Uint8Array(512));
    await disk.readSectors(2, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);

    // Touch chunk 0 so chunk 1 becomes LRU.
    await disk.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Fetching chunk 2 should evict chunk 1 to stay within limit.
    await disk.readSectors(4, new Uint8Array(512));
    expect(hits.get("/chunks/00000002.bin")).toBe(1);

    // Chunk 0 should still be cached (no extra fetch).
    await disk.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Chunk 1 should have been evicted (re-fetch).
    await disk.readSectors(2, new Uint8Array(512));
    expect(hits.get("/chunks/00000001.bin")).toBe(2);

    await disk.close();
  });

  it("heals cache metadata when a chunk file is missing", async () => {
    const chunkSize = 1024;
    const totalSize = chunkSize * 2;
    const chunkCount = 2;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const common = {
      store,
      cacheImageId: "img-1",
      cacheVersion: "v1",
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
      cacheLimitBytes: null,
    };

    const disk1 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, common);
    await disk1.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    const cacheKey = await RemoteCacheManager.deriveCacheKey({
      imageId: common.cacheImageId,
      version: common.cacheVersion,
      deliveryType: remoteChunkedDeliveryType(chunkSize),
    });
    const cacheRoot = `${OPFS_AERO_DIR}/${OPFS_DISKS_DIR}/${OPFS_REMOTE_CACHE_DIR}`;
    await store.remove(`${cacheRoot}/${cacheKey}/chunks/0.bin`);

    const disk2 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, common);
    await disk2.readSectors(0, new Uint8Array(512));
    // Missing file should trigger a cache miss (refetch).
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    await disk2.close();
  });

  it("enforces cacheLimitBytes on open by evicting older chunks", async () => {
    const chunkSize = 1024;
    const totalSize = chunkSize * 3;
    const chunkCount = 3;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize, chunkSize * 2), img.slice(chunkSize * 2)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[2]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const stable = {
      store,
      cacheImageId: "img-1",
      cacheVersion: "v1",
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    };

    // First run: cache chunks 0,1,2 in order (2 is MRU).
    const disk1 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: null });
    await disk1.readSectors(0, new Uint8Array(512));
    await disk1.readSectors(2, new Uint8Array(512));
    await disk1.readSectors(4, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);
    expect(hits.get("/chunks/00000002.bin")).toBe(1);
    await disk1.close();

    // Re-open with a strict limit: should evict older chunks on open and keep chunk 2.
    const disk2 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: chunkSize });
    await disk2.readSectors(4, new Uint8Array(512));
    expect(hits.get("/chunks/00000002.bin")).toBe(1); // cache hit
    await disk2.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2); // evicted => refetch
    await disk2.close();
  });

  it("enforces cacheLimitBytes on open even when meta.json lacks chunkLastAccess and Object.prototype is polluted", async () => {
    const chunkSize = 512;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 1,
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }
      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const stable = {
      store,
      cacheImageId: "img-1",
      cacheVersion: "v1",
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    };

    // Pre-seed a cache directory with a meta.json that omits chunkLastAccess (legacy/partial metadata),
    // but includes a cached range. On open, the disk should enforce cacheLimitBytes by evicting it.
    const cacheKey = await RemoteCacheManager.deriveCacheKey({
      imageId: stable.cacheImageId,
      version: stable.cacheVersion,
      deliveryType: remoteChunkedDeliveryType(chunkSize),
    });
    const cacheRoot = `${OPFS_AERO_DIR}/${OPFS_DISKS_DIR}/${OPFS_REMOTE_CACHE_DIR}`;
    await store.write(
      `${cacheRoot}/${cacheKey}/meta.json`,
      new TextEncoder().encode(
        JSON.stringify(
          {
            version: 1,
            imageId: stable.cacheImageId,
            imageVersion: stable.cacheVersion,
            deliveryType: remoteChunkedDeliveryType(chunkSize),
            validators: { sizeBytes: totalSize },
            chunkSizeBytes: chunkSize,
            createdAtMs: 0,
            lastAccessedAtMs: 0,
            // Omit chunkLastAccess intentionally.
            cachedRanges: [{ start: 0, end: chunkSize }],
          },
          null,
          2,
        ),
      ) as Uint8Array<ArrayBuffer>,
    );
    await store.write(`${cacheRoot}/${cacheKey}/chunks/0.bin`, new Uint8Array(chunkSize).fill(0x11) as Uint8Array<ArrayBuffer>);

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "0");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      // Prototype pollution with numeric keys must not prevent LRU reconciliation / eviction.
      Object.defineProperty(Object.prototype, "0", { value: 123, configurable: true, writable: true });

      const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: 1 });
      try {
        // The single cached chunk should be evicted immediately on open due to the strict cache limit.
        expect(disk.getTelemetrySnapshot().cachedBytes).toBe(0);
        expect(await store.read(`${cacheRoot}/${cacheKey}/chunks/0.bin`)).toBeNull();
      } finally {
        await disk.close();
      }
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "0", existing);
      else Reflect.deleteProperty(Object.prototype, "0");
    }
  });

  it("persists LRU order across sessions when cache hits update access order", async () => {
    const chunkSize = 1024;
    const totalSize = chunkSize * 2;
    const chunkCount = 2;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const stable = {
      store,
      cacheImageId: "img-1",
      cacheVersion: "v1",
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    };

    // First run: cache chunks 0 and 1, then touch chunk 0 (cache hit) so chunk 1 becomes LRU.
    const disk1 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: null });
    await disk1.readSectors(0, new Uint8Array(512)); // cache chunk 0
    await disk1.readSectors(2, new Uint8Array(512)); // cache chunk 1
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);
    await disk1.readSectors(0, new Uint8Array(512)); // cache hit updates LRU order
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    // Re-open with a strict limit: should evict chunk 1 on open and keep chunk 0 (MRU from the cache hit).
    const disk2 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: chunkSize });
    await disk2.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1); // cache hit
    await disk2.readSectors(2, new Uint8Array(512));
    expect(hits.get("/chunks/00000001.bin")).toBe(2); // evicted => refetch
    await disk2.close();
  });

  it("coalesces meta.json writes for repeated cache hits", async () => {
    const chunkSize = 1024;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: chunkSize, sha256: await sha256Hex(chunks[0]!) }],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new CountingMetaWritesStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
      cacheLimitBytes: null,
    });

    // Prime the cache.
    await disk.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk.flush();

    const baseWrites = store.metaWrites;

    // Repeated cache hits should not trigger one meta.json write per access.
    for (let i = 0; i < 100; i += 1) {
      await disk.readSectors(0, new Uint8Array(512));
    }
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    await disk.close();

    const hitWrites = store.metaWrites - baseWrites;
    expect(hitWrites).toBeGreaterThan(0); // still persists eventually (best-effort)
    expect(hitWrites).toBeLessThanOrEqual(3); // should be dramatically less than 100 reads
  });

  it("retries on integrity mismatch and then fails", async () => {
    const chunkSize = 1024;
    const totalSize = 2048;
    const chunkCount = 2;

    const img = buildTestImageBytes(totalSize);
    const goodChunks = [img.slice(0, 1024), img.slice(1024, 2048)];

    // Corrupt chunk 0 on the wire.
    const corrupt0 = goodChunks[0]!.slice();
    corrupt0[0] ^= 0xff;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: 1024, sha256: await sha256Hex(goodChunks[0]!) },
        { size: 1024, sha256: await sha256Hex(goodChunks[1]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(corrupt0);
        return;
      }

      if (url.pathname === "/chunks/00000001.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(goodChunks[1]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store: new TestMemoryStore(),
      prefetchSequentialChunks: 0,
      maxAttempts: 2,
      retryBaseDelayMs: 0,
    });

    const buf = new Uint8Array(512);
    await expect(disk.readSectors(0, buf)).rejects.toThrow(/sha256 mismatch/i);
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    const t = disk.getTelemetrySnapshot();
    expect(t.cacheMisses).toBe(1);
    expect(t.requests).toBe(2);
    expect(t.bytesDownloaded).toBe(2048);
    expect(t.cachedBytes).toBe(0);
    expect(t.lastFetchMs).toBeNull();
    await disk.close();
  });

  it("does not wipe telemetry for reads that occur while clearCache is in-flight", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = 1024;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, 1024)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: 1024, sha256: await sha256Hex(chunks[0]!) }],
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new BlockingRemoveStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });

    store.armRecursiveRemoveBlock();
    const clearPromise = disk.clearCache();
    await store.waitForRecursiveRemove();

    const buf = new Uint8Array(512);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(img.slice(0, 512));

    store.releaseRecursiveRemove();
    await clearPromise;

    const t = disk.getTelemetrySnapshot();
    expect(t.requests).toBe(1);
    expect(t.bytesDownloaded).toBe(1024);

    await disk.close();
  });

  it(
    "streams large multi-chunk reads without unbounded inflight fetch state",
    async () => {
    const chunkSize = 1024 * 1024; // 1 MiB (multiple of 512)
    const chunkCount = 16;
    const totalSize = chunkSize * chunkCount;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
    };

    let activeChunkRequests = 0;
    let maxActiveChunkRequests = 0;
    const pendingResponses: Array<() => void> = [];
    let stalledResponses = 0;

    let twoChunkRequestsResolve: (() => void) | null = null;
    const twoChunkRequests = new Promise<void>((resolve) => {
      twoChunkRequestsResolve = resolve;
    });

    const { baseUrl, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        if (!Number.isSafeInteger(idx) || idx < 0 || idx >= chunkCount) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }

        activeChunkRequests += 1;
        maxActiveChunkRequests = Math.max(maxActiveChunkRequests, activeChunkRequests);
        res.on("finish", () => {
          activeChunkRequests -= 1;
        });

        const start = idx * chunkSize;
        const data = new Uint8Array(chunkSize);
        for (let i = 0; i < data.length; i += 1) data[i] = (start + i) & 0xff;

        const send = () => {
          res.statusCode = 200;
          res.setHeader("content-type", "application/octet-stream");
          res.end(data);
        };

        // Stall the first two chunk responses so the read stays in-flight long enough
        // to observe telemetry. This makes unbounded per-chunk promise creation
        // (and the resulting inflight map growth) deterministic in the test.
        if (stalledResponses < 2) {
          stalledResponses += 1;
          pendingResponses.push(send);
          if (stalledResponses === 2) twoChunkRequestsResolve?.();
          return;
        }

        send();
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store: new TestMemoryStore(),
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 2,
    });

    const buf = new Uint8Array(totalSize);

    let inflightBeforeRelease = 0;
    try {
      const readPromise = disk.readSectors(0, buf);

      await Promise.race([
        twoChunkRequests,
        new Promise<void>((_, reject) => setTimeout(() => reject(new Error("timed out waiting for chunk requests")), 2000)),
      ]);

      // Yield to ensure any queued chunk tasks have a chance to register as inflight.
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
      inflightBeforeRelease = disk.getTelemetrySnapshot().inflightFetches;

      // Release stalled responses and complete the read.
      for (const send of pendingResponses.splice(0)) send();
      await readPromise;

      for (let i = 0; i < buf.length; i += 1) {
        const expected = i & 0xff;
        const actual = buf[i]!;
        if (actual !== expected) {
          throw new Error(`read mismatch at byte=${i} expected=${expected} actual=${actual}`);
        }
      }
    } finally {
      // Best-effort: avoid leaking stalled HTTP responses and in-flight reads on failure.
      for (const send of pendingResponses.splice(0)) send();
      await disk.close();
    }

      expect(maxActiveChunkRequests).toBeLessThanOrEqual(2);
      expect(inflightBeforeRelease).toBeLessThanOrEqual(2);
    },
    30_000,
  );
});

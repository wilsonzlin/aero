import { openFileHandle, removeOpfsEntry } from "./opfs";

export type ByteRange = { start: number; end: number };

function rangeLen(r: ByteRange): number {
  return r.end - r.start;
}

function overlapsOrAdjacent(a: ByteRange, b: ByteRange): boolean {
  return a.start <= b.end && b.start <= a.end;
}

function mergeRanges(a: ByteRange, b: ByteRange): ByteRange {
  return { start: Math.min(a.start, b.start), end: Math.max(a.end, b.end) };
}

export class RangeSet {
  private ranges: ByteRange[] = [];

  getRanges(): ByteRange[] {
    return [...this.ranges];
  }

  totalLen(): number {
    return this.ranges.reduce((sum, r) => sum + rangeLen(r), 0);
  }

  containsRange(start: number, end: number): boolean {
    if (start >= end) return true;
    for (const r of this.ranges) {
      if (r.end <= start) continue;
      return r.start <= start && r.end >= end;
    }
    return false;
  }

  insert(start: number, end: number): void {
    if (start >= end) return;
    let next: ByteRange = { start, end };
    const out: ByteRange[] = [];
    let inserted = false;
    for (const r of this.ranges) {
      if (r.end < next.start) {
        out.push(r);
        continue;
      }
      if (next.end < r.start) {
        if (!inserted) {
          out.push(next);
          inserted = true;
        }
        out.push(r);
        continue;
      }
      next = mergeRanges(next, r);
    }
    if (!inserted) out.push(next);
    this.ranges = compactRanges(out);
  }

  remove(start: number, end: number): void {
    if (start >= end) return;
    const out: ByteRange[] = [];
    for (const r of this.ranges) {
      if (r.end <= start || r.start >= end) {
        out.push(r);
        continue;
      }
      if (r.start < start) out.push({ start: r.start, end: start });
      if (r.end > end) out.push({ start: end, end: r.end });
    }
    this.ranges = compactRanges(out);
  }
}

function compactRanges(ranges: ByteRange[]): ByteRange[] {
  if (ranges.length <= 1) return ranges;
  const sorted = [...ranges].sort((a, b) => a.start - b.start);
  const out: ByteRange[] = [];
  let cur = sorted[0]!;
  for (const r of sorted.slice(1)) {
    if (overlapsOrAdjacent(cur, r)) {
      cur = mergeRanges(cur, r);
    } else {
      out.push(cur);
      cur = r;
    }
  }
  out.push(cur);
  return out;
}

export type RemoteDiskProbeResult = {
  size: number;
  acceptRanges: string;
  rangeProbeStatus: number;
  partialOk: boolean;
  contentRange: string;
};

export async function probeRemoteDisk(url: string): Promise<RemoteDiskProbeResult> {
  const head = await fetch(url, { method: "HEAD" });
  if (!head.ok) {
    throw new Error(`HEAD failed: ${head.status} ${head.statusText}`);
  }

  const size = Number(head.headers.get("content-length") ?? "NaN");
  if (!Number.isFinite(size) || size <= 0) {
    throw new Error("Remote server did not provide a valid Content-Length.");
  }
  const acceptRanges = head.headers.get("accept-ranges") ?? "";

  const probe = await fetch(url, { method: "GET", headers: { Range: "bytes=0-0" } });
  const contentRange = probe.headers.get("content-range") ?? "";
  return {
    size,
    acceptRanges,
    rangeProbeStatus: probe.status,
    partialOk: probe.status === 206,
    contentRange,
  };
}

type CacheMeta = {
  version: 1;
  url: string;
  totalSize: number;
  blockSize: number;
  downloaded: ByteRange[];
  accessCounter: number;
  blockLastAccess: Record<string, number>;
};

export type RemoteDiskCacheStatus = {
  totalSize: number;
  cachedBytes: number;
  cachedRanges: ByteRange[];
  cacheLimitBytes: number | null;
};

export type RemoteDiskOptions = {
  blockSize?: number;
  cacheLimitBytes?: number | null;
  prefetchSequentialBlocks?: number;
};

export class RemoteStreamingDisk {
  private readonly url: string;
  private readonly totalSize: number;
  private readonly blockSize: number;
  private readonly cacheLimitBytes: number | null;
  private readonly prefetchSequentialBlocks: number;
  private readonly cacheKey: string;

  private meta: CacheMeta;
  private rangeSet: RangeSet;
  private lastReadEnd: number | null = null;

  private constructor(url: string, totalSize: number, cacheKey: string, options: Required<RemoteDiskOptions>) {
    this.url = url;
    this.totalSize = totalSize;
    this.blockSize = options.blockSize;
    this.cacheLimitBytes = options.cacheLimitBytes;
    this.prefetchSequentialBlocks = options.prefetchSequentialBlocks;
    this.cacheKey = cacheKey;

    this.meta = {
      version: 1,
      url,
      totalSize,
      blockSize: this.blockSize,
      downloaded: [],
      accessCounter: 0,
      blockLastAccess: {},
    };
    this.rangeSet = new RangeSet();
  }

  static async open(url: string, options: RemoteDiskOptions = {}): Promise<RemoteStreamingDisk> {
    const probe = await probeRemoteDisk(url);
    if (!probe.partialOk) {
      throw new Error(
        "Remote server does not appear to support HTTP Range requests (required). " +
          "Ensure it returns 206 Partial Content and exposes Content-Range via CORS.",
      );
    }

    const resolved: Required<RemoteDiskOptions> = {
      blockSize: options.blockSize ?? 1024 * 1024,
      cacheLimitBytes: options.cacheLimitBytes ?? 512 * 1024 * 1024,
      prefetchSequentialBlocks: options.prefetchSequentialBlocks ?? 2,
    };

    const cacheKey = await stableCacheKey(url);
    const disk = new RemoteStreamingDisk(url, probe.size, cacheKey, resolved);
    await disk.loadMeta();
    return disk;
  }

  async getCacheStatus(): Promise<RemoteDiskCacheStatus> {
    await this.loadMeta();
    return {
      totalSize: this.totalSize,
      cachedBytes: this.rangeSet.totalLen(),
      cachedRanges: this.rangeSet.getRanges(),
      cacheLimitBytes: this.cacheLimitBytes,
    };
  }

  async read(offset: number, length: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    if (length === 0) {
      this.lastReadEnd = offset;
      return new Uint8Array();
    }
    if (offset + length > this.totalSize) {
      throw new Error("Read beyond end of image.");
    }

    const startBlock = Math.floor(offset / this.blockSize);
    const endBlock = Math.floor((offset + length - 1) / this.blockSize);

    const out = new Uint8Array(length);
    let written = 0;

    for (let block = startBlock; block <= endBlock; block++) {
      const bytes = await this.getBlock(block, onLog);
      const blockStart = block * this.blockSize;
      const inBlockStart = offset > blockStart ? offset - blockStart : 0;
      const toCopy = Math.min(length - written, bytes.length - inBlockStart);
      out.set(bytes.subarray(inBlockStart, inBlockStart + toCopy), written);
      written += toCopy;
    }

    await this.maybePrefetch(offset, length, onLog);
    return out;
  }

  private async maybePrefetch(offset: number, length: number, onLog?: (msg: string) => void): Promise<void> {
    const sequential = this.lastReadEnd !== null && this.lastReadEnd === offset;
    this.lastReadEnd = offset + length;
    if (!sequential) return;

    const nextOffset = offset + length;
    const nextBlock = Math.floor(nextOffset / this.blockSize);
    for (let i = 0; i < this.prefetchSequentialBlocks; i++) {
      const block = nextBlock + i;
      if (block * this.blockSize >= this.totalSize) break;
      try {
        await this.getBlock(block, onLog);
      } catch {
        // best-effort prefetch
      }
    }
  }

  private blockRange(blockIndex: number): ByteRange {
    const start = blockIndex * this.blockSize;
    const end = Math.min(start + this.blockSize, this.totalSize);
    return { start, end };
  }

  private blockPath(blockIndex: number): string {
    return `state/remote-cache/${this.cacheKey}/blocks/${blockIndex}.bin`;
  }

  private metaPath(): string {
    return `state/remote-cache/${this.cacheKey}/meta.json`;
  }

  private noteAccess(blockIndex: number): void {
    this.meta.accessCounter++;
    this.meta.blockLastAccess[String(blockIndex)] = this.meta.accessCounter;
  }

  private async getBlock(blockIndex: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    await this.loadMeta();
    const r = this.blockRange(blockIndex);
    if (this.rangeSet.containsRange(r.start, r.end)) {
      try {
        const handle = await openFileHandle(this.blockPath(blockIndex), { create: false });
        const file = await handle.getFile();
        const bytes = new Uint8Array(await file.arrayBuffer());
        if (bytes.length === r.end - r.start) {
          this.noteAccess(blockIndex);
          await this.persistMeta();
          return bytes;
        }
      } catch {
        // treat as cache miss and heal metadata below
      }

      // Heal: metadata said cached but file missing/corrupt.
      this.rangeSet.remove(r.start, r.end);
      delete this.meta.blockLastAccess[String(blockIndex)];
      this.meta.downloaded = this.rangeSet.getRanges();
      await this.persistMeta();
    }

    onLog?.(`cache miss: fetching bytes=${r.start}-${r.end - 1}`);
    const resp = await fetch(this.url, { headers: { Range: `bytes=${r.start}-${r.end - 1}` } });
    if (resp.status !== 206) {
      throw new Error(`Expected 206 Partial Content, got ${resp.status}`);
    }
    const buf = new Uint8Array(await resp.arrayBuffer());
    if (buf.length !== r.end - r.start) {
      throw new Error(`Unexpected range length: expected ${r.end - r.start}, got ${buf.length}`);
    }

    const handle = await openFileHandle(this.blockPath(blockIndex), { create: true });
    const writable = await handle.createWritable();
    await writable.write(buf);
    await writable.close();

    this.rangeSet.insert(r.start, r.end);
    this.noteAccess(blockIndex);
    this.meta.downloaded = this.rangeSet.getRanges();
    await this.persistMeta();
    await this.enforceCacheLimit(blockIndex);
    return buf;
  }

  private async enforceCacheLimit(protectedBlock: number): Promise<void> {
    if (this.cacheLimitBytes === null) return;

    while (this.rangeSet.totalLen() > this.cacheLimitBytes) {
      let lruBlock: number | null = null;
      let lruCounter = Number.POSITIVE_INFINITY;
      for (const [blockStr, counter] of Object.entries(this.meta.blockLastAccess)) {
        const block = Number(blockStr);
        if (!Number.isFinite(block) || block === protectedBlock) continue;
        if (counter < lruCounter) {
          lruCounter = counter;
          lruBlock = block;
        }
      }

      if (lruBlock === null) break;

      const r = this.blockRange(lruBlock);
      await removeOpfsEntry(this.blockPath(lruBlock));
      this.rangeSet.remove(r.start, r.end);
      delete this.meta.blockLastAccess[String(lruBlock)];
      this.meta.downloaded = this.rangeSet.getRanges();
      await this.persistMeta();
    }
  }

  private async loadMeta(): Promise<void> {
    const path = this.metaPath();
    try {
      const handle = await openFileHandle(path, { create: false });
      const file = await handle.getFile();
      const raw = await file.text();
      const parsed = JSON.parse(raw) as CacheMeta;

      const compatible =
        parsed &&
        parsed.version === 1 &&
        parsed.url === this.url &&
        parsed.totalSize === this.totalSize &&
        parsed.blockSize === this.blockSize;

      if (!compatible) return;
      this.meta = parsed;
      this.rangeSet = new RangeSet();
      for (const r of parsed.downloaded) this.rangeSet.insert(r.start, r.end);
    } catch {
      // ignore missing / unreadable meta
    }
  }

  private async persistMeta(): Promise<void> {
    const handle = await openFileHandle(this.metaPath(), { create: true });
    const writable = await handle.createWritable();
    await writable.write(JSON.stringify(this.meta, null, 2));
    await writable.close();
  }
}

async function stableCacheKey(url: string): Promise<string> {
  // Use SHA-256 when available, fall back to a filesystem-safe encoding.
  try {
    const data = new TextEncoder().encode(url);
    const digest = await crypto.subtle.digest("SHA-256", data);
    const bytes = new Uint8Array(digest);
    return Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  } catch {
    return encodeURIComponent(url).replaceAll("%", "_").slice(0, 128);
  }
}


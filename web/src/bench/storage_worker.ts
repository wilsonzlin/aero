import type {
  StorageBenchApiMode,
  StorageBenchBackend,
  StorageBenchLatencyRun,
  StorageBenchLatencySummary,
  StorageBenchOpts,
  StorageBenchResult,
  StorageBenchThroughputRun,
  StorageBenchThroughputSummary,
} from "./storage_types";
import { createRandomSource, randomAlignedOffset, randomInt } from "./seeded_rng";
import { formatOneLineError } from "../text";

type WorkerRequest = { type: "run"; id: string; opts?: StorageBenchOpts };
type WorkerResponse =
  | { type: "result"; id: string; result: StorageBenchResult }
  | { type: "error"; id: string; error: string };

const KiB = 1024;
const MiB = 1024 * 1024;
const BLOCK_4K = 4 * KiB;

function mbPerSec(bytes: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return bytes / MiB / (durationMs / 1000);
}

function clampInt(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.floor(value)));
}

function createRunId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function mean(values: number[]): number {
  if (values.length === 0) return 0;
  return values.reduce((a, b) => a + b, 0) / values.length;
}

function stdev(values: number[]): number {
  if (values.length < 2) return 0;
  const m = mean(values);
  const variance =
    values.reduce((acc, v) => acc + (v - m) * (v - m), 0) / values.length;
  return Math.sqrt(variance);
}

function percentile(sortedValues: number[], pct: number): number {
  if (sortedValues.length === 0) return 0;
  const idx = Math.floor((pct / 100) * (sortedValues.length - 1));
  return sortedValues[idx] ?? sortedValues[sortedValues.length - 1]!;
}

function summarizeThroughput(runs: StorageBenchThroughputRun[]): StorageBenchThroughputSummary {
  const mbps = runs.map((r) => r.mb_per_s);
  return {
    runs,
    mean_mb_per_s: mean(mbps),
    stdev_mb_per_s: stdev(mbps),
  };
}

function summarizeLatency(runs: StorageBenchLatencyRun[]): StorageBenchLatencySummary {
  const p50s = runs.map((r) => r.p50_ms);
  const p95s = runs.map((r) => r.p95_ms);
  return {
    runs,
    mean_p50_ms: mean(p50s),
    mean_p95_ms: mean(p95s),
    stdev_p50_ms: stdev(p50s),
    stdev_p95_ms: stdev(p95s),
  };
}

function toErrorString(err: unknown): string {
  const msg = formatOneLineError(err, 512);
  let name = "";
  try {
    name = err && typeof err === "object" && typeof (err as { name?: unknown }).name === "string" ? (err as { name: string }).name : "";
  } catch {
    name = "";
  }
  if (name && msg && !msg.toLowerCase().startsWith(name.toLowerCase())) return `${name}: ${msg}`;
  return msg;
}

function createChunkBuffer(bytes: number): Uint8Array<ArrayBuffer> {
  const buf = new Uint8Array(new ArrayBuffer(bytes));
  for (let i = 0; i < buf.length; i++) {
    buf[i] = i & 0xff;
  }
  return buf;
}

function createBlockBuffer(bytes: number): Uint8Array<ArrayBuffer> {
  const buf = new Uint8Array(new ArrayBuffer(bytes));
  for (let i = 0; i < buf.length; i++) {
    buf[i] = (i * 31) & 0xff;
  }
  return buf;
}

function resolveOpts(opts?: StorageBenchOpts): {
  runId: string;
  config: StorageBenchResult["config"];
} {
  const runId = opts?.run_id ?? createRunId();

  const backend = opts?.backend ?? "auto";
  const random_seed =
    typeof opts?.random_seed === "number" && Number.isFinite(opts.random_seed)
      ? clampInt(opts.random_seed, 0, 0xffffffff)
      : undefined;
  const seq_total_mb = clampInt(opts?.seq_total_mb ?? 64, 4, 512);
  const seq_chunk_mb = clampInt(opts?.seq_chunk_mb ?? 4, 1, 8);
  const seq_runs = clampInt(opts?.seq_runs ?? 3, 1, 10);
  const warmup_mb = clampInt(opts?.warmup_mb ?? 8, 0, 64);
  const random_ops = clampInt(opts?.random_ops ?? 1000, 10, 20000);
  const random_runs = clampInt(opts?.random_runs ?? 2, 1, 10);
  const random_space_mb = clampInt(opts?.random_space_mb ?? 4, 1, 256);
  const include_random_write = Boolean(opts?.include_random_write ?? false);

  return {
    runId,
    config: {
      backend,
      random_seed,
      seq_total_mb,
      seq_chunk_mb,
      seq_runs,
      warmup_mb,
      random_ops,
      random_runs,
      random_space_mb,
      include_random_write,
    },
  };
}

async function runOpfsBench(params: {
  runId: string;
  config: StorageBenchResult["config"];
}): Promise<StorageBenchResult> {
  const warnings: string[] = [];

  const root = await navigator.storage.getDirectory();
  const benchDir = await root.getDirectoryHandle("aero-bench", { create: true });
  const runDir = await benchDir.getDirectoryHandle(params.runId, { create: true });
  const fileHandle = await runDir.getFileHandle("storage-io.bin", { create: true });

  const totalBytes = params.config.seq_total_mb * MiB;
  const chunkBytes = params.config.seq_chunk_mb * MiB;
  const warmupBytes = Math.min(totalBytes, params.config.warmup_mb * MiB);
  const randomSpaceBytes = Math.min(totalBytes, params.config.random_space_mb * MiB);

  let apiMode: StorageBenchApiMode = "async";
  let accessHandle: any | undefined;

  try {
    const createSyncAccessHandle = (fileHandle as unknown as { createSyncAccessHandle?: unknown }).createSyncAccessHandle;
    if (typeof createSyncAccessHandle === "function") {
      try {
        accessHandle = await (createSyncAccessHandle as (this: unknown) => Promise<unknown>).call(fileHandle);
        apiMode = "sync_access_handle";
      } catch (err) {
        warnings.push(`OPFS sync access handle unavailable: ${toErrorString(err)}`);
      }
    }

    if (apiMode === "sync_access_handle") {
      const chunk = createChunkBuffer(chunkBytes);
      const readBuf = new Uint8Array(chunkBytes);
      const block = createBlockBuffer(BLOCK_4K);
      const blockReadBuf = new Uint8Array(BLOCK_4K);

      if (warmupBytes > 0) {
        accessHandle.truncate(0);
        for (let offset = 0; offset < warmupBytes; offset += chunkBytes) {
          const size = Math.min(chunkBytes, warmupBytes - offset);
          accessHandle.write(size === chunkBytes ? chunk : chunk.subarray(0, size), {
            at: offset,
          });
        }
        accessHandle.flush();
        for (let offset = 0; offset < warmupBytes; offset += chunkBytes) {
          const size = Math.min(chunkBytes, warmupBytes - offset);
          accessHandle.read(size === chunkBytes ? readBuf : readBuf.subarray(0, size), {
            at: offset,
          });
        }
      }

      const writeRuns: StorageBenchThroughputRun[] = [];
      for (let i = 0; i < params.config.seq_runs; i++) {
        const start = performance.now();
        accessHandle.truncate(0);
        for (let offset = 0; offset < totalBytes; offset += chunkBytes) {
          const size = Math.min(chunkBytes, totalBytes - offset);
          accessHandle.write(size === chunkBytes ? chunk : chunk.subarray(0, size), {
            at: offset,
          });
        }
        accessHandle.flush();
        const duration = performance.now() - start;
        writeRuns.push({
          bytes: totalBytes,
          duration_ms: duration,
          mb_per_s: mbPerSec(totalBytes, duration),
        });
      }

      const readRuns: StorageBenchThroughputRun[] = [];
      for (let i = 0; i < params.config.seq_runs; i++) {
        const start = performance.now();
        for (let offset = 0; offset < totalBytes; offset += chunkBytes) {
          const size = Math.min(chunkBytes, totalBytes - offset);
          accessHandle.read(size === chunkBytes ? readBuf : readBuf.subarray(0, size), {
            at: offset,
          });
        }
        const duration = performance.now() - start;
        readRuns.push({
          bytes: totalBytes,
          duration_ms: duration,
          mb_per_s: mbPerSec(totalBytes, duration),
        });
      }

      const randomReadRuns: StorageBenchLatencyRun[] = [];
      for (let run = 0; run < params.config.random_runs; run++) {
        const rand = createRandomSource(params.config.random_seed, 1000 + run);
        const latencies: number[] = [];
        let min = Number.POSITIVE_INFINITY;
        let max = 0;
        for (let op = 0; op < params.config.random_ops; op++) {
          const offset = randomAlignedOffset(randomSpaceBytes, BLOCK_4K, rand);
          const t0 = performance.now();
          accessHandle.read(blockReadBuf, { at: offset });
          const dt = performance.now() - t0;
          latencies.push(dt);
          min = Math.min(min, dt);
          max = Math.max(max, dt);
        }

        latencies.sort((a, b) => a - b);
        randomReadRuns.push({
          ops: params.config.random_ops,
          block_bytes: BLOCK_4K,
          min_ms: min === Number.POSITIVE_INFINITY ? 0 : min,
          max_ms: max,
          mean_ms: mean(latencies),
          stdev_ms: stdev(latencies),
          p50_ms: percentile(latencies, 50),
          p95_ms: percentile(latencies, 95),
        });
      }

      let randomWriteSummary: StorageBenchLatencySummary | undefined;
      if (params.config.include_random_write) {
        const randomWriteRuns: StorageBenchLatencyRun[] = [];
        for (let run = 0; run < params.config.random_runs; run++) {
          const rand = createRandomSource(params.config.random_seed, 2000 + run);
          const latencies: number[] = [];
          let min = Number.POSITIVE_INFINITY;
          let max = 0;
          for (let op = 0; op < params.config.random_ops; op++) {
            const offset = randomAlignedOffset(randomSpaceBytes, BLOCK_4K, rand);
            const t0 = performance.now();
            accessHandle.write(block, { at: offset });
            const dt = performance.now() - t0;
            latencies.push(dt);
            min = Math.min(min, dt);
            max = Math.max(max, dt);
          }
          accessHandle.flush();
          latencies.sort((a, b) => a - b);
          randomWriteRuns.push({
            ops: params.config.random_ops,
            block_bytes: BLOCK_4K,
            min_ms: min === Number.POSITIVE_INFINITY ? 0 : min,
            max_ms: max,
            mean_ms: mean(latencies),
            stdev_ms: stdev(latencies),
            p50_ms: percentile(latencies, 50),
            p95_ms: percentile(latencies, 95),
          });
        }
        randomWriteSummary = summarizeLatency(randomWriteRuns);
      }

      return {
        version: 1,
        run_id: params.runId,
        backend: "opfs",
        api_mode: apiMode,
        config: params.config,
        sequential_write: summarizeThroughput(writeRuns),
        sequential_read: summarizeThroughput(readRuns),
        random_read_4k: summarizeLatency(randomReadRuns),
        random_write_4k: randomWriteSummary,
        warnings: warnings.length > 0 ? warnings : undefined,
      };
    }

    const chunk = createChunkBuffer(chunkBytes);
    const block = createBlockBuffer(BLOCK_4K);

    if (warmupBytes > 0) {
      const warmupWriter = await fileHandle.createWritable();
      for (let offset = 0; offset < warmupBytes; offset += chunkBytes) {
        const size = Math.min(chunkBytes, warmupBytes - offset);
        await warmupWriter.write(size === chunkBytes ? chunk : chunk.subarray(0, size));
      }
      await warmupWriter.close();

      const file = await fileHandle.getFile();
      for (let offset = 0; offset < warmupBytes; offset += chunkBytes) {
        const size = Math.min(chunkBytes, warmupBytes - offset);
        await file.slice(offset, offset + size).arrayBuffer();
      }
    }

    const writeRuns: StorageBenchThroughputRun[] = [];
    for (let i = 0; i < params.config.seq_runs; i++) {
      const start = performance.now();
      const writer = await fileHandle.createWritable();
      for (let offset = 0; offset < totalBytes; offset += chunkBytes) {
        const size = Math.min(chunkBytes, totalBytes - offset);
        await writer.write(size === chunkBytes ? chunk : chunk.subarray(0, size));
      }
      await writer.close();
      const duration = performance.now() - start;
      writeRuns.push({
        bytes: totalBytes,
        duration_ms: duration,
        mb_per_s: mbPerSec(totalBytes, duration),
      });
    }

    const readRuns: StorageBenchThroughputRun[] = [];
    for (let i = 0; i < params.config.seq_runs; i++) {
      const start = performance.now();
      const file = await fileHandle.getFile();
      for (let offset = 0; offset < totalBytes; offset += chunkBytes) {
        const size = Math.min(chunkBytes, totalBytes - offset);
        await file.slice(offset, offset + size).arrayBuffer();
      }
      const duration = performance.now() - start;
      readRuns.push({
        bytes: totalBytes,
        duration_ms: duration,
        mb_per_s: mbPerSec(totalBytes, duration),
      });
    }

    const randomReadRuns: StorageBenchLatencyRun[] = [];
    for (let run = 0; run < params.config.random_runs; run++) {
      const file = await fileHandle.getFile();
      const rand = createRandomSource(params.config.random_seed, 3000 + run);
      const latencies: number[] = [];
      let min = Number.POSITIVE_INFINITY;
      let max = 0;
      for (let op = 0; op < params.config.random_ops; op++) {
        const offset = randomAlignedOffset(randomSpaceBytes, BLOCK_4K, rand);
        const blob = file.slice(offset, offset + BLOCK_4K);
        const t0 = performance.now();
        await blob.arrayBuffer();
        const dt = performance.now() - t0;
        latencies.push(dt);
        min = Math.min(min, dt);
        max = Math.max(max, dt);
      }

      latencies.sort((a, b) => a - b);
      randomReadRuns.push({
        ops: params.config.random_ops,
        block_bytes: BLOCK_4K,
        min_ms: min === Number.POSITIVE_INFINITY ? 0 : min,
        max_ms: max,
        mean_ms: mean(latencies),
        stdev_ms: stdev(latencies),
        p50_ms: percentile(latencies, 50),
        p95_ms: percentile(latencies, 95),
      });
    }

    let randomWriteSummary: StorageBenchLatencySummary | undefined;
    if (params.config.include_random_write) {
      const randomWriteRuns: StorageBenchLatencyRun[] = [];
      for (let run = 0; run < params.config.random_runs; run++) {
        let writer: FileSystemWritableFileStream;
        try {
          writer = await fileHandle.createWritable({ keepExistingData: true });
        } catch {
          writer = await fileHandle.createWritable();
        }
        const rand = createRandomSource(params.config.random_seed, 4000 + run);
        const latencies: number[] = [];
        let min = Number.POSITIVE_INFINITY;
        let max = 0;
        for (let op = 0; op < params.config.random_ops; op++) {
          const offset = randomAlignedOffset(randomSpaceBytes, BLOCK_4K, rand);
          const t0 = performance.now();
          await writer.write({ type: "write", position: offset, data: block });
          const dt = performance.now() - t0;
          latencies.push(dt);
          min = Math.min(min, dt);
          max = Math.max(max, dt);
        }
        await writer.close();
        latencies.sort((a, b) => a - b);
        randomWriteRuns.push({
          ops: params.config.random_ops,
          block_bytes: BLOCK_4K,
          min_ms: min === Number.POSITIVE_INFINITY ? 0 : min,
          max_ms: max,
          mean_ms: mean(latencies),
          stdev_ms: stdev(latencies),
          p50_ms: percentile(latencies, 50),
          p95_ms: percentile(latencies, 95),
        });
      }
      randomWriteSummary = summarizeLatency(randomWriteRuns);
    }

    return {
      version: 1,
      run_id: params.runId,
      backend: "opfs",
      api_mode: apiMode,
      config: params.config,
      sequential_write: summarizeThroughput(writeRuns),
      sequential_read: summarizeThroughput(readRuns),
      random_read_4k: summarizeLatency(randomReadRuns),
      random_write_4k: randomWriteSummary,
      warnings: warnings.length > 0 ? warnings : undefined,
    };
  } finally {
    try {
      accessHandle?.close();
    } catch {
    }
    try {
      await benchDir.removeEntry(params.runId, { recursive: true });
    } catch {
    }
  }
}

function openIndexedDb(dbName: string): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(dbName, 1);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains("chunks")) db.createObjectStore("chunks");
      if (!db.objectStoreNames.contains("blocks4k")) db.createObjectStore("blocks4k");
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB open failed"));
  });
}

function waitForTransaction(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction aborted"));
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB transaction error"));
  });
}

function deleteIndexedDb(dbName: string): Promise<void> {
  return new Promise((resolve) => {
    const request = indexedDB.deleteDatabase(dbName);
    request.onsuccess = () => resolve();
    request.onerror = () => resolve();
    request.onblocked = () => resolve();
  });
}

async function runIndexedDbBench(params: {
  runId: string;
  config: StorageBenchResult["config"];
}): Promise<StorageBenchResult> {
  const warnings: string[] = [];
  const dbName = `aero-bench-${params.runId}`;
  const totalBytes = params.config.seq_total_mb * MiB;
  const chunkBytes = params.config.seq_chunk_mb * MiB;
  const warmupBytes = Math.min(totalBytes, params.config.warmup_mb * MiB);
  const randomSpaceBytes = Math.min(totalBytes, params.config.random_space_mb * MiB);

  const chunks = Math.ceil(totalBytes / chunkBytes);
  const warmupChunks = warmupBytes > 0 ? Math.ceil(warmupBytes / chunkBytes) : 0;
  const randomBlocks = Math.ceil(randomSpaceBytes / BLOCK_4K);

  const chunk = createChunkBuffer(chunkBytes);
  const block = createBlockBuffer(BLOCK_4K);

  let db: IDBDatabase | undefined;

  try {
    db = await openIndexedDb(dbName);

    if (warmupChunks > 0) {
      const tx = db.transaction(["chunks"], "readwrite");
      const store = tx.objectStore("chunks");
      for (let i = 0; i < warmupChunks; i++) {
        const offset = i * chunkBytes;
        const size = Math.min(chunkBytes, warmupBytes - offset);
        store.put(size === chunkBytes ? chunk : chunk.subarray(0, size), i);
      }
      await waitForTransaction(tx);

      const txRead = db.transaction(["chunks"], "readonly");
      const storeRead = txRead.objectStore("chunks");
      for (let i = 0; i < warmupChunks; i++) {
        storeRead.get(i);
      }
      await waitForTransaction(txRead);
    }

    const writeRuns: StorageBenchThroughputRun[] = [];
    for (let run = 0; run < params.config.seq_runs; run++) {
      const start = performance.now();
      const tx = db.transaction(["chunks"], "readwrite");
      const store = tx.objectStore("chunks");
      for (let i = 0; i < chunks; i++) {
        const offset = i * chunkBytes;
        const size = Math.min(chunkBytes, totalBytes - offset);
        store.put(size === chunkBytes ? chunk : chunk.subarray(0, size), i);
      }
      await waitForTransaction(tx);
      const duration = performance.now() - start;
      writeRuns.push({
        bytes: totalBytes,
        duration_ms: duration,
        mb_per_s: mbPerSec(totalBytes, duration),
      });
    }

    const readRuns: StorageBenchThroughputRun[] = [];
    for (let run = 0; run < params.config.seq_runs; run++) {
      const start = performance.now();
      const tx = db.transaction(["chunks"], "readonly");
      const store = tx.objectStore("chunks");
      for (let i = 0; i < chunks; i++) {
        store.get(i);
      }
      await waitForTransaction(tx);
      const duration = performance.now() - start;
      readRuns.push({
        bytes: totalBytes,
        duration_ms: duration,
        mb_per_s: mbPerSec(totalBytes, duration),
      });
    }

    if (randomBlocks > 0) {
      const tx = db.transaction(["blocks4k"], "readwrite");
      const store = tx.objectStore("blocks4k");
      for (let i = 0; i < randomBlocks; i++) {
        store.put(block, i);
      }
      await waitForTransaction(tx);
    } else {
      warnings.push("random_space_mb too small for 4KB random I/O sample set");
    }

    const randomReadRuns: StorageBenchLatencyRun[] = [];
    for (let run = 0; run < params.config.random_runs; run++) {
      const rand = createRandomSource(params.config.random_seed, 5000 + run);
      const latencies: number[] = [];
      let min = Number.POSITIVE_INFINITY;
      let max = 0;
      for (let op = 0; op < params.config.random_ops; op++) {
        const key = randomInt(Math.max(1, randomBlocks), rand);
        const t0 = performance.now();
        const tx = db.transaction(["blocks4k"], "readonly");
        const req = tx.objectStore("blocks4k").get(key);
        await new Promise<void>((resolve, reject) => {
          req.onsuccess = () => resolve();
          req.onerror = () => reject(req.error ?? new Error("IndexedDB get failed"));
        });
        await waitForTransaction(tx);
        const dt = performance.now() - t0;
        latencies.push(dt);
        min = Math.min(min, dt);
        max = Math.max(max, dt);
      }

      latencies.sort((a, b) => a - b);
      randomReadRuns.push({
        ops: params.config.random_ops,
        block_bytes: BLOCK_4K,
        min_ms: min === Number.POSITIVE_INFINITY ? 0 : min,
        max_ms: max,
        mean_ms: mean(latencies),
        stdev_ms: stdev(latencies),
        p50_ms: percentile(latencies, 50),
        p95_ms: percentile(latencies, 95),
      });
    }

    let randomWriteSummary: StorageBenchLatencySummary | undefined;
    if (params.config.include_random_write) {
      const randomWriteRuns: StorageBenchLatencyRun[] = [];
      for (let run = 0; run < params.config.random_runs; run++) {
        const rand = createRandomSource(params.config.random_seed, 6000 + run);
        const latencies: number[] = [];
        let min = Number.POSITIVE_INFINITY;
        let max = 0;
        for (let op = 0; op < params.config.random_ops; op++) {
          const key = randomInt(Math.max(1, randomBlocks), rand);
          const t0 = performance.now();
          const tx = db.transaction(["blocks4k"], "readwrite");
          const req = tx.objectStore("blocks4k").put(block, key);
          await new Promise<void>((resolve, reject) => {
            req.onsuccess = () => resolve();
            req.onerror = () => reject(req.error ?? new Error("IndexedDB put failed"));
          });
          await waitForTransaction(tx);
          const dt = performance.now() - t0;
          latencies.push(dt);
          min = Math.min(min, dt);
          max = Math.max(max, dt);
        }
        latencies.sort((a, b) => a - b);
        randomWriteRuns.push({
          ops: params.config.random_ops,
          block_bytes: BLOCK_4K,
          min_ms: min === Number.POSITIVE_INFINITY ? 0 : min,
          max_ms: max,
          mean_ms: mean(latencies),
          stdev_ms: stdev(latencies),
          p50_ms: percentile(latencies, 50),
          p95_ms: percentile(latencies, 95),
        });
      }
      randomWriteSummary = summarizeLatency(randomWriteRuns);
    }

    return {
      version: 1,
      run_id: params.runId,
      backend: "indexeddb",
      api_mode: "async",
      config: params.config,
      sequential_write: summarizeThroughput(writeRuns),
      sequential_read: summarizeThroughput(readRuns),
      random_read_4k: summarizeLatency(randomReadRuns),
      random_write_4k: randomWriteSummary,
      warnings: warnings.length > 0 ? warnings : undefined,
    };
  } finally {
    try {
      db?.close();
    } catch {
    }
    await deleteIndexedDb(dbName);
  }
}

self.addEventListener("message", (event: MessageEvent<WorkerRequest>) => {
  const msg = event.data;
  if (!msg || msg.type !== "run") return;

  void (async () => {
    const resolved = resolveOpts(msg.opts);
    const requestedBackend = resolved.config.backend;
    const warnings: string[] = [];

    try {
      let result: StorageBenchResult | undefined;
      if (requestedBackend === "opfs" || requestedBackend === "auto") {
        try {
          result = await runOpfsBench(resolved);
        } catch (err) {
          if (requestedBackend === "opfs") throw err;
          warnings.push(`OPFS failed, falling back to IndexedDB: ${toErrorString(err)}`);
        }
      }

      if (!result) {
        result = await runIndexedDbBench(resolved);
      }

      if (warnings.length > 0) {
        result.warnings = [...(result.warnings ?? []), ...warnings];
      }

      const response: WorkerResponse = { type: "result", id: msg.id, result };
      self.postMessage(response);
    } catch (err) {
      const response: WorkerResponse = {
        type: "error",
        id: msg.id,
        error: toErrorString(err),
      };
      self.postMessage(response);
    }
  })();
});

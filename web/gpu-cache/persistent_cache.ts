// @ts-nocheck

// Persistent cache for expensive GPU/translation artifacts.
//
// Goals:
// - Avoid repeated DXBC -> WGSL translation work across browser sessions.
// - Persist derived pipeline descriptors to allow warming in-memory caches.
//
// Storage model:
// - IndexedDB is used as the index + metadata store (key -> {lastUsed,size,...}).
// - OPFS is used as an optional blob store for large payloads (WGSL + metadata JSON).
//   If OPFS is unavailable/fails, we fall back to storing the payload directly in IDB.
//
// Notes:
// - GPU objects (GPUShaderModule/GPURenderPipeline) are NOT serializable and therefore
//   are re-created each session. This cache only removes translation/descriptor work.
// - Cache keys include:
//   - a SHA-256 hash of the "content bytes" (DXBC + translation flags)
//   - an explicit `CACHE_SCHEMA_VERSION`
//   - a `backendKind` string (to separate translation pipelines)
//   - an optional device fingerprint (capabilities hash)
//
// This file intentionally avoids TypeScript-only syntax so it can be served directly
// as an ES module in tests without a build step.

// NOTE: Keep this name stable; it is user data that persists across sessions.
// If the schema changes incompatibly, bump `DB_VERSION` (to drop stores) and/or
// bump `CACHE_SCHEMA_VERSION` (to orphan old keys).
const DB_NAME = "aero-gpu-cache";
const DB_VERSION = 1;

const STORE_SHADERS = "shaders";
const STORE_PIPELINES = "pipelines";

// Bump this whenever the persisted value schema or key derivation changes in a
// way that would make old entries invalid to read.
//
// This version is embedded into cache keys so old entries become unreachable
// without needing to bump IndexedDB's schema version.
export const CACHE_SCHEMA_VERSION = 1;

// Backend "kinds" are included in keys to prevent accidental cross-contamination
// between different translation pipelines or codegen modes.
export const BACKEND_KIND_DXBC_TO_WGSL = "dxbc-to-wgsl";
export const BACKEND_KIND_PIPELINE_DESC = "pipeline-desc";

// Store small payloads directly in IndexedDB (fast/simple), and only spill to
// OPFS for larger blobs to avoid excessive file handle churn.
const OPFS_MIN_BYTES = 256 * 1024;

const DEFAULT_LIMITS = Object.freeze({
  shaders: { maxEntries: 2048, maxBytes: 64 * 1024 * 1024 },
  pipelines: { maxEntries: 4096, maxBytes: 32 * 1024 * 1024 },
});

/**
 * @param {any} value
 * @returns {string}
 */
function stableStringify(value) {
  if (value === null || value === undefined) return String(value);
  const t = typeof value;
  if (t === "string") return JSON.stringify(value);
  if (t === "number" || t === "boolean") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(",")}]`;
  if (t === "object") {
    const keys = Object.keys(value).sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${stableStringify(value[k])}`).join(",")}}`;
  }
  // Functions/symbols/bigints should never appear in persisted structures.
  return JSON.stringify(String(value));
}

/**
 * @param {string} part
 * @returns {string}
 */
function sanitizeKeyPart(part) {
  // The cache key may be reused as an OPFS file name, so keep it conservative.
  return String(part).replace(/[^a-zA-Z0-9._-]/g, "_");
}

/**
 * @param {Uint8Array[]} parts
 * @returns {Uint8Array}
 */
function concatBytes(parts) {
  let total = 0;
  for (const p of parts) total += p.byteLength;
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.byteLength;
  }
  return out;
}

/**
 * @param {ArrayBuffer|Uint8Array} buf
 * @returns {Uint8Array}
 */
function toU8(buf) {
  return buf instanceof Uint8Array ? buf : new Uint8Array(buf);
}

/**
 * @param {ArrayBuffer|Uint8Array} data
 * @returns {Promise<string>}
 */
export async function sha256Hex(data) {
  const digest = await crypto.subtle.digest("SHA-256", data instanceof Uint8Array ? data : new Uint8Array(data));
  const bytes = new Uint8Array(digest);
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/**
 * Compute a cache key using the standard Aero GPU cache scheme:
 *
 * `CacheKey = hash(content_bytes) + schema_version + backend_kind + device_fingerprint(optional)`
 *
 * This returns a *string* key that is safe to use as:
 * - an IndexedDB key
 * - an OPFS file name prefix
 *
 * @param {{schemaVersion:number, backendKind:string, deviceFingerprint?:string|null, contentHash:string}} parts
 * @returns {string}
 */
export function formatCacheKey(parts) {
  const v = parts.schemaVersion;
  const backend = sanitizeKeyPart(parts.backendKind);
  const device = parts.deviceFingerprint ? sanitizeKeyPart(parts.deviceFingerprint) : "none";
  const hash = parts.contentHash;
  return `gpu-cache-v${v}-${backend}-${device}-${hash}`;
}

/**
 * Compute a stable cache key for shader translation artifacts.
 *
 * @param {ArrayBuffer|Uint8Array} dxbc
 * @param {{
 *   halfPixelCenter?: boolean,
 *   // Optional device fingerprint / capability hash. Include this only if the
 *   // codegen can vary based on device limits or feature availability.
 *   capsHash?: string | null,
 *   [k: string]: any
 * }} flags
 * @returns {Promise<string>}
 */
export async function computeShaderCacheKey(dxbc, flags) {
  const enc = new TextEncoder();
  const canonicalFlags = { ...flags, halfPixelCenter: !!flags.halfPixelCenter };

  // Treat capsHash as the optional device fingerprint component.
  const deviceFingerprint = canonicalFlags.capsHash ?? null;
  delete canonicalFlags.capsHash;

  // Hash the *content bytes* that determine the translation output: DXBC bytes
  // plus translation flags (excluding device fingerprint, which is already a
  // dedicated key component).
  const metaBytes = enc.encode(stableStringify(canonicalFlags));
  const contentBytes = concatBytes([toU8(dxbc), metaBytes]);
  const contentHash = await sha256Hex(contentBytes);

  return formatCacheKey({
    schemaVersion: CACHE_SCHEMA_VERSION,
    backendKind: BACKEND_KIND_DXBC_TO_WGSL,
    deviceFingerprint,
    contentHash,
  });
}

/**
 * Compute a stable key for pipeline descriptors (NOT compiled pipelines).
 *
 * @param {any} pipelineDesc
 * @returns {Promise<string>}
 */
export async function computePipelineCacheKey(pipelineDesc) {
  const enc = new TextEncoder();
  const contentBytes = enc.encode(stableStringify(pipelineDesc));
  const contentHash = await sha256Hex(contentBytes);
  return formatCacheKey({
    schemaVersion: CACHE_SCHEMA_VERSION,
    backendKind: BACKEND_KIND_PIPELINE_DESC,
    deviceFingerprint: null,
    contentHash,
  });
}

const WEBGPU_LIMIT_KEYS = [
  "maxTextureDimension1D",
  "maxTextureDimension2D",
  "maxTextureDimension3D",
  "maxTextureArrayLayers",
  "maxBindGroups",
  "maxBindGroupsPlusVertexBuffers",
  "maxBindingsPerBindGroup",
  "maxDynamicUniformBuffersPerPipelineLayout",
  "maxDynamicStorageBuffersPerPipelineLayout",
  "maxSampledTexturesPerShaderStage",
  "maxSamplersPerShaderStage",
  "maxStorageBuffersPerShaderStage",
  "maxStorageTexturesPerShaderStage",
  "maxUniformBuffersPerShaderStage",
  "maxUniformBufferBindingSize",
  "maxStorageBufferBindingSize",
  "minUniformBufferOffsetAlignment",
  "minStorageBufferOffsetAlignment",
  "maxVertexBuffers",
  "maxBufferSize",
  "maxVertexAttributes",
  "maxVertexBufferArrayStride",
  "maxInterStageShaderComponents",
  "maxInterStageShaderVariables",
  "maxColorAttachments",
  "maxColorAttachmentBytesPerSample",
  "maxComputeWorkgroupStorageSize",
  "maxComputeInvocationsPerWorkgroup",
  "maxComputeWorkgroupSizeX",
  "maxComputeWorkgroupSizeY",
  "maxComputeWorkgroupSizeZ",
  "maxComputeWorkgroupsPerDimension",
];

/**
 * Compute a stable hash of the WebGPU adapter/device capabilities that can
 * influence shader translation output.
 *
 * Callers should include this string in the shader cache flags (`capsHash`) so
 * entries are safely invalidated when capabilities/limits change.
 *
 * Note: `GPUSupportedLimits` exposes its fields as WebIDL attributes, which are
 * not enumerable; we probe a fixed list of known limit names.
 *
 * @param {GPUAdapter | GPUDevice} adapterOrDevice
 * @returns {Promise<string>}
 */
export async function computeWebGpuCapsHash(adapterOrDevice) {
  const enc = new TextEncoder();

  const features = adapterOrDevice?.features ? Array.from(adapterOrDevice.features.values()).sort() : [];

  const limitsObj = adapterOrDevice?.limits ?? null;
  /** @type {Record<string, number>} */
  const limits = {};
  if (limitsObj) {
    for (const k of WEBGPU_LIMIT_KEYS) {
      const v = limitsObj[k];
      if (typeof v === "number" && Number.isFinite(v)) {
        limits[k] = v;
      }
    }
  }

  // WGSL language features are a browser-level capability (not per-adapter).
  /** @type {string[]} */
  let wgslFeatures = [];
  try {
    const gpu = /** @type {any} */ (navigator)?.gpu;
    const lf = gpu?.wgslLanguageFeatures;
    if (lf && typeof lf.values === "function") {
      wgslFeatures = Array.from(lf.values()).sort();
    }
  } catch {
    // Ignore.
  }

  // Adapter info is optional and may be blocked by permissions/user agent.
  /** @type {any} */
  let adapterInfo = null;
  const maybeAdapter = /** @type {any} */ (adapterOrDevice);
  try {
    if (typeof maybeAdapter.requestAdapterInfo === "function") {
      adapterInfo = await maybeAdapter.requestAdapterInfo();
    } else if (maybeAdapter.info) {
      adapterInfo = maybeAdapter.info;
    }
  } catch {
    adapterInfo = null;
  }

  return await sha256Hex(
    enc.encode(
      stableStringify({
        features,
        limits,
        wgslFeatures,
        adapterInfo,
      }),
    ),
  );
}

/**
 * @param {IDBRequest<any>} req
 * @returns {Promise<any>}
 */
function reqToPromise(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("IndexedDB request failed"));
  });
}

/**
 * @param {IDBTransaction} tx
 * @returns {Promise<void>}
 */
function txDone(tx) {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction aborted"));
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB transaction error"));
  });
}

/**
 * @returns {Promise<{root: FileSystemDirectoryHandle, cacheDir: FileSystemDirectoryHandle} | null>}
 */
async function tryOpenOpfsCacheDir() {
  if (!navigator.storage || typeof navigator.storage.getDirectory !== "function") return null;
  try {
    const root = await navigator.storage.getDirectory();
    const cacheDir = await root.getDirectoryHandle(DB_NAME, { create: true });
    await cacheDir.getDirectoryHandle("shaders", { create: true });
    await cacheDir.getDirectoryHandle("pipelines", { create: true });
    return { root, cacheDir };
  } catch {
    return null;
  }
}

/**
 * @param {FileSystemDirectoryHandle} dir
 * @param {string} name
 * @returns {Promise<string | null>}
 */
async function readOpfsTextFile(dir, name) {
  try {
    const handle = await dir.getFileHandle(name);
    const file = await handle.getFile();
    return await file.text();
  } catch {
    return null;
  }
}

/**
 * @param {FileSystemDirectoryHandle} dir
 * @param {string} name
 * @param {string} contents
 * @returns {Promise<boolean>}
 */
async function writeOpfsTextFile(dir, name, contents) {
  try {
    const handle = await dir.getFileHandle(name, { create: true });
    // Truncate by default so rewriting an existing key does not append and
    // corrupt the JSON blob (implementation detail varies across browsers).
    const writable =
      (await handle.createWritable({ keepExistingData: false }).catch(() => null)) ?? (await handle.createWritable());
    await writable.write(contents);
    await writable.close();
    return true;
  } catch {
    return false;
  }
}

/**
 * @param {FileSystemDirectoryHandle} dir
 * @param {string} name
 * @returns {Promise<void>}
 */
async function deleteOpfsFile(dir, name) {
  try {
    await dir.removeEntry(name);
  } catch {
    // Ignore.
  }
}

/**
 * @param {string} wgsl
 * @param {any} reflection
 * @returns {number}
 */
function approxShaderBytes(wgsl, reflection) {
  const enc = new TextEncoder();
  // Approximate: WGSL bytes + reflection JSON bytes.
  return enc.encode(wgsl).byteLength + enc.encode(stableStringify(reflection)).byteLength;
}

/**
 * @param {any} pipelineDesc
 * @returns {number}
 */
function approxPipelineBytes(pipelineDesc) {
  const enc = new TextEncoder();
  return enc.encode(stableStringify(pipelineDesc)).byteLength;
}

/**
 * @typedef {Object} PersistentGpuCacheOptions
 * @property {{maxEntries: number, maxBytes: number}} [shaderLimits]
 * @property {{maxEntries: number, maxBytes: number}} [pipelineLimits]
 */

export class PersistentGpuCache {
  /**
   * @param {IDBDatabase} db
   * @param {FileSystemDirectoryHandle | null} opfsCacheDir
   * @param {Required<PersistentGpuCacheOptions>} options
   */
  constructor(db, opfsCacheDir, options) {
    this._db = db;
    this._opfsCacheDir = opfsCacheDir;
    this._shaderLimits = options.shaderLimits;
    this._pipelineLimits = options.pipelineLimits;

    this._shaderBytes = 0;
    this._pipelineBytes = 0;

    // Warmed pipeline descriptor cache (key -> desc)
    this.pipelineDescriptors = new Map();

    this._telemetry = {
      shader: {
        hits: 0,
        misses: 0,
        bytesRead: 0,
        bytesWritten: 0,
        evictions: 0,
        evictedBytes: 0,
      },
      pipeline: {
        hits: 0,
        misses: 0,
        bytesRead: 0,
        bytesWritten: 0,
        evictions: 0,
        evictedBytes: 0,
      },
    };
  }

  /**
   * @param {PersistentGpuCacheOptions} [options]
   * @returns {Promise<PersistentGpuCache>}
   */
  static async open(options = {}) {
    let didUpgrade = false;
    const db = await new Promise((resolve, reject) => {
      const req = indexedDB.open(DB_NAME, DB_VERSION);
      req.onupgradeneeded = () => {
        didUpgrade = true;
        const db = req.result;

        // Drop & recreate stores on version bump to guarantee a clean schema.
        // This is intentionally conservative: shader caches are derivable.
        for (const name of Array.from(db.objectStoreNames)) {
          db.deleteObjectStore(name);
        }

        const shaders = db.createObjectStore(STORE_SHADERS, { keyPath: "key" });
        shaders.createIndex("lastUsed", "lastUsed", { unique: false });

        const pipelines = db.createObjectStore(STORE_PIPELINES, { keyPath: "key" });
        pipelines.createIndex("lastUsed", "lastUsed", { unique: false });
      };
      req.onerror = () => reject(req.error ?? new Error("Failed to open IndexedDB"));
      req.onsuccess = () => resolve(req.result);
    });

    let opfs = await tryOpenOpfsCacheDir();
    if (didUpgrade && opfs) {
      // If we dropped IndexedDB stores due to a schema bump, also clear OPFS
      // payload files; otherwise we can leak orphaned blobs across upgrades.
      //
      // We also clear the legacy OPFS directory name used by earlier revisions.
      for (const dirName of [DB_NAME, "aero_gpu_cache"]) {
        try {
          await opfs.root.removeEntry(dirName, { recursive: true });
        } catch {
          // Ignore.
        }
      }
      opfs = await tryOpenOpfsCacheDir();
    }
    const cache = new PersistentGpuCache(db, opfs ? opfs.cacheDir : null, {
      shaderLimits: options.shaderLimits ?? DEFAULT_LIMITS.shaders,
      pipelineLimits: options.pipelineLimits ?? DEFAULT_LIMITS.pipelines,
    });

    await cache._recomputeByteCounts();
    await cache._evictIfNeeded();
    await cache.warmPipelines();
    return cache;
  }

  /**
   * Remove all persisted GPU cache state for this origin.
   *
   * This deletes:
   * - the IndexedDB database (`DB_NAME`)
   * - the OPFS cache directory (if available)
   *
   * @returns {Promise<void>}
   */
  static async clearAll() {
    await new Promise((resolve, reject) => {
      const req = indexedDB.deleteDatabase(DB_NAME);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error ?? new Error("Failed to delete IndexedDB database"));
      req.onblocked = () => resolve(); // best-effort: treat blocked as "done enough"
    });

    const opfs = await tryOpenOpfsCacheDir();
    if (opfs) {
      for (const dirName of [DB_NAME, "aero_gpu_cache"]) {
        try {
          await opfs.root.removeEntry(dirName, { recursive: true });
        } catch {
          // Ignore.
        }
      }
    }
  }

  async close() {
    this._db.close();
  }

  /**
   * Lightweight introspection helper for overlays/debugging.
   *
   * @returns {Promise<{shaders:{entries:number, bytes:number}, pipelines:{entries:number, bytes:number}, opfs:boolean}>}
   */
  async stats() {
    const [shaderEntries, pipelineEntries] = await Promise.all([
      this._countStore(STORE_SHADERS),
      this._countStore(STORE_PIPELINES),
    ]);
    return {
      shaders: { entries: shaderEntries, bytes: this._shaderBytes },
      pipelines: { entries: pipelineEntries, bytes: this._pipelineBytes },
      opfs: !!this._opfsCacheDir,
    };
  }

  /**
   * Drop all cached entries (IndexedDB + any OPFS blobs) and reset byte counts.
   *
   * @returns {Promise<void>}
   */
  async clearCache() {
    this.pipelineDescriptors.clear();

    // Best-effort OPFS cleanup.
    if (this._opfsCacheDir) {
      try {
        await this._opfsCacheDir.removeEntry("shaders", { recursive: true });
      } catch {
        // Ignore.
      }
      try {
        await this._opfsCacheDir.removeEntry("pipelines", { recursive: true });
      } catch {
        // Ignore.
      }
      // Recreate directories so subsequent writes succeed without re-opening.
      try {
        await this._opfsCacheDir.getDirectoryHandle("shaders", { create: true });
        await this._opfsCacheDir.getDirectoryHandle("pipelines", { create: true });
      } catch {
        // Ignore; we'll fall back to IDB payload storage.
      }
    }

    const tx = this._db.transaction([STORE_SHADERS, STORE_PIPELINES], "readwrite");
    tx.objectStore(STORE_SHADERS).clear();
    tx.objectStore(STORE_PIPELINES).clear();
    await txDone(tx);

    this._shaderBytes = 0;
    this._pipelineBytes = 0;
    this.resetTelemetry();
  }

  /**
   * @returns {{
   *   shader: {hits:number, misses:number, bytesRead:number, bytesWritten:number, evictions:number, evictedBytes:number},
   *   pipeline: {hits:number, misses:number, bytesRead:number, bytesWritten:number, evictions:number, evictedBytes:number},
   * }}
   */
  getTelemetry() {
    // Ensure the return value is structured-cloneable.
    return JSON.parse(JSON.stringify(this._telemetry));
  }

  resetTelemetry() {
    this._telemetry.shader.hits = 0;
    this._telemetry.shader.misses = 0;
    this._telemetry.shader.bytesRead = 0;
    this._telemetry.shader.bytesWritten = 0;
    this._telemetry.shader.evictions = 0;
    this._telemetry.shader.evictedBytes = 0;

    this._telemetry.pipeline.hits = 0;
    this._telemetry.pipeline.misses = 0;
    this._telemetry.pipeline.bytesRead = 0;
    this._telemetry.pipeline.bytesWritten = 0;
    this._telemetry.pipeline.evictions = 0;
    this._telemetry.pipeline.evictedBytes = 0;
  }

  /**
   * Load pipeline descriptors into memory so pipeline creation code can quickly
   * determine if a pipeline has been seen before.
   *
   * @returns {Promise<void>}
   */
  async warmPipelines() {
    this.pipelineDescriptors.clear();
    // IndexedDB transactions cannot be kept alive across `await` points, so we
    // first collect records and then load any OPFS payloads after the transaction
    // completes.
    /** @type {any[]} */
    const records = [];
    {
      const tx = this._db.transaction([STORE_PIPELINES], "readonly");
      const store = tx.objectStore(STORE_PIPELINES);
      await new Promise((resolve, reject) => {
        const req = store.openCursor();
        req.onerror = () => reject(req.error ?? new Error("Failed to iterate pipelines"));
        req.onsuccess = () => {
          const cursor = req.result;
          if (!cursor) {
            resolve();
            return;
          }
          records.push(cursor.value);
          cursor.continue();
        };
      });
      await txDone(tx);
    }

    for (const record of records) {
      const desc = await this._loadPipelinePayload(record);
      if (desc !== null) this.pipelineDescriptors.set(record.key, desc);
    }
  }

  /**
   * @param {string} key
   * @returns {Promise<{wgsl: string, reflection: any} | null>}
   */
  async getShader(key) {
    const tx = this._db.transaction([STORE_SHADERS], "readonly");
    const store = tx.objectStore(STORE_SHADERS);
    /** @type {any} */
    const record = await reqToPromise(store.get(key));
    await txDone(tx);
    if (!record) {
      this._telemetry.shader.misses += 1;
      return null;
    }

    const payload = await this._loadShaderPayload(record);
    if (payload === null) {
      // Stale record (e.g. OPFS file removed). Treat as miss and delete metadata.
      await this.deleteShader(key);
      this._telemetry.shader.misses += 1;
      return null;
    }

    this._telemetry.shader.hits += 1;
    this._telemetry.shader.bytesRead += record.size ?? approxShaderBytes(payload.wgsl, payload.reflection);

    // Touch LRU (best-effort).
    await this._touchShader(key, record);
    return payload;
  }

  /**
   * @param {string} key
   * @param {{wgsl: string, reflection: any}} value
   * @returns {Promise<void>}
   */
  async putShader(key, value) {
    const now = Date.now();
    const size = approxShaderBytes(value.wgsl, value.reflection);
    const payload = { wgsl: value.wgsl, reflection: value.reflection };

    let storage = "idb";
    let opfsFile = null;
    if (this._opfsCacheDir && size > OPFS_MIN_BYTES) {
      const shadersDir = await this._opfsCacheDir.getDirectoryHandle("shaders");
      const name = `${key}.json`;
      const ok = await writeOpfsTextFile(shadersDir, name, JSON.stringify(payload));
      if (ok) {
        storage = "opfs";
        opfsFile = name;
      }
    }

    const tx = this._db.transaction([STORE_SHADERS], "readwrite");
    const store = tx.objectStore(STORE_SHADERS);
    const existing = await reqToPromise(store.get(key));
    if (existing) this._shaderBytes -= existing.size ?? 0;

    /** @type {any} */
    const record = {
      key,
      storage,
      opfsFile,
      wgsl: storage === "idb" ? value.wgsl : undefined,
      reflection: storage === "idb" ? value.reflection : undefined,
      size,
      createdAt: existing?.createdAt ?? now,
      lastUsed: now,
    };

    store.put(record);
    await txDone(tx);

    this._shaderBytes += size;
    this._telemetry.shader.bytesWritten += size;
    await this._evictIfNeeded();
  }

  /**
   * @param {string} key
   * @returns {Promise<void>}
   */
  async deleteShader(key) {
    const tx = this._db.transaction([STORE_SHADERS], "readwrite");
    const store = tx.objectStore(STORE_SHADERS);
    const existing = await reqToPromise(store.get(key));
    if (existing) {
      this._shaderBytes -= existing.size ?? 0;
      if (existing.storage === "opfs" && this._opfsCacheDir) {
        const shadersDir = await this._opfsCacheDir.getDirectoryHandle("shaders");
        await deleteOpfsFile(shadersDir, existing.opfsFile);
      }
    }
    store.delete(key);
    await txDone(tx);
  }

  /**
   * @param {string} key
   * @returns {Promise<any | null>}
   */
  async getPipelineDescriptor(key) {
    // Serve from warmed in-memory map when possible.
    if (this.pipelineDescriptors.has(key)) return this.pipelineDescriptors.get(key);

    const tx = this._db.transaction([STORE_PIPELINES], "readonly");
    const store = tx.objectStore(STORE_PIPELINES);
    const record = await reqToPromise(store.get(key));
    await txDone(tx);
    if (!record) {
      this._telemetry.pipeline.misses += 1;
      return null;
    }

    const desc = await this._loadPipelinePayload(record);
    if (desc === null) {
      await this.deletePipelineDescriptor(key);
      this._telemetry.pipeline.misses += 1;
      return null;
    }

    this._telemetry.pipeline.hits += 1;
    this._telemetry.pipeline.bytesRead += record.size ?? approxPipelineBytes(desc);

    await this._touchPipeline(key, record);
    this.pipelineDescriptors.set(key, desc);
    return desc;
  }

  /**
   * @param {string} key
   * @param {any} pipelineDesc
   * @returns {Promise<void>}
   */
  async putPipelineDescriptor(key, pipelineDesc) {
    const now = Date.now();
    const size = approxPipelineBytes(pipelineDesc);
    const payload = pipelineDesc;

    let storage = "idb";
    let opfsFile = null;
    if (this._opfsCacheDir && size > OPFS_MIN_BYTES) {
      const dir = await this._opfsCacheDir.getDirectoryHandle("pipelines");
      const name = `${key}.json`;
      const ok = await writeOpfsTextFile(dir, name, JSON.stringify(payload));
      if (ok) {
        storage = "opfs";
        opfsFile = name;
      }
    }

    const tx = this._db.transaction([STORE_PIPELINES], "readwrite");
    const store = tx.objectStore(STORE_PIPELINES);
    const existing = await reqToPromise(store.get(key));
    if (existing) this._pipelineBytes -= existing.size ?? 0;

    /** @type {any} */
    const record = {
      key,
      storage,
      opfsFile,
      desc: storage === "idb" ? payload : undefined,
      size,
      createdAt: existing?.createdAt ?? now,
      lastUsed: now,
    };

    store.put(record);
    await txDone(tx);

    this._pipelineBytes += size;
    this._telemetry.pipeline.bytesWritten += size;
    this.pipelineDescriptors.set(key, pipelineDesc);
    await this._evictIfNeeded();
  }

  /**
   * @param {string} key
   * @returns {Promise<void>}
   */
  async deletePipelineDescriptor(key) {
    this.pipelineDescriptors.delete(key);

    const tx = this._db.transaction([STORE_PIPELINES], "readwrite");
    const store = tx.objectStore(STORE_PIPELINES);
    const existing = await reqToPromise(store.get(key));
    if (existing) {
      this._pipelineBytes -= existing.size ?? 0;
      if (existing.storage === "opfs" && this._opfsCacheDir) {
        const dir = await this._opfsCacheDir.getDirectoryHandle("pipelines");
        await deleteOpfsFile(dir, existing.opfsFile);
      }
    }
    store.delete(key);
    await txDone(tx);
  }

  /**
   * @returns {Promise<void>}
   */
  async _recomputeByteCounts() {
    this._shaderBytes = 0;
    this._pipelineBytes = 0;
    await Promise.all([this._sumStoreBytes(STORE_SHADERS), this._sumStoreBytes(STORE_PIPELINES)]);
  }

  /**
   * @param {string} storeName
   * @returns {Promise<number>}
   */
  async _countStore(storeName) {
    const tx = this._db.transaction([storeName], "readonly");
    const store = tx.objectStore(storeName);
    const count = await reqToPromise(store.count());
    await txDone(tx);
    return count ?? 0;
  }

  /**
   * @param {string} storeName
   * @returns {Promise<void>}
   */
  async _sumStoreBytes(storeName) {
    const tx = this._db.transaction([storeName], "readonly");
    const store = tx.objectStore(storeName);
    await new Promise((resolve, reject) => {
      const req = store.openCursor();
      req.onerror = () => reject(req.error ?? new Error(`Failed to iterate ${storeName}`));
      req.onsuccess = () => {
        const cursor = req.result;
        if (!cursor) {
          resolve();
          return;
        }
        const size = cursor.value?.size ?? 0;
        if (storeName === STORE_SHADERS) this._shaderBytes += size;
        else this._pipelineBytes += size;
        cursor.continue();
      };
    });
    await txDone(tx);
  }

  /**
   * @returns {Promise<void>}
   */
  async _evictIfNeeded() {
    await this._evictStoreIfNeeded(STORE_SHADERS, this._shaderLimits, async (record) => {
      await this.deleteShader(record.key);
    });
    await this._evictStoreIfNeeded(STORE_PIPELINES, this._pipelineLimits, async (record) => {
      await this.deletePipelineDescriptor(record.key);
    });
  }

  /**
   * @param {string} storeName
   * @param {{maxEntries:number,maxBytes:number}} limits
   * @param {(record:any)=>Promise<void>} deleteFn
   * @returns {Promise<void>}
   */
  async _evictStoreIfNeeded(storeName, limits, deleteFn) {
    const bytesRef = storeName === STORE_SHADERS ? () => this._shaderBytes : () => this._pipelineBytes;
    const setBytes = storeName === STORE_SHADERS ? (v) => (this._shaderBytes = v) : (v) => (this._pipelineBytes = v);

    while (true) {
      const tx = this._db.transaction([storeName], "readonly");
      const store = tx.objectStore(storeName);
      const count = await reqToPromise(store.count());
      await txDone(tx);

      if (count <= limits.maxEntries && bytesRef() <= limits.maxBytes) return;

      // Delete the least-recently-used entry (smallest lastUsed).
      const tx2 = this._db.transaction([storeName], "readonly");
      const store2 = tx2.objectStore(storeName);
      const idx = store2.index("lastUsed");
      const oldest = await new Promise((resolve, reject) => {
        const req = idx.openCursor();
        req.onerror = () => reject(req.error ?? new Error(`Failed to open ${storeName} lastUsed cursor`));
        req.onsuccess = () => {
          const cursor = req.result;
          resolve(cursor ? cursor.value : null);
        };
      });
      await txDone(tx2);

      if (!oldest) return;

      const oldestSize = oldest.size ?? 0;
      if (storeName === STORE_SHADERS) {
        this._telemetry.shader.evictions += 1;
        this._telemetry.shader.evictedBytes += oldestSize;
      } else {
        this._telemetry.pipeline.evictions += 1;
        this._telemetry.pipeline.evictedBytes += oldestSize;
      }

      // Deleting updates byte counts inside deleteFn.
      await deleteFn(oldest);

      // deleteFn already adjusted bytes, but if multiple contexts are writing we can go negative.
      setBytes(Math.max(0, bytesRef()));
    }
  }

  /**
   * @param {string} key
   * @param {any} record
   * @returns {Promise<void>}
   */
  async _touchShader(key, record) {
    const now = Date.now();
    if ((record.lastUsed ?? 0) >= now - 1000) return; // avoid churn on same-tick re-reads
    const tx = this._db.transaction([STORE_SHADERS], "readwrite");
    const store = tx.objectStore(STORE_SHADERS);
    const fresh = await reqToPromise(store.get(key));
    if (fresh) {
      fresh.lastUsed = now;
      store.put(fresh);
    }
    await txDone(tx);
  }

  /**
   * @param {string} key
   * @param {any} record
   * @returns {Promise<void>}
   */
  async _touchPipeline(key, record) {
    const now = Date.now();
    if ((record.lastUsed ?? 0) >= now - 1000) return;
    const tx = this._db.transaction([STORE_PIPELINES], "readwrite");
    const store = tx.objectStore(STORE_PIPELINES);
    const fresh = await reqToPromise(store.get(key));
    if (fresh) {
      fresh.lastUsed = now;
      store.put(fresh);
    }
    await txDone(tx);
  }

  /**
   * @param {any} record
   * @returns {Promise<{wgsl: string, reflection: any} | null>}
   */
  async _loadShaderPayload(record) {
    if (record.storage === "opfs") {
      if (!this._opfsCacheDir) return null;
      const dir = await this._opfsCacheDir.getDirectoryHandle("shaders");
      const text = await readOpfsTextFile(dir, record.opfsFile);
      if (text === null) return null;
      try {
        const parsed = JSON.parse(text);
        if (!parsed || typeof parsed.wgsl !== "string") return null;
        return { wgsl: parsed.wgsl, reflection: parsed.reflection };
      } catch {
        return null;
      }
    }
    if (typeof record.wgsl !== "string") return null;
    return { wgsl: record.wgsl, reflection: record.reflection };
  }

  /**
   * @param {any} record
   * @returns {Promise<any | null>}
   */
  async _loadPipelinePayload(record) {
    if (record.storage === "opfs") {
      if (!this._opfsCacheDir) return null;
      const dir = await this._opfsCacheDir.getDirectoryHandle("pipelines");
      const text = await readOpfsTextFile(dir, record.opfsFile);
      if (text === null) return null;
      try {
        return JSON.parse(text);
      } catch {
        return null;
      }
    }
    return record.desc ?? null;
  }
}

/**
 * Shader translation cache that combines:
 * - a persistent store (`PersistentGpuCache`) for cross-session reuse, and
 * - an in-memory map for the current session.
 *
 * This is a higher-level helper intended for "DXBC -> WGSL + reflection"
 * translation caching. It intentionally does not attempt to persist compiled
 * GPU objects like `GPUShaderModule` or pipelines.
 */
export class ShaderTranslationCache {
  /**
   * @param {PersistentGpuCache} persistent
   */
  constructor(persistent) {
    this._persistent = persistent;
    /** @type {Map<string, {wgsl: string, reflection: any}>} */
    this._memory = new Map();
  }

  /**
   * Clear the in-memory (session) cache.
   */
  clearMemory() {
    this._memory.clear();
  }

  /**
   * Get a shader translation artifact, using the lookup order required by Aero:
   *
   * 1) persistent cache (cross-session)
   * 2) in-memory cache (this session)
   * 3) translate (slow) + persist
   *
   * On persistent hit, the caller may provide a validator (typically Naga) to
   * defensively verify cached data before use. If validation fails, the cache
   * entry is deleted and the shader is retranslated.
   *
   * @param {ArrayBuffer|Uint8Array} dxbc
   * @param {{halfPixelCenter?: boolean, capsHash?: string|null, [k: string]: any}} flags
   * @param {() => Promise<{wgsl: string, reflection: any}>} translateFn
   * @param {{ validateWgsl?: (wgsl: string) => Promise<boolean> }} [opts]
   * @returns {Promise<{key: string, value: {wgsl: string, reflection: any}, source: "persistent"|"memory"|"translated"}>}
   */
  async getOrTranslate(dxbc, flags, translateFn, opts = {}) {
    const key = await computeShaderCacheKey(dxbc, flags);

    const persistent = await this._persistent.getShader(key);
    if (persistent) {
      if (opts.validateWgsl) {
        const ok = await opts.validateWgsl(persistent.wgsl);
        if (!ok) {
          // Corruption defense: drop entry and retranslate.
          await this._persistent.deleteShader(key);
        } else {
          this._memory.set(key, persistent);
          return { key, value: persistent, source: "persistent" };
        }
      } else {
        this._memory.set(key, persistent);
        return { key, value: persistent, source: "persistent" };
      }
    }

    const mem = this._memory.get(key);
    if (mem) {
      return { key, value: mem, source: "memory" };
    }

    const translated = await translateFn();
    this._memory.set(key, translated);
    await this._persistent.putShader(key, translated);
    return { key, value: translated, source: "translated" };
  }
}

/**
 * Compile a WGSL shader module and validate it if the browser supports
 * `GPUShaderModule.getCompilationInfo()`.
 *
 * @param {GPUDevice} device
 * @param {string} wgsl
 * @returns {Promise<{module: GPUShaderModule, ok: boolean, messages?: any[]}>}
 */
export async function compileWgslModule(device, wgsl) {
  const module = device.createShaderModule({ code: wgsl });
  // `getCompilationInfo` is the only standardized way to check errors without
  // forcing a pipeline creation.
  if (typeof module.getCompilationInfo === "function") {
    try {
      const info = await module.getCompilationInfo();
      const messages = info.messages ?? [];
      const ok = messages.every((m) => m.type !== "error");
      return { module, ok, messages };
    } catch {
      // If compilation info fails, assume ok; errors will surface later.
      return { module, ok: true };
    }
  }
  return { module, ok: true };
}

// Optional global registration to make it easy for benchmark pages and ad-hoc
// debugging to access the implementation without a bundler.
if (typeof globalThis !== "undefined") {
  const g = /** @type {any} */ (globalThis);
  if (!g.AeroPersistentGpuCache) {
    g.AeroPersistentGpuCache = {
      PersistentGpuCache,
      computeShaderCacheKey,
      computePipelineCacheKey,
      computeWebGpuCapsHash,
      compileWgslModule,
      sha256Hex,
    };
  } else {
    g.AeroPersistentGpuCache.PersistentGpuCache ??= PersistentGpuCache;
    g.AeroPersistentGpuCache.computeShaderCacheKey ??= computeShaderCacheKey;
    g.AeroPersistentGpuCache.computePipelineCacheKey ??= computePipelineCacheKey;
    g.AeroPersistentGpuCache.computeWebGpuCapsHash ??= computeWebGpuCapsHash;
    g.AeroPersistentGpuCache.compileWgslModule ??= compileWgslModule;
    g.AeroPersistentGpuCache.sha256Hex ??= sha256Hex;
  }
}

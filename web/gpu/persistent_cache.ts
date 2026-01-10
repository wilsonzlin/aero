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
// - Cache keys are SHA-256 of DXBC bytecode + translation flags + caps hash + key version.
//
// This file intentionally avoids TypeScript-only syntax so it can be served directly
// as an ES module in tests without a build step.

const DB_NAME = "aero_gpu_cache";
const DB_VERSION = 1;

const STORE_SHADERS = "shaders";
const STORE_PIPELINES = "pipelines";

// Bump this whenever the shader key derivation / persisted value schema changes.
// This is included in the cache key preimage so old entries become unreachable.
const SHADER_CACHE_KEY_VERSION = 1;
const PIPELINE_CACHE_KEY_VERSION = 1;

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
 * Compute a strong shader cache key.
 *
 * @param {ArrayBuffer|Uint8Array} dxbc
 * @param {{halfPixelCenter?: boolean, capsHash: string, [k: string]: any}} flags
 * @returns {Promise<string>}
 */
export async function computeShaderCacheKey(dxbc, flags) {
  const enc = new TextEncoder();
  const canonicalFlags = { ...flags, halfPixelCenter: !!flags.halfPixelCenter };
  const metaBytes = enc.encode(
    stableStringify({
      v: SHADER_CACHE_KEY_VERSION,
      flags: canonicalFlags,
    }),
  );
  const preimage = concatBytes([toU8(dxbc), metaBytes]);
  return await sha256Hex(preimage);
}

/**
 * Compute a stable key for pipeline descriptors.
 *
 * @param {any} pipelineDesc
 * @returns {Promise<string>}
 */
export async function computePipelineCacheKey(pipelineDesc) {
  const enc = new TextEncoder();
  const metaBytes = enc.encode(
    stableStringify({
      v: PIPELINE_CACHE_KEY_VERSION,
      desc: pipelineDesc,
    }),
  );
  return await sha256Hex(metaBytes);
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
    const cacheDir = await root.getDirectoryHandle("aero_gpu_cache", { create: true });
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
    const writable = await handle.createWritable();
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
      try {
        await opfs.root.removeEntry("aero_gpu_cache", { recursive: true });
      } catch {
        // Ignore.
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
    if (!record) return null;

    const payload = await this._loadShaderPayload(record);
    if (payload === null) {
      // Stale record (e.g. OPFS file removed). Treat as miss and delete metadata.
      await this.deleteShader(key);
      return null;
    }

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
    if (this._opfsCacheDir) {
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
    if (!record) return null;

    const desc = await this._loadPipelinePayload(record);
    if (desc === null) {
      await this.deletePipelineDescriptor(key);
      return null;
    }

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
    if (this._opfsCacheDir) {
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

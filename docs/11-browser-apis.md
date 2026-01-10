# 11 - Browser APIs & Web Platform Integration

## Overview

Aero leverages cutting-edge browser APIs to achieve performance and functionality. This document details all web platform features used and their browser compatibility.

---

## API Dependency Matrix

| API | Chrome | Firefox | Safari | Edge | Required? |
|-----|--------|---------|--------|------|-----------|
| WebAssembly | 57+ | 52+ | 11+ | 16+ | **Yes** |
| WASM SIMD | 91+ | 89+ | 16.4+ | 91+ | **Yes** |
| WASM Threads | 74+ | 79+ | 14.1+ | 79+ | **Yes** |
| SharedArrayBuffer | 68+ | 79+ | 15.2+ | 79+ | **Yes** |
| WebGPU | 113+ | 🚧 | 🚧 | 113+ | **Preferred** |
| WebGL2 | ✓ | ✓ | ✓ | ✓ | Fallback |
| OPFS | 102+ | 111+ | 15.2+ | 102+ | **Yes** |
| Web Workers | ✓ | ✓ | ✓ | ✓ | **Yes** |
| AudioWorklet | 66+ | 76+ | 14.1+ | 79+ | **Yes** |
| WebSocket | ✓ | ✓ | ✓ | ✓ | **Yes** |
| WebRTC | ✓ | ✓ | 11+ | ✓ | Optional |
| Pointer Lock | ✓ | ✓ | 10.1+ | ✓ | **Yes** |
| Fullscreen | ✓ | ✓ | ✓ | ✓ | Recommended |
| Gamepad | ✓ | ✓ | 10.1+ | ✓ | Optional |
| WebCodecs | 94+ | 🚧 | 🚧 | 94+ | Optional |

---

## Cross-Origin Isolation (COOP/COEP) Deployment Requirements

Modern browsers gate **WebAssembly threads** and **SharedArrayBuffer** behind **cross-origin isolation** (a Spectre mitigation). In practice:

- `globalThis.crossOriginIsolated` must be `true`
- `SharedArrayBuffer` must be defined
- `Atomics` must be available

Cross-origin isolation is enabled by two HTTP response headers on the **top-level document**:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Many deployments apply these headers to **all app-owned responses** (HTML + JS + worker scripts + `.wasm`) because it simplifies CDN/proxy configuration and avoids accidentally serving a document without the required headers.

> `Cross-Origin-Opener-Policy` only affects documents, but it is harmless to set it on static assets. `Cross-Origin-Embedder-Policy` is what enforces the “no non-opted-in cross-origin subresources” rule that can break `crossOriginIsolated`.

These must be delivered as **HTTP response headers** (not `<meta http-equiv>`).

### Optional: `Cross-Origin-Embedder-Policy: credentialless`

Some deployments can use:

```
Cross-Origin-Embedder-Policy: credentialless
```

Trade-offs:

- **Pros:** can reduce friction with some cross-origin resources by ensuring cross-origin requests are made without credentials by default.
- **Cons:** any cross-origin subresource that relies on cookies/credentials can break; browser support is not as universal as `require-corp` (especially outside Chromium-based browsers).

For maximum compatibility, prefer `require-corp` unless you have a specific reason to switch.

### What breaks `crossOriginIsolated`

With `Cross-Origin-Embedder-Policy: require-corp`, any **cross-origin** subresource must opt-in to being embedded. If you load a cross-origin script/worker/WASM/fetch that is **not CORS-enabled** *and* does **not** send an appropriate `Cross-Origin-Resource-Policy` header, the browser will block it as a COEP violation.

### Production deployment examples (set headers on all assets)

**Nginx** (apply to HTML + JS/worker/WASM responses):

```nginx
server {
  # ... your listen/server_name/etc ...

  add_header Cross-Origin-Opener-Policy "same-origin" always;
  add_header Cross-Origin-Embedder-Policy "require-corp" always;

  location / {
    try_files $uri $uri/ /index.html;
  }
}
```

**Static hosting** (Cloudflare Pages / Netlify-style `_headers` file):

```text
/*
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp
```

### Vite setup (dev + preview)

Vite requires configuring both the dev server and the preview server:

- `server.headers` → `npm run dev`
- `preview.headers` → `npm run preview` / `vite preview` (serving `dist/`)

See `vite.config.ts` in the repo for the canonical header values.

For production hosting templates (Netlify / Cloudflare Pages) and caching defaults, see
[`docs/deployment.md`](./deployment.md).

### Debugging when `SharedArrayBuffer` is undefined

If `typeof SharedArrayBuffer === 'undefined'` (or `crossOriginIsolated === false`):

1. **Verify the response headers** on the main HTML document in DevTools → Network → (document) → Response Headers.
2. **Check you are in a secure context** (`https://` or `http://localhost`). Non-localhost `http://` will not expose `SharedArrayBuffer`.
3. **Look for COEP/CORS errors** in the console. With `Cross-Origin-Embedder-Policy: require-corp`, any cross-origin subresource (WASM, scripts, images, fonts) must be served with CORS headers or a compatible `Cross-Origin-Resource-Policy` header. Prefer serving assets from the same origin.

### Disk image streaming implications (remote disk bytes are a subresource)

Disk image streaming is implemented via `fetch()` (often with HTTP `Range` requests). Under COEP it is treated like any other subresource:

- **Strong recommendation:** serve disk bytes from the **same origin** as the app (for example, reverse-proxy the streaming service under `/disk/...`). This avoids CORS/CORP edge cases and keeps cross-origin isolation stable.
- If disk bytes are fetched from a **different origin**, the streaming endpoint must satisfy COEP via **CORS and/or CORP**:
  - **CORS:** set `Access-Control-Allow-Origin` (and `Access-Control-Allow-Credentials: true` if you use cookies/HTTP auth), matching how the client fetches the resource.
    - For `fetch()`-based streaming where JS needs to read the response body, **CORS is effectively required**; `Cross-Origin-Resource-Policy` alone is not sufficient because `no-cors` fetches are opaque.
  - **CORP:** set `Cross-Origin-Resource-Policy: same-site` (same eTLD+1) or `Cross-Origin-Resource-Policy: cross-origin` (intended to be embeddable by arbitrary sites).
- **Range requests may trigger preflight:** `Range` is not a CORS-safelisted request header, so browsers will send an `OPTIONS` preflight. Ensure the service allows the headers you use (commonly `Range, Authorization`) and exposes `Content-Range` so JS can read it.
- **Do not transform/compress disk bytes:** byte offsets must match the on-disk stream. If you use a CDN/reverse proxy, disable gzip/auto-compression for the disk route.

**Quick header checklist for a cross-origin image host (example):**

_Preflight (`OPTIONS`) response_:

```http
HTTP/1.1 204 No Content
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, Authorization
Access-Control-Max-Age: 600
Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers
```

_Disk byte response (`GET`/`HEAD`, including `206 Partial Content`)_:

```http
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag
Cross-Origin-Resource-Policy: same-site
Vary: Origin
Accept-Ranges: bytes
Cache-Control: no-transform
```

If you use cookies/other credentials, add `Access-Control-Allow-Credentials: true` and do not use `Access-Control-Allow-Origin: *`.

For the detailed header matrix and troubleshooting guidance, see:

- [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md)
- [Disk Image Streaming Service (Runbook)](./backend/disk-image-streaming-service.md)
- [05 - Storage Subsystem: Remote HTTP server requirements](./05-storage-subsystem.md#remote-http-server-requirements-rangecorsno-transform)

---

## WebAssembly

### Core WASM Features

```javascript
// Feature detection
const wasmFeatures = {
    simd: WebAssembly.validate(new Uint8Array([
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7b,
        0x03, 0x02, 0x01, 0x00, 0x0a, 0x0a, 0x01,
        0x08, 0x00, 0x41, 0x00, 0xfd, 0x0c, 0x00, 0x00, 0x0b
    ])),
    
    threads: typeof SharedArrayBuffer !== 'undefined',
    
    bulkMemory: WebAssembly.validate(new Uint8Array([
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
        0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01,
        0x0a, 0x0a, 0x01, 0x08, 0x00, 0x41, 0x00, 0x41, 0x00, 0x41, 0x00, 0xfc, 0x0a, 0x00, 0x00, 0x0b
    ])),
    
    tailCall: WebAssembly.validate(new Uint8Array([
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
        0x03, 0x02, 0x01, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x12, 0x00, 0x0b
    ])),
};
```

### Memory Management

```javascript
// Guest RAM is configurable and must fit within wasm32 + browser limits.
// In practice, shared WebAssembly.Memory often tops out below 4GiB.
const GUEST_RAM_MIB = 1024; // 512 / 1024 / 2048 / 3072 (best-effort)
const GUEST_RAM_BYTES = GUEST_RAM_MIB * 1024 * 1024;
const WASM_PAGE_BYTES = 64 * 1024;
const RING_CTRL_BYTES = 16;      // Int32Array[4] header (see docs/ipc-protocol.md)
const CMD_CAP_BYTES = 1 << 20;   // 1 MiB ring data region
const EVT_CAP_BYTES = 1 << 20;   // 1 MiB ring data region

async function initializeMemory() {
    if (!crossOriginIsolated || typeof SharedArrayBuffer === 'undefined') {
        throw new Error('SharedArrayBuffer requires COOP/COEP (cross-origin isolated page)');
    }

    // Shared guest RAM (wasm32).
    const guestPages = Math.ceil(GUEST_RAM_BYTES / WASM_PAGE_BYTES);
    const guestMemory = new WebAssembly.Memory({
        initial: guestPages,
        maximum: guestPages,
        shared: true,
    });

    // Separate small SABs for state + command/event rings (no >4GiB offsets).
    const stateSab = new SharedArrayBuffer(64 * 1024);
    const cmdSab = new SharedArrayBuffer(RING_CTRL_BYTES + CMD_CAP_BYTES);
    const eventSab = new SharedArrayBuffer(RING_CTRL_BYTES + EVT_CAP_BYTES);

    // Initialize ring headers: [head, tail_reserve, tail_commit, capacity_bytes]
    new Int32Array(cmdSab, 0, 4).set([0, 0, 0, CMD_CAP_BYTES]);
    new Int32Array(eventSab, 0, 4).set([0, 0, 0, EVT_CAP_BYTES]);
    
    // Create views
    const views = {
        guestU8: new Uint8Array(guestMemory.buffer),
        guestU32: new Uint32Array(guestMemory.buffer),
        stateI32: new Int32Array(stateSab),
        cmdCtrl: new Int32Array(cmdSab, 0, 4),
        evtCtrl: new Int32Array(eventSab, 0, 4),
        cmdData: new Uint8Array(cmdSab, RING_CTRL_BYTES, CMD_CAP_BYTES),
        evtData: new Uint8Array(eventSab, RING_CTRL_BYTES, EVT_CAP_BYTES),
    };
    
    return { guestMemory, stateSab, cmdSab, eventSab, views };
}
```

### WASM Module Compilation

```javascript
// Streaming compilation for large modules
async function loadEmulatorModule(url) {
    const response = await fetch(url);
    
    // Use streaming compilation for better performance
    const module = await WebAssembly.compileStreaming(response);
    
    // Cache compiled module
    await caches.open('aero-wasm-cache').then(cache => {
        cache.put(url, response.clone());
    });
    
    return module;
}

// Dynamic module generation for JIT
function compileJitBlock(wasmBytes) {
    return WebAssembly.compile(wasmBytes);
}
```

---

## WebGPU

WebGPU is the preferred graphics API for Aero. The current implementation approach is to use **Rust `wgpu` (WASM)** targeting the browser’s WebGPU backend when available, with a **WebGL2 fallback** when WebGPU is unavailable or insufficient.

For the concrete backend design (selection algorithm, OffscreenCanvas worker model, capability matrix, fallback limitations), see:

- [16 - Browser GPU Backends (WebGPU-first + WebGL2 Fallback)](./16-browser-gpu-backends.md)

### Required vs Optional WebGPU Features

**Required (to run in WebGPU mode):**

- Baseline WebGPU support (`navigator.gpu`, render pipelines, texture upload, canvas presentation)
- Sufficient limits for the configured guest display size (e.g. `maxTextureDimension2D`)

**Optional (enabled opportunistically with fallbacks):**

- `texture-compression-bc` for BCn/DXT textures (otherwise decompress on CPU)
- `float32-filterable` for higher-quality/precision sampling paths
- `timestamp-query` for profiling builds and benchmark instrumentation

### Device Initialization

```javascript
async function initializeWebGPU({ enableGpuTiming = false } = {}) {
    if (!navigator.gpu) {
        throw new Error('WebGPU not supported');
    }
    
    const adapter = await navigator.gpu.requestAdapter({
        powerPreference: 'high-performance'
    });
    
    if (!adapter) {
        throw new Error('No GPU adapter found');
    }

    // Timestamp queries are optional and may be unavailable in some browsers
    // and in headless/CI environments. Only request them when explicitly
    // enabled, otherwise keep GPU timing disabled and export `gpu_time_ms: null`.
    const supportsTimestampQuery = adapter.features.has('timestamp-query');
    const gpuTimingEnabled = enableGpuTiming && supportsTimestampQuery;
    
    // Aero should keep true requirements minimal and negotiate optional features.
    // Requiring rarely-supported features causes unnecessary init failures.
    const REQUIRED_LIMITS = {
        // Example baseline requirement: enough for 1080p+ framebuffers.
        // The actual requirement should be derived from the current guest display mode.
        maxTextureDimension2D: 2048,
    };
    
    if (adapter.limits.maxTextureDimension2D < REQUIRED_LIMITS.maxTextureDimension2D) {
        throw new Error('GPU limits too low for Aero framebuffer requirements');
    }
    
    // Optional features: use if available, otherwise fall back to CPU paths / simpler shaders.
    // Only request timestamp queries when explicitly enabled (profiling/bench builds).
    const OPTIONAL_FEATURES = [
        'texture-compression-bc', // Use BCn/DXT directly when available; otherwise decompress on CPU.
        'float32-filterable',     // Quality/perf improvement for some HDR/linear paths.
        ...(gpuTimingEnabled ? ['timestamp-query'] : []),
    ];
    
    const enabledFeatures = OPTIONAL_FEATURES.filter(f => adapter.features.has(f));
    
    const device = await adapter.requestDevice({
        requiredFeatures: enabledFeatures,
        requiredLimits: REQUIRED_LIMITS,
    });
    
    return {
        adapter,
        device,
        gpuTiming: {
            supported: supportsTimestampQuery,
            enabled: gpuTimingEnabled,
        },
    };
}
```

### GPU timestamp queries (optional)

When `timestamp-query` is supported and enabled, the renderer can measure **GPU execution time** using a `GPUQuerySet`:

1. Write a timestamp at the start/end of a frame (or per major render pass).
2. Resolve the query set into a buffer.
3. Read the buffer back asynchronously on a later frame to avoid stalling.

If `timestamp-query` is **unsupported**, all other graphics telemetry should still work; GPU timing fields should be exported as `null` and the perf HUD should display an `n/a` indicator.

### Render Pipeline

```javascript
function createRenderPipeline(device, shaderCode) {
    const shaderModule = device.createShaderModule({
        code: shaderCode
    });
    
    return device.createRenderPipeline({
        layout: 'auto',
        vertex: {
            module: shaderModule,
            entryPoint: 'vs_main',
            buffers: [{
                arrayStride: 32,
                attributes: [
                    { shaderLocation: 0, offset: 0, format: 'float32x3' },   // position
                    { shaderLocation: 1, offset: 12, format: 'float32x2' },  // texcoord
                    { shaderLocation: 2, offset: 20, format: 'float32x3' },  // normal
                ]
            }]
        },
        fragment: {
            module: shaderModule,
            entryPoint: 'fs_main',
            targets: [{
                format: navigator.gpu.getPreferredCanvasFormat(),
                blend: {
                    color: {
                        srcFactor: 'src-alpha',
                        dstFactor: 'one-minus-src-alpha',
                        operation: 'add',
                    },
                    alpha: {
                        srcFactor: 'one',
                        dstFactor: 'one-minus-src-alpha',
                        operation: 'add',
                    }
                }
            }]
        },
        primitive: {
            topology: 'triangle-list',
            cullMode: 'back',
            frontFace: 'ccw',
        },
        depthStencil: {
            format: 'depth24plus-stencil8',
            depthWriteEnabled: true,
            depthCompare: 'less',
        },
        multisample: {
            count: 4,
        }
    });
}
```

### Compute Shaders for GPU Acceleration

Compute is only available in WebGPU mode; the WebGL2 fallback must use CPU implementations for compute-based accelerations.

```wgsl
// Compute shader for parallel texture decompression
@group(0) @binding(0) var<storage, read> compressed_data: array<u32>;
@group(0) @binding(1) var<storage, read_write> decompressed_data: array<u32>;
@group(0) @binding(2) var<uniform> params: DecompressParams;

struct DecompressParams {
    width: u32,
    height: u32,
    format: u32,
}

@compute @workgroup_size(8, 8)
fn decompress_bc1(@builtin(global_invocation_id) id: vec3<u32>) {
    let block_x = id.x;
    let block_y = id.y;
    
    if (block_x >= params.width / 4 || block_y >= params.height / 4) {
        return;
    }
    
    // Read compressed block (64 bits)
    let block_idx = block_y * (params.width / 4) + block_x;
    let color0_raw = compressed_data[block_idx * 2];
    let color1_raw = compressed_data[block_idx * 2 + 1];
    
    // Decompress BC1 block...
    // (Implementation details omitted for brevity)
}
```

### WebGPU → WebGL2 Fallback

For **maximum performance and feature coverage**, Aero prefers WebGPU. However, Aero must still be able to run (with reduced capability) in environments where WebGPU is unavailable or disabled.

At runtime we select:

1. WebGPU if `navigator.gpu` is present and device creation succeeds.
2. Otherwise **WebGL2** as a fallback backend.

The WebGL2 backend is intentionally a subset:

- No compute shaders (GPU decompression/translation paths are unavailable).
- Fewer texture formats and pipeline state features.
- Designed to handle early milestones: BIOS/VGA framebuffer presentation and simple GPU sanity checks (e.g. triangle test).

---

## WebGL2 fallback (no WebGPU)

When `navigator.gpu` is unavailable, Aero can fall back to WebGL2 for framebuffer presentation so the project can still boot and display output in browsers that do not yet ship WebGPU.

### Color space & orientation notes

- **Texture origin conventions:** WebGL’s texture coordinate origin is effectively bottom-left, while the emulator framebuffer is treated as top-left origin. The WebGL2 blit path must therefore handle a Y flip (either during upload or in the blit shader).
- **sRGB differences:** WebGPU and WebGL2 canvases can differ in how the browser compositor applies color management. Avoid relying on exact mid-tone values unless you explicitly control linear/sRGB conversions; primary colors (0/255 channels) are typically robust for smoke tests.

## Origin Private File System (OPFS)

### File Access

```javascript
async function initializeStorage() {
    const root = await navigator.storage.getDirectory();
    
    // Create directory structure
    const imagesDir = await root.getDirectoryHandle('images', { create: true });
    const stateDir = await root.getDirectoryHandle('state', { create: true });
    
    return { root, imagesDir, stateDir };
}

// Synchronous access for worker threads
async function openDiskImage(filename) {
    const root = await navigator.storage.getDirectory();
    const imagesDir = await root.getDirectoryHandle('images');
    const fileHandle = await imagesDir.getFileHandle(filename, { create: true });
    
    // Get synchronous access handle for fast I/O in workers
    const syncHandle = await fileHandle.createSyncAccessHandle();
    
    return {
        read(buffer, offset) {
            return syncHandle.read(buffer, { at: offset });
        },
        write(buffer, offset) {
            return syncHandle.write(buffer, { at: offset });
        },
        flush() {
            syncHandle.flush();
        },
        close() {
            syncHandle.close();
        },
        getSize() {
            return syncHandle.getSize();
        },
        truncate(size) {
            syncHandle.truncate(size);
        }
    };
}
```

### Large File Handling

```javascript
// Stream-based disk image import
async function importDiskImage(file, progressCallback) {
    const root = await navigator.storage.getDirectory();
    const imagesDir = await root.getDirectoryHandle('images', { create: true });
    const destHandle = await imagesDir.getFileHandle(file.name, { create: true });
    
    const writable = await destHandle.createWritable();
    const reader = file.stream().getReader();
    
    let bytesWritten = 0;
    const totalBytes = file.size;
    
    while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        
        await writable.write(value);
        bytesWritten += value.length;
        progressCallback(bytesWritten / totalBytes);
    }
    
    await writable.close();
}
```

---

## IndexedDB (Small Persistent Caches)

IndexedDB is used for **small key/value** data that benefits from persistence across sessions but does not require OPFS throughput:

- **GPU derived artifacts** (e.g., DXBC→WGSL translations + reflection metadata)
- **Hot sector cache** (optional) for storage acceleration
- Small emulator configuration/state blobs

### Database Design (GPU Cache)

Recommended layout for a persistent GPU cache:

- Database name: `aero-gpu-cache`
- Object store: `entries`
  - key: `CacheKey` (string; includes schema version + backend kind + content hash)
  - value: `{ value_bytes, size_bytes, created_at_ms, last_access_ms }`
- Optional object store: `meta`
  - singleton record holding `total_size_bytes` to make eviction cheap

The object store value should be treated as untrusted; shader cache hits must validate WGSL (e.g., with Naga) before use.

### Opening a Database (TypeScript)

```ts
export async function openGpuCacheDb(): Promise<IDBDatabase> {
  return await new Promise((resolve, reject) => {
    const req = indexedDB.open("aero-gpu-cache", /* version */ 1);

    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains("entries")) {
        db.createObjectStore("entries");
      }
      if (!db.objectStoreNames.contains("meta")) {
        db.createObjectStore("meta");
      }
    };

    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}
```

### Clearing the Cache

The application should expose an explicit `clear_cache()` API (for UI + debugging) that:

- clears the `aero-gpu-cache` IndexedDB database
- deletes any OPFS files used by the cache (if OPFS indirection is enabled)

Users can also clear the cache via browser site data controls (e.g., DevTools → Application → Storage → Clear site data).

---

## Web Workers

### Worker Architecture

```javascript
// Main thread coordinator
class WorkerCoordinator {
    constructor() {
        this.cpuWorker = new Worker('cpu-worker.js', { type: 'module' });
        this.gpuWorker = new Worker('gpu-worker.js', { type: 'module' });
        this.ioWorker = new Worker('io-worker.js', { type: 'module' });
        this.jitWorker = new Worker('jit-worker.js', { type: 'module' });
    }

    static async create() {
        const coordinator = new WorkerCoordinator();
        await coordinator.initSharedMemory();
        return coordinator;
    }

    async initSharedMemory() {
        // Shared buffers (split; no >4GiB offsets).
        const { guestMemory, stateSab, cmdSab, eventSab } = await initializeMemory();
        this.guestMemory = guestMemory;
        this.stateSab = stateSab;
        this.cmdSab = cmdSab;
        this.eventSab = eventSab;
        this.statusFlags = new Int32Array(this.stateSab, 0, 256);

        // Initialize workers with shared memory
        [this.cpuWorker, this.gpuWorker, this.ioWorker, this.jitWorker].forEach(worker => {
            worker.postMessage({
                type: 'init',
                guestMemory: this.guestMemory,
                stateSab: this.stateSab,
                cmdSab: this.cmdSab,
                eventSab: this.eventSab,
            });
        });
    }
    
    // Note: Atomics.wait() is only allowed in workers. The main thread can use
    // Atomics.waitAsync() (where available) or poll/await messages.
    
    // Signal CPU to resume
    signalCpu() {
        Atomics.store(this.statusFlags, STATUS_CPU_RUNNING, 1);
        Atomics.notify(this.statusFlags, STATUS_CPU_RUNNING, 1);
    }
}
```

### CPU Worker

```javascript
// cpu-worker.js
import init, { CpuEmulator } from './aero_cpu.js';

let emulator = null;
let guestMemory = null;
let statusFlags = null; // Int32Array over `stateSab`

self.onmessage = async (event) => {
    const { type, data } = event.data;
    
    switch (type) {
        case 'init':
            await init();
            guestMemory = event.data.guestMemory;
            statusFlags = new Int32Array(event.data.stateSab, 0, 256);
            emulator = new CpuEmulator(guestMemory);
            break;
            
        case 'run':
            runEmulationLoop();
            break;
            
        case 'stop':
            Atomics.store(statusFlags, STATUS_STOP_REQUESTED, 1);
            break;
    }
};

function runEmulationLoop() {
    while (true) {
        // Check for stop request
        if (Atomics.load(statusFlags, STATUS_STOP_REQUESTED) === 1) {
            Atomics.store(statusFlags, STATUS_STOP_REQUESTED, 0);
            break;
        }
        
        // Execute instructions
        const result = emulator.execute_batch(10000);
        
        // Handle events
        if (result.interrupt_pending) {
            self.postMessage({ type: 'interrupt', vector: result.vector });
        }
        
        // Check if we need to wait
        if (result.halted) {
            Atomics.store(statusFlags, STATUS_CPU_RUNNING, 0);
            Atomics.wait(statusFlags, STATUS_CPU_RUNNING, 0);
        }
    }
}
```

---

## Audio Worklet

### Processor Registration

```javascript
// audio-worklet-processor.js
class AeroAudioProcessor extends AudioWorkletProcessor {
    static get parameterDescriptors() {
        return [{
            name: 'volume',
            defaultValue: 1.0,
            minValue: 0.0,
            maxValue: 1.0,
        }];
    }
    
    constructor(options) {
        super();
        
        // Shared ring buffer for audio data
        this.ringBuffer = options.processorOptions.ringBuffer;
        this.readIndex = new Uint32Array(this.ringBuffer, 0, 1);
        this.writeIndex = new Uint32Array(this.ringBuffer, 4, 1);
        this.audioData = new Float32Array(this.ringBuffer, 8);
        
        this.port.onmessage = this.handleMessage.bind(this);
    }
    
    process(inputs, outputs, parameters) {
        const output = outputs[0];
        const volume = parameters.volume[0];
        
        const readIdx = Atomics.load(this.readIndex, 0);
        const writeIdx = Atomics.load(this.writeIndex, 0);
        
        const available = (writeIdx - readIdx + this.audioData.length) % this.audioData.length;
        const samplesNeeded = output[0].length * output.length;
        
        if (available >= samplesNeeded) {
            let idx = readIdx;
            for (let channel = 0; channel < output.length; channel++) {
                const channelData = output[channel];
                for (let i = 0; i < channelData.length; i++) {
                    channelData[i] = this.audioData[idx] * volume;
                    idx = (idx + 1) % this.audioData.length;
                }
            }
            Atomics.store(this.readIndex, 0, idx);
        } else {
            // Underrun - output silence
            for (let channel = 0; channel < output.length; channel++) {
                output[channel].fill(0);
            }
            this.port.postMessage({ type: 'underrun' });
        }
        
        return true;
    }
}

registerProcessor('aero-audio-processor', AeroAudioProcessor);
```

### Audio Context Setup

```javascript
async function setupAudio() {
    const audioContext = new AudioContext({
        sampleRate: 48000,
        latencyHint: 'interactive'
    });
    
    await audioContext.audioWorklet.addModule('audio-worklet-processor.js');
    
    // Shared buffer for audio data (1 second @ 48kHz stereo)
    const ringBufferSize = 8 + (48000 * 2 * 4);
    const ringBuffer = new SharedArrayBuffer(ringBufferSize);
    
    const audioNode = new AudioWorkletNode(audioContext, 'aero-audio-processor', {
        processorOptions: { ringBuffer },
        outputChannelCount: [2]
    });
    
    audioNode.connect(audioContext.destination);
    
    return { audioContext, audioNode, ringBuffer };
}
```

---

## Input APIs

### Pointer Lock

```javascript
function setupPointerLock(canvas) {
    canvas.addEventListener('click', () => {
        if (document.pointerLockElement !== canvas) {
            canvas.requestPointerLock();
        }
    });
    
    document.addEventListener('pointerlockchange', () => {
        if (document.pointerLockElement === canvas) {
            // Pointer is locked - capture movement
            document.addEventListener('mousemove', handleMouseMove);
        } else {
            document.removeEventListener('mousemove', handleMouseMove);
        }
    });
    
    function handleMouseMove(event) {
        // movementX/Y give delta since last event
        emulator.mouseMove(event.movementX, event.movementY);
    }
}
```

### Keyboard Handling

```javascript
function setupKeyboard(canvas) {
    // Make canvas focusable
    canvas.tabIndex = 0;
    
    canvas.addEventListener('keydown', (event) => {
        event.preventDefault();
        
        const scancode = keyCodeToScancode(event.code);
        if (scancode !== 0) {
            emulator.keyDown(scancode);
        }
    });
    
    canvas.addEventListener('keyup', (event) => {
        event.preventDefault();
        
        const scancode = keyCodeToScancode(event.code);
        if (scancode !== 0) {
            emulator.keyUp(scancode);
        }
    });
}
```

---

## Network APIs

### WebSocket

All TCP egress uses the **Aero Gateway** backend (see `backend/aero-gateway`):

- [Aero Gateway API](./backend/01-aero-gateway-api.md)
- [Aero Gateway OpenAPI](./backend/openapi.yaml)

```javascript
class NetworkProxy {
    // `gatewayWsBaseUrl` should be `ws://...` or `wss://...`.
    constructor(gatewayWsBaseUrl, { token } = {}) {
        this.gatewayWsBaseUrl = gatewayWsBaseUrl;
        this.token = token;
        this.connections = new Map();
        this.nextId = 1;
    }
    
    async connect(host, port) {
        const url = new URL('/tcp', this.gatewayWsBaseUrl);
        url.searchParams.set('v', '1');
        url.searchParams.set('host', host);
        url.searchParams.set('port', String(port));

        // If the gateway is same-origin, cookie-based auth is typically sufficient.
        //
        // Browsers don't allow setting arbitrary headers on WebSocket handshakes,
        // so token auth must be passed via a WebSocket-compatible mechanism
        // (commonly `Sec-WebSocket-Protocol`). See the gateway API for the exact
        // subprotocol format.
        const protocols = this.token ? [`aero-auth.${this.token}`] : undefined;
        const ws = new WebSocket(url.toString(), protocols);
        ws.binaryType = 'arraybuffer';
         
        return new Promise((resolve, reject) => {
            ws.onopen = () => {
                const id = this.nextId++;
                this.connections.set(id, ws);
                resolve(id);
            };
            ws.onerror = reject;
        });
    }
    
    send(connectionId, data) {
        const ws = this.connections.get(connectionId);
        if (ws && ws.readyState === WebSocket.OPEN) {
            ws.send(data);
        }
    }
}
```

### WebRTC for UDP

The WebRTC UDP relay wire protocol (signaling + DataChannel framing) is
specified in [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md).

```javascript
async function setupUdpProxy(signalingUrl) {
    const pc = new RTCPeerConnection({
        iceServers: [{ urls: 'stun:stun.l.google.com:19302' }]
    });
    
    const dc = pc.createDataChannel('udp', {
        ordered: false,
        maxRetransmits: 0
    });
    
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    
    // Exchange SDP with signaling server
    const response = await fetch(signalingUrl, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ version: 1, offer: pc.localDescription })
    });
    const { answer } = await response.json();
    
    await pc.setRemoteDescription(new RTCSessionDescription(answer));
    
    return new Promise(resolve => {
        dc.onopen = () => resolve(dc);
    });
}
```

Once the DataChannel is open, each message is one UDP datagram frame. The v1
header starts with `guest_port` (u16 big-endian), which corresponds to the
guest's UDP **source port on outbound** and **destination port on inbound**.

---

## Fullscreen API

```javascript
function setupFullscreen(canvas) {
    document.addEventListener('keydown', (event) => {
        if (event.key === 'F11' || (event.key === 'Enter' && event.altKey)) {
            toggleFullscreen(canvas);
        }
    });
}

async function toggleFullscreen(element) {
    if (document.fullscreenElement) {
        await document.exitFullscreen();
    } else {
        await element.requestFullscreen({
            navigationUI: 'hide'
        });
    }
}
```

---

## Browser Compatibility Shims

```javascript
// Polyfills and fallbacks
const compat = {
    async checkSupport() {
        const issues = [];
        
        if (!navigator.gpu) {
            issues.push('WebGPU not supported - falling back to WebGL2');
        }
        
        if (typeof SharedArrayBuffer === 'undefined') {
            issues.push('SharedArrayBuffer not available - check COOP/COEP headers (see docs/security-headers.md)');
        }
        
        if (!navigator.storage?.getDirectory) {
            issues.push('OPFS not supported - using IndexedDB fallback');
        }
        
        return issues;
    },
    
    async getStorageBackend() {
        if (navigator.storage?.getDirectory) {
            return new OpfsStorage();
        }
        return new IndexedDbStorage();
    }
};
```

---

## Next Steps

- See [Testing Strategy](./12-testing-strategy.md) for browser testing
- See [Performance Optimization](./10-performance-optimization.md) for API-specific optimizations

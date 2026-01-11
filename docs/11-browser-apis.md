# 11 - Browser APIs & Web Platform Integration

## Overview

Aero leverages cutting-edge browser APIs to achieve performance and functionality. This document details all web platform features used and their browser compatibility.

---

## Deployment headers

The high-performance (threaded) build requires **cross-origin isolation** to enable `SharedArrayBuffer` and WASM threads (see [ADR 0002](./adr/0002-cross-origin-isolation.md) and [ADR 0004](./adr/0004-wasm-build-variants.md)).

When COOP/COEP is not available, the web runtime can fall back to a non-shared-memory WASM build so the UI still boots
(degraded performance/functionality is expected).

Required headers (serve on the HTML document *and* all JS/WASM/worker responses):

| Header | Value |
|---|---|
| `Cross-Origin-Opener-Policy` | `same-origin` |
| `Cross-Origin-Embedder-Policy` | `require-corp` |

Recommended hardening (optional):

- `Cross-Origin-Resource-Policy: same-origin` (CORP)

For production hosting templates and CSP guidance, see:

- [`docs/deployment.md`](./deployment.md)
- [`docs/security-headers.md`](./security-headers.md)

Minimal Vite dev server configuration:

```ts
// web/vite.config.ts
export default {
  server: {
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
      'Cross-Origin-Resource-Policy': 'same-origin',
    },
  },
};
```

### Pitfalls

- **Headers must be present on every response**, including `304 Not Modified` and worker script responses; otherwise `crossOriginIsolated` will stay `false`.
- **COEP blocks cross-origin subresources** unless they opt-in via CORS or `Cross-Origin-Resource-Policy`. Avoid loading WASM/worker bundles from a CDN unless it is configured correctly.
- **COOP changes popup/opener behavior**, which can break OAuth/login flows or integrations that rely on `window.opener`.

---

## API Dependency Matrix

| API | Chrome | Firefox | Safari | Edge | Required? |
|-----|--------|---------|--------|------|-----------|
| WebAssembly | 57+ | 52+ | 11+ | 16+ | **Yes** |
| WASM SIMD | 91+ | 89+ | 16.4+ | 91+ | **Yes** |
| WASM Threads | 74+ | 79+ | 14.1+ | 79+ | Threaded build |
| SharedArrayBuffer | 68+ | 79+ | 15.2+ | 79+ | Threaded build |
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
| WebUSB (`navigator.usb`) | 61+ | ✗ | ✗ | 79+ | Optional |
| WebHID (`navigator.hid`) | Chromium-only | ✗ | ✗ | Chromium-only | Optional |

---

## WebUSB (USB device access)

WebUSB (`navigator.usb`) enables direct access to USB peripherals from the browser.

- **Browser support:** Chromium-only (Chrome / Edge). No Firefox or Safari support.
- **Secure context:** requires `https://` (or `http://localhost`).
- **User activation:** `navigator.usb.requestDevice()` requires **transient user activation** and must be called directly from a user gesture handler on the **main thread**.
- **Workers:** user activation does **not** propagate across `postMessage()` to workers, so a “click → postMessage → worker calls `requestDevice()`” flow will fail.

### Architecture options for Aero

Because Aero prefers worker-side I/O, there are two viable integration patterns:

- **A) Main thread requests permission, worker performs I/O (best case):**
  1. Main thread handles the user gesture and calls `navigator.usb.requestDevice()`.
  2. The I/O worker calls `navigator.usb.getDevices()` and performs transfers **if** the browser exposes WebUSB in workers (`WorkerNavigator.usb`).
- **B) Main thread proxies all WebUSB I/O (fallback):**
  - If worker access is unavailable, or the `USBDevice` handle cannot be moved/shared, keep all WebUSB calls on the main thread and proxy operations over Aero’s existing main↔worker IPC.

> `USBDevice` structured-clone / transferability support is **browser-dependent** and must be treated as a runtime capability. This should be probed at runtime (see the [WebUSB probe panel](../src/main.ts) once implemented).

---

## WebHID (HID device access)

WebHID (`navigator.hid`) enables direct access to HID-class devices from the browser.

- **Browser support:** Chromium-only (Chrome / Edge). No Firefox or Safari support.
- **Secure context:** requires `https://` (or `http://localhost`).
- **User activation:** requesting a device requires a user gesture on the main thread (similar to WebUSB).
- **Report descriptor access:** WebHID does **not** expose the raw HID report descriptor bytes. It only provides a structured view (collections/reports/items), so Aero must synthesize a report descriptor when it needs byte-accurate HID descriptors for a Windows 7 guest.

See: [`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md).

---

## Cross-Origin Isolation (COOP/COEP) Deployment Requirements

Modern browsers gate **WebAssembly threads** and **SharedArrayBuffer** behind **cross-origin isolation** (a Spectre mitigation). In practice:

- `globalThis.crossOriginIsolated` must be `true`
- `SharedArrayBuffer` must be defined
- `Atomics` must be available
- Creating a shared wasm memory must succeed (feature-test via `new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true })`)

Serve the **top-level HTML document** and all app-owned **JS / worker scripts / `.wasm` responses** with:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

> `Cross-Origin-Opener-Policy` only affects documents, but it is harmless to set it on static assets; applying both headers broadly simplifies CDN/proxy configuration. `Cross-Origin-Embedder-Policy` is what enforces the “no non-opted-in cross-origin subresources” rule that can break `crossOriginIsolated`.

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

See:

- `web/vite.config.ts` for the production `web/` app
- `vite.harness.config.ts` for the repo-root dev harness (Playwright, module import tests)

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
    
    // Threads/SAB are only available when the page is cross-origin isolated.
    threads: crossOriginIsolated && typeof SharedArrayBuffer !== 'undefined',
    
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
// wasm32 WebAssembly.Memory is ≤ 4GiB addressable, and shared memories often top out below that.
// Aero splits guest RAM from control/IPC buffers to avoid relying on >4GiB offsets (ADR 0003).
const GUEST_RAM_MIB = 1024; // 512 / 1024 / 2048 / 3072 (best-effort)
const GUEST_RAM_BYTES = GUEST_RAM_MIB * 1024 * 1024;
const WASM_PAGE_BYTES = 64 * 1024;
const RING_CTRL_BYTES = 16;      // Int32Array[4] header (see docs/ipc-protocol.md)
const CMD_CAP_BYTES = 1 << 20;   // 1 MiB ring data region
const EVT_CAP_BYTES = 1 << 20;   // 1 MiB ring data region

async function initializeMemory() {
    // If this fails, fall back to the single-threaded build (ADR 0004).
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

### CSP / COOP / COEP Constraints (Production Reality)

**Why this matters:** Aero’s Tier-1/2 JIT strategy relies on compiling WASM at runtime (e.g. `WebAssembly.compile(...)`, `new WebAssembly.Module(...)`). In real deployments, these operations can be blocked by **Content Security Policy** unless the CSP explicitly allows WebAssembly compilation.

#### Minimum headers we require (threads + WASM + dynamic JIT)

To enable:
- `SharedArrayBuffer` / WASM threads (**COOP/COEP**)
- WebAssembly compilation (**CSP**)
- dynamic Tier-1/2 JIT block compilation (**CSP**)

Serve your app with:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

And a CSP similar to:

```
Content-Security-Policy:
  default-src 'self';
  script-src 'self' 'wasm-unsafe-eval';
  object-src 'none';
  base-uri 'none';
  frame-ancestors 'none'
```

Notes:
- Prefer **`'wasm-unsafe-eval'`** over **`'unsafe-eval'`** to avoid enabling arbitrary JS `eval`/`new Function`.
- If `SharedArrayBuffer` is needed, COOP/COEP must be set on the **top-level document** response (and all cross-origin subresources must be CORP/CORS compatible).

#### Observed browser behavior (Playwright 1.57.0 PoC)

A small PoC app + Playwright tests live in this repo (`web/`, `server/`, `tests/`) and validate the above in real engines.
The PoC also reports **average per-module compile+instantiate latency** (for repeated “block compilation”) and best-effort memory deltas (JS heap + `measureUserAgentSpecificMemory` when available).

| Browser engine | CSP: `script-src 'self'` (no wasm-unsafe-eval) | CSP: `script-src 'self' 'wasm-unsafe-eval'` |
| --- | --- | --- |
| Chromium (Chrome/Edge) | `WebAssembly.compile` / `new WebAssembly.Module` **blocked** | **allowed** |
| Firefox | **blocked** | **allowed** |
| WebKit (Safari) | **blocked** | **allowed** |

This implies: **if a deployment forbids `wasm-unsafe-eval`, Aero cannot rely on WebAssembly at all** (unless a JS-only interpreter fallback exists).

#### Runtime capability negotiation + fallback

The host should feature-detect and expose a capability:

```ts
const jit_dynamic_wasm: boolean = await detectDynamicWasmCompilation();
```

Then select a tier at runtime:
- If `jit_dynamic_wasm === true`: enable Tier-1/2 JIT (compile blocks to WASM).
- Else: fall back to a non-compiling execution path (e.g. interpreter-only mode), rather than crashing on startup.

See also:
- `server/poc-server.mjs` for header templates
- Host-side capability detection: `src/platform/features.ts` (`jit_dynamic_wasm`)
- PoC app: `web/public/wasm-jit-csp/`
- Tests: `tests/e2e/csp-fallback.spec.ts`

To run the PoC locally:

```bash
npm install
node server/poc-server.mjs --port 4180
```

Then open one of:
- `http://127.0.0.1:4180/csp/strict/`
- `http://127.0.0.1:4180/csp/wasm-unsafe-eval/`

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

### CI / Playwright Testing Notes

WebGPU availability in **headless** browsers varies by OS, driver, and GPU
blocklists. To keep CI reliable, WebGPU tests are:

- **Tagged** with `@webgpu` (Playwright grep tag)
- **Gated** via `AERO_REQUIRE_WEBGPU`:
  - If `navigator.gpu` is missing and `AERO_REQUIRE_WEBGPU != 1`, tests **skip**
  - If `AERO_REQUIRE_WEBGPU=1` and WebGPU is still unavailable, tests **fail**
- Run via a dedicated Playwright project, `chromium-webgpu`, which adds extra
  Chromium flags intended to maximize WebGPU availability in headless:
  - `--enable-unsafe-webgpu`: expose WebGPU in automation/insecure origins
  - `--enable-features=WebGPU`: force-enable the feature even in conservative CI builds
  - `--ignore-gpu-blocklist`: CI VMs are often GPU-blocklisted
  - `--use-angle=swiftshader` / `--use-gl=swiftshader`: prefer a software backend for determinism and to avoid requiring a host GPU
  - `--disable-gpu-sandbox`: helps in some containerized environments

Local usage:

```bash
# Run non-WebGPU tests only (default project excludes @webgpu)
npx playwright test --project=chromium

# Require WebGPU and run the WebGPU-only project
AERO_REQUIRE_WEBGPU=1 npx playwright test --project=chromium-webgpu
```

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

Concrete layout used by `web/gpu/persistent_cache.ts`:

- Database name: `aero-gpu-cache`
- Object store: `shaders` (keyPath: `key`, indexed by `lastUsed`)
  - record: `{ key, storage, opfsFile?, wgsl?, reflection?, size, createdAt, lastUsed }`
- Object store: `pipelines` (keyPath: `key`, indexed by `lastUsed`)
  - record: `{ key, storage, opfsFile?, desc?, size, createdAt, lastUsed }`

The payload may be stored inline in IndexedDB (`storage: "idb"`) or spilled to OPFS (`storage: "opfs"`, `opfsFile: "<key>.json"`) for larger blobs.

The object store value should be treated as untrusted; shader cache hits must validate WGSL (e.g., with Naga) before use.

### Opening a Database (TypeScript)

```ts
export async function openGpuCacheDb(): Promise<IDBDatabase> {
  return await new Promise((resolve, reject) => {
    const req = indexedDB.open("aero-gpu-cache", /* version */ 1);

    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains("shaders")) {
        const store = db.createObjectStore("shaders", { keyPath: "key" });
        store.createIndex("lastUsed", "lastUsed");
      }
      if (!db.objectStoreNames.contains("pipelines")) {
        const store = db.createObjectStore("pipelines", { keyPath: "key" });
        store.createIndex("lastUsed", "lastUsed");
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

In `web/gpu/persistent_cache.ts`, this is exposed as:

- `await PersistentGpuCache.clearAll()` (drop everything)
- `await cache.clearCache()` (clear stores for an existing open handle)

Users can also clear the cache via browser site data controls (e.g., DevTools → Application → Storage → Clear site data).

---

## Web Workers

### Worker Architecture

#### Atomics wait/notify on the web (important)

- `Atomics.wait()` is **not permitted on the Window (main/UI) thread** because it blocks the event loop and would freeze rendering/input. Browsers either throw or disallow it.
- On the main thread we use **non-blocking** waits:
  - Prefer `Atomics.waitAsync()` when available (woken by `Atomics.notify()` with low latency).
  - Fall back to a **polling loop** (e.g. `requestAnimationFrame`/timer) when `waitAsync` is unavailable.
- Workers should still use `Atomics.notify()` after mutating shared flags/queues; additionally, workers can `postMessage({type:'queue-notify'})` as a coarse hint so the coordinator polls shared queues sooner (especially important in the polling fallback).

```javascript
// Main thread coordinator
import { waitUntilNotEqual } from './runtime/atomics_wait.js';

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
     }
     
     // Wait for CPU worker without blocking the UI thread.
     //
     // Note: Atomics.wait() is only allowed in workers. The main thread must use
     // Atomics.waitAsync() (where available) or poll/await messages.
     async waitForCpu({ timeoutMs } = {}) {
         return waitUntilNotEqual(this.statusFlags, STATUS_CPU_RUNNING, 1, { timeoutMs });
     }
     
     // Signal CPU to resume
     signalCpu() {
         Atomics.store(this.statusFlags, STATUS_CPU_RUNNING, 1);
        Atomics.notify(this.statusFlags, STATUS_CPU_RUNNING, 1);
    }
 }
 ```

### Power / Reset Orchestration (ACPI)

ACPI fixed-feature power management is implemented via the PM1/GPE I/O ports
advertised in the FADT. When the guest requests S5 (soft-off) or a reset, the
device model must surface that to the main-thread coordinator so the browser UI
and worker lifecycle stay consistent.

#### Guest → Coordinator events

```typescript
// Worker → Coordinator
type PowerRequest =
  | { type: 'acpi_power_off_request' }
  | { type: 'acpi_reset_request' };
```

#### Coordinator handling

```typescript
class Coordinator {
  powerState: 'running' | 'shutting_down' | 'powered_off' | 'resetting' = 'running';

  onWorkerMessage(msg: any) {
    switch (msg.type) {
      case 'acpi_power_off_request':
        this.powerState = 'shutting_down';
        this.ui.setStatus('Guest requested power off (S5)…');
        this.stopAllWorkersGracefully();   // stop CPU loop, stop GPU, etc
        this.ioWorker.postMessage({ type: 'flush' }); // ensure disk flush
        this.powerState = 'powered_off';
        break;

      case 'acpi_reset_request':
        this.powerState = 'resetting';
        this.ui.setStatus('Guest requested reset…');
        this.resetInPlace(); // reset CPU+device state without page reload
        this.powerState = 'running';
        break;
    }
  }
}
```

#### UI override controls

Expose explicit host controls (useful if the guest hangs during shutdown):

- **Force Power Off**: immediately stops workers and marks VM powered off.
- **Force Reset**: resets VM state and restarts workers without a full page reload.

These should call the same coordinator entry points as the ACPI-triggered
requests to ensure consistent cleanup and state transitions.

### OffscreenCanvas Compatibility Fallback (Safari)

The preferred design is to run the GPU presenter in a dedicated worker using `OffscreenCanvas` transferred from the main thread via `HTMLCanvasElement.transferControlToOffscreen()`. However, some browsers (notably Safari) either lack `OffscreenCanvas` support in workers or do not expose `transferControlToOffscreen`.

To maintain broad browser coverage, the GPU presenter should support a **main-thread fallback mode** that uses a regular `HTMLCanvasElement` directly, while keeping higher-level code agnostic via a unified `GpuRuntime` facade.

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
             Atomics.notify(statusFlags, STATUS_CPU_RUNNING, 1);
             Atomics.wait(statusFlags, STATUS_CPU_RUNNING, 0);
         }
     }
 }
 ```

---

## Audio Worklet

> Note: The snippets below are illustrative. The canonical Aero implementation uses a
> `SharedArrayBuffer` ring buffer with a 16-byte `u32[4]` header:
>
> - `readFrameIndex` (bytes 0..4)
> - `writeFrameIndex` (bytes 4..8)
> - `underrunCount` (bytes 8..12, incremented by the worklet when it must output silence)
> - `overrunCount` (bytes 12..16, incremented by the producer when frames are dropped due to buffer full)
>
> followed by interleaved `f32` samples at byte 16. See:
> `web/src/platform/audio.ts` and `web/src/platform/audio-worklet-processor.js`.

### Processor Registration

> Note: The code below is illustrative. The canonical implementation lives in:
>
> - `web/src/platform/audio.ts` (ring buffer layout + producer)
> - `web/src/platform/audio-worklet-processor.js` (AudioWorklet consumer)
>
> The playback ring buffer uses a **16-byte header** (`u32[4]`):
> `readFrameIndex`, `writeFrameIndex`, `underrunCount`, `overrunCount`, followed by interleaved
> `f32` samples starting at byte offset 16.

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
        this.audioData = new Float32Array(this.ringBuffer, 16);
        
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
    const ringBufferSize = 16 + (48000 * 2 * 4);
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
        //
        // Many deployments pass a token directly as the selected subprotocol
        // (e.g. a base64url/JWT token), but the exact format is gateway-defined.
        const protocols = this.token ? [this.token] : undefined;
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

Once the DataChannel is open, each message is one UDP relay frame (v1 or v2).

- **v1** is the legacy IPv4-only format; its header starts with `guest_port`
  (u16 big-endian), which corresponds to the guest's UDP **source port on
  outbound** and **destination port on inbound**.
- **v2** begins with the magic/version prefix `0xA2 0x02` and supports both
  IPv4 and IPv6. See [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md).

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
        
        if (!crossOriginIsolated || typeof SharedArrayBuffer === 'undefined') {
            issues.push('SharedArrayBuffer/threads not available - check COOP/COEP headers (see docs/security-headers.md)');
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

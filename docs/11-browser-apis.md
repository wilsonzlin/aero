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
- `Origin-Agent-Cluster: ?1` (OAC)

For production hosting templates and CSP guidance, see:

- [`docs/deployment.md`](./deployment.md)
- [`docs/security-headers.md`](./security-headers.md)

Minimal Vite dev server configuration:

```ts
// web/vite.config.ts
import { defineConfig } from "vite";

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
} from "../scripts/security_headers.mjs";

export default defineConfig({
  server: {
    headers: {
      ...crossOriginIsolationHeaders,
      ...baselineSecurityHeaders,
    },
  },
});
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
| WebUSB (`navigator.usb`) | 61+ | ✗ | ✗ | 79+ | Optional (Chromium-only; limited passthrough) |
| WebHID (`navigator.hid`) | 89+ | ✗ | ✗ | 89+ | Optional (Chromium-only; HID devices) |
| WebSerial (`navigator.serial`) | 89+ | ✗ | ✗ | 89+ | Optional (Chromium-only) |
| Keyboard Lock (`navigator.keyboard.lock`) | ✓ | ✗ | ✗ | ✓ | Optional (improves VM input capture of reserved keys) |

Legend: `✓` supported · `🚧` in progress/partial · `✗` not available.

---

## Keyboard Lock (reserved key capture)

When available, Aero uses the **Keyboard Lock API** to improve delivery of browser-reserved keys (e.g. `Escape`, function keys) to the guest while input capture is active.

- API: `navigator.keyboard.lock([...codes])` and `navigator.keyboard.unlock()` (best-effort).
- **User activation:** lock requests require transient user activation and must be initiated from a user gesture handler on the main thread (Aero does this from the same click that requests Pointer Lock).
- **Failure handling:** lock/unlock are optional and never cause capture to fail; rejections are logged and ignored.
- **Important:** locking `Escape` can prevent the browser’s default “Escape exits pointer lock” behavior, so apps should provide an alternative capture-exit path (e.g. a host-only release chord or an on-screen UI control).

---

## WebUSB (USB device access)

WebUSB (`navigator.usb`) enables direct access to USB peripherals from the browser.

- **Browser support:** Chromium-only (Chrome / Edge). No Firefox or Safari support.
- **Secure context:** requires `https://` (or `http://localhost`).
- **User activation:** `navigator.usb.requestDevice()` requires **transient user activation** and must be called directly from a user gesture handler on the **main thread**.
- **Workers:** user activation does **not** propagate across `postMessage()` to workers, so a “click → postMessage → worker calls `requestDevice()`” flow will fail.
- **Runtime support:** WebUSB passthrough is currently implemented for the legacy browser runtime (`vmRuntime=legacy`), where guest USB controller/device models live in the I/O worker. In `vmRuntime=machine`, guest USB device models live inside the canonical `api.Machine` owned by the machine CPU worker and the I/O worker runs in host-only stub mode, so passthrough is not yet available.
- **Canonical stack selection:** for the browser runtime, the canonical USB stack is `aero-usb` + `aero-wasm` + `web/`; see [ADR 0015](./adr/0015-canonical-usb-stack.md).
- **UHCI passthrough mapping:** for the guest UHCI TD ↔ WebUSB transfer mapping (including “TD-level NAK while pending”), see [`docs/webusb-passthrough.md`](./webusb-passthrough.md).
- **Troubleshooting:** for `requestDevice()` / `open()` / `claimInterface()` failures (protected interface classes, WinUSB/udev permissions, etc.), see [`docs/webusb.md`](./webusb.md).

### Architecture options for Aero

Because Aero prefers worker-side I/O, there are two viable integration patterns:

- **A) Main thread requests permission, worker performs I/O (best case):**
  1. Main thread handles the user gesture and calls `navigator.usb.requestDevice()`.
  2. The I/O worker calls `navigator.usb.getDevices()` and performs transfers **if** the browser exposes WebUSB in workers (`WorkerNavigator.usb`).
- **B) Main thread proxies all WebUSB I/O (fallback):**
  - If worker access is unavailable, or the `USBDevice` handle cannot be moved/shared, keep all WebUSB calls on the main thread and proxy operations over Aero’s existing main↔worker IPC.
  - In the production web runtime (`web/`), this is implemented by `UsbBroker` (main thread) + `WebUsbPassthroughRuntime` (worker) using a `MessagePort` protocol (`web/src/usb/{usb_broker,webusb_passthrough_runtime,usb_proxy_protocol}.ts`).
  - When `crossOriginIsolated` and `SharedArrayBuffer`/`Atomics` are available, the same proxy can enable an optional SharedArrayBuffer ring fast path negotiated by `usb.ringAttach` (`web/src/usb/usb_proxy_ring.ts`):
    - **action ring (worker → main):** `UsbHostAction` records
    - **completion ring (main → worker):** `UsbHostCompletion` records
    - The ring path is opportunistic: when rings are unavailable (or temporarily full), messages fall back to typed `postMessage` (`usb.action` / `usb.completion`).
    - Late-starting worker runtimes can request the rings via `usb.ringAttachRequest`.
    - If a ring becomes corrupted (decode error), runtimes can disable the ring fast path by sending `usb.ringDetach` and falling back to `postMessage`.
  - The repo-root Vite harness previously contained a **legacy/demo WebUSB RPC** under
    `src/platform/legacy/webusb_{broker,client,protocol}.ts` (direct `navigator.usb` operations). It has been
    removed and is not the canonical passthrough `UsbHostAction` contract.

> `USBDevice` structured-clone / transferability support is **browser-dependent** and must be treated as a runtime capability. Probe this at runtime via the production WebUSB smoke-test panel (`web/src/usb/webusb_panel.ts`) or the dedicated diagnostics page (`/webusb_diagnostics.html`).
>
> The repo-root dev harness also has a WebUSB probe panel (`src/main.ts`).
> If you see `DataCloneError` when trying to `postMessage()` a `USBDevice` to a worker, your browser does not support structured-cloning the device handle. In that case, keep WebUSB on the main thread and proxy I/O to workers, or have the worker call `navigator.usb.getDevices()` after permission is granted.

See also: [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md).

---

## WebHID (HID device access)

WebHID (`navigator.hid`) enables direct access to HID-class devices from the browser.

- **Browser support:** Chromium-only (Chrome / Edge). No Firefox or Safari support.
- **Secure context:** requires `https://` (or `http://localhost`).
- **User activation:** requesting a device requires a user gesture on the main thread (similar to WebUSB).
- **Report descriptor access:** WebHID does **not** expose the raw HID report descriptor bytes. It only provides a structured view (collections/reports/items), so Aero must synthesize a report descriptor when it needs byte-accurate HID descriptors for a Windows 7 guest.
- **Runtime support:** WebHID passthrough is currently implemented for the legacy browser runtime (`vmRuntime=legacy`) via the I/O worker USB/HID stack. In `vmRuntime=machine`, guest USB device models live inside the canonical `api.Machine` owned by the machine CPU worker and the I/O worker runs in host-only stub mode, so passthrough is not yet available.
- **Main↔worker proxying (Aero runtime):** WebHID handles are main-thread-only, so the production runtime forwards report traffic to the I/O worker:
  - Default path: typed `postMessage` (`hid.inputReport`, `hid.sendReport`, `hid.getFeatureReport`, `hid.featureReportResult`).
  - Fast path (when `crossOriginIsolated` and SAB/Atomics are available): SharedArrayBuffer rings (`hid.ring.init` for input reports, `hid.ringAttach` for output/feature reports). On detected ring corruption either side can send `hid.ringDetach` to disable the SAB fast paths and fall back to `postMessage`.

See: [`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md).
See also: [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md).

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

- `vite.harness.config.ts` for the repo-root Vite app (used by CI/Playwright)
- `web/vite.config.ts` for the legacy/experimental `web/` Vite app

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

*Preflight (`OPTIONS`) response*:

```http
HTTP/1.1 204 No Content
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since, Authorization
Access-Control-Max-Age: 600
Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers
```

*Disk byte response (`GET`/`HEAD`, including `206 Partial Content`)*:

```http
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Content-Encoding
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
        0x03, 0x02, 0x01, 0x00, 0x0a, 0x08, 0x01,
        0x06, 0x00, 0x41, 0x00, 0xfd, 0x11, 0x0b
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
import { allocateSharedMemorySegments, createSharedMemoryViews, guestToLinear } from './runtime/shared_layout.js';

// Guest RAM is configurable and must fit within wasm32 + browser limits.
// wasm32 WebAssembly.Memory is ≤ 4GiB addressable, and shared memories often top out below that.
// Aero splits guest RAM from control/IPC buffers to avoid relying on >4GiB offsets (ADR 0003).
const GUEST_RAM_MIB = 1024; // 512 / 1024 / 2048 / 3072 (best-effort)

function initializeMemory() {
    // If this fails, fall back to the single-threaded build (ADR 0004).
    const segments = allocateSharedMemorySegments({ guestRamMiB: GUEST_RAM_MIB });
    const shared = createSharedMemoryViews(segments);

    // IMPORTANT: The wasm linear memory is shared between guest RAM and the Rust/WASM runtime
    // (stack/heap/statics). Guest physical address 0 does NOT map to linear address 0.
    //
    // `guest_base` is the byte offset inside `guestMemory` where guest physical address 0 begins.
    // This is written by the coordinator into the control/status SAB and then treated as immutable.
    const { guest_base, guest_size } = shared.guestLayout;

    // Translate guest physical addresses to wasm linear addresses.
    //
    // Note: on the canonical PC/Q35 platform, guest physical RAM is not a single contiguous range:
    // there is an ECAM + PCI/MMIO hole below 4 GiB, and any remaining RAM is remapped above 4 GiB.
    // The runtime `guestToLinear` helper (implemented via `web/src/arch/guest_ram_translate.ts`)
    // enforces this piecewise mapping and rejects hole accesses.
    const guestToLinearAddr = (paddr) => guestToLinear(shared.guestLayout, paddr);

    // `segments` contains:
    // - `control`: SharedArrayBuffer for status + per-worker command/event rings
    // - `guestMemory`: shared WebAssembly.Memory for guest RAM
    // - `vram`: optional SharedArrayBuffer backing the AeroGPU BAR1/VRAM aperture (for shared MMIO + scanout)
    // - `ioIpc`: SharedArrayBuffer for high-frequency CPU<->IO IPC
    // - `sharedFramebuffer`: demo/legacy display buffer
    // - `scanoutState`: SharedArrayBuffer for the shared ScanoutState descriptor (legacy/VBE ↔ WDDM handoff)
    // - `cursorState`: SharedArrayBuffer for the shared hardware cursor descriptor (WDDM cursor regs)
    return { segments, shared, guestToLinear: guestToLinearAddr };
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
- `texture-compression-etc2` for ETC2 textures (common on mobile; otherwise decompress on CPU)
- `texture-compression-astc` for ASTC textures (common on mobile; otherwise decompress on CPU)
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
        'texture-compression-etc2',
        'texture-compression-astc',
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
  - If `navigator.gpu` is missing and `AERO_REQUIRE_WEBGPU` is unset/false, tests **skip**
  - If `AERO_REQUIRE_WEBGPU` is enabled (`1/true/yes/on`) and WebGPU is still unavailable, tests **fail**
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

Note: In Aero, the boot-critical synchronous OPFS backend used by the Rust disk/controller stack is
implemented in Rust/wasm32 in `crates/aero-opfs` (e.g. `aero_opfs::OpfsByteStorage`). The snippets
below show the underlying browser APIs for reference and are not necessarily the exact
implementation used in this repo.

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

    // Prefer truncating overwrites so importing over an existing file does not leave trailing bytes.
    // Some implementations may reject the options bag; fall back to `createWritable()` and
    // best-effort truncate.
    let writable;
    let truncateFallback = false;
    try {
        writable = await destHandle.createWritable({ keepExistingData: false });
    } catch {
        writable = await destHandle.createWritable();
        truncateFallback = true;
    }
    if (truncateFallback) {
        try {
            await writable.truncate(0);
        } catch {
            // ignore
        }
    }

    const reader = file.stream().getReader();
    
    let bytesWritten = 0;
    const totalBytes = file.size;
    
    try {
        while (true) {
            const { done, value } = await reader.read();
            if (done) break;
            
            await writable.write(value);
            bytesWritten += value.length;
            progressCallback(bytesWritten / totalBytes);
        }
        
        await writable.close();
    } catch (err) {
        // Abort on error so a failed write does not leave behind a partially-written/truncated file.
        try {
            await reader.cancel(err);
        } catch {
            // ignore
        }
        try {
            await writable.abort(err);
        } catch {
            // ignore
        }
        throw err;
    }
}
```

---

## IndexedDB (Small Persistent Caches)

Note: IndexedDB is async. The Rust/wasm32 async IndexedDB block store lives in `crates/st-idb`,
and it is not currently exposed as a synchronous `aero_storage::StorageBackend` /
`aero_storage::VirtualDisk`; see [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md)
and [`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

IndexedDB is used for **small key/value** data that benefits from persistence across sessions but does not require OPFS throughput:

- **GPU derived artifacts** (e.g., DXBC→WGSL translations + reflection metadata)
- **Hot sector cache** (optional) for storage acceleration
- Small emulator configuration/state blobs

### Database Design (GPU Cache)

Concrete layout used by `web/gpu-cache/persistent_cache.ts`:

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

In `web/gpu-cache/persistent_cache.ts`, this is exposed as:

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
// Main thread coordinator (simplified; see `web/src/runtime/coordinator.ts`)
import { waitUntilNotEqual } from './atomics_wait.js';
import { allocateSharedMemorySegments, createSharedMemoryViews, StatusIndex } from './shared_layout.js';

class WorkerCoordinator {
    constructor() {
        // The CPU worker entrypoint depends on the VM runtime implementation:
        // - legacy:  `cpu.worker.ts` (CPU-only wasm harness + JS IO/MMIO shims)
        // - machine: `machine_cpu.worker.ts` (canonical `api.Machine` runtime)
        const vmRuntime = 'legacy'; // or 'machine'
        const cpuWorkerEntrypoint =
            vmRuntime === 'machine' ? '../workers/machine_cpu.worker.ts' : '../workers/cpu.worker.ts';
        this.cpuWorker = new Worker(new URL(cpuWorkerEntrypoint, import.meta.url), { type: 'module' });
        this.gpuWorker = new Worker(new URL('../workers/gpu.worker.ts', import.meta.url), { type: 'module' });
        this.ioWorker = new Worker(new URL('../workers/io.worker.ts', import.meta.url), { type: 'module' });
        this.jitWorker = new Worker(new URL('../workers/jit.worker.ts', import.meta.url), { type: 'module' });
    }

    static async create() {
        const coordinator = new WorkerCoordinator();
        await coordinator.initSharedMemory();
        return coordinator;
    }

    async initSharedMemory() {
        // Shared buffers (split; no >4GiB offsets).
        //
        // The canonical web runtime allocation is:
        // - `controlSab`: a small SharedArrayBuffer holding status + command/event rings per worker
        // - `guestMemory`: shared WebAssembly.Memory holding guest RAM
        // - `vram`: optional SharedArrayBuffer backing the VRAM/BAR1 aperture (outside guest RAM)
        // - `ioIpcSab`: separate AIPC rings for high-frequency CPU<->IO ops
        // - `scanoutState`: shared scanout descriptor (legacy/VBE ↔ WDDM handoff)
        // - `cursorState`: shared hardware cursor descriptor (WDDM cursor regs)
        const segments = allocateSharedMemorySegments();
        this.shared = createSharedMemoryViews(segments);
        this.statusFlags = this.shared.status;

        // Initialize workers with shared memory.
        const workers = [
            ['cpu', this.cpuWorker],
            ['gpu', this.gpuWorker],
            ['io', this.ioWorker],
            ['jit', this.jitWorker],
        ];
        workers.forEach(([role, worker]) => {
            // In production code the init payload also includes optional `wasmModule`
            // (structured-cloneable WebAssembly.Module), perf channels, and other
            // attachments (see `web/src/runtime/protocol.ts`).
            worker.postMessage({
                kind: 'init',
                role,
                controlSab: segments.control,
                guestMemory: segments.guestMemory,
                // Shared VRAM aperture (BAR1 backing). Optional in some test harnesses.
                vram: segments.vram,
                // Current web runtime contract: VRAM lives at `PCI_MMIO_BASE = 0xE000_0000`.
                // See `web/src/arch/guest_phys.ts`.
                vramBasePaddr: segments.vram ? 0xE000_0000 : undefined,
                vramSizeBytes: segments.vram ? segments.vram.byteLength : undefined,
                ioIpcSab: segments.ioIpc,
                sharedFramebuffer: segments.sharedFramebuffer,
                sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
                scanoutState: segments.scanoutState,
                scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
                cursorState: segments.cursorState,
                cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
            });
        });
    }
      
    // Wait for CPU worker readiness without blocking the UI thread.
    //
    // Note: Atomics.wait() is only allowed in workers. The main thread must use
    // Atomics.waitAsync() (where available) or poll/await messages.
    async waitForCpuReady({ timeoutMs } = {}) {
        // The worker sets `StatusIndex.CpuReady = 1` during init.
        return waitUntilNotEqual(this.statusFlags, StatusIndex.CpuReady, 0, { timeoutMs });
    }
      
    // Signal the VM to stop (one-way flag; the coordinator resets status on restart).
    requestStop() {
        Atomics.store(this.statusFlags, StatusIndex.StopRequested, 1);
        Atomics.notify(this.statusFlags, StatusIndex.StopRequested, 1);
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

> Note: The snippet below is illustrative (legacy runtime, `vmRuntime=legacy`). The current
> wasm-bindgen entrypoint is `aero_wasm.js` (built from `crates/aero-wasm`). The worker runtime has
> two CPU worker entrypoints:
>
> - `vmRuntime=legacy`: `web/src/workers/cpu.worker.ts` (drives `WasmVm` / `WasmTieredVm`)
> - `vmRuntime=machine`: `web/src/workers/machine_cpu.worker.ts` (drives the canonical `Machine`)
>
> Both use helpers in `web/src/runtime/`.
>
> The real runtime may use `WasmTieredVm` (tiered/Tier-1 JIT) in addition to `WasmVm` (Tier-0). For the canonical
> mapping of `aero-wasm` exports to runtime paths, see [`docs/vm-crate-map.md`](./vm-crate-map.md) and
> [ADR 0014](./adr/0014-canonical-machine-stack.md).

```javascript
// cpu.worker.js (illustrative, legacy vmRuntime=legacy)
import { initWasmForContext } from "/web/src/runtime/wasm_context";
import {
    CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
    CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
    CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
    CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES,
    StatusIndex,
    STATUS_INTS,
    readGuestRamLayoutFromStatus,
} from "/web/src/runtime/shared_layout";

// `WasmApi` exported by `crates/aero-wasm` (see `web/src/runtime/wasm_loader.ts`).
let wasm = null;
 
// Minimal Tier-0 VM loop (wired to injected shared guest RAM).
//
// Note: `crates/aero-wasm` also exports `Machine` (the canonical full-system VM,
// `aero_machine::Machine`). In `vmRuntime=legacy`, the CPU worker runtime uses
// the CPU-only exports (`WasmVm` / `WasmTieredVm`) and forwards port I/O + MMIO
// back to JS. In `vmRuntime=machine`, the CPU worker uses `Machine` directly
// (see `web/src/workers/machine_cpu.worker.ts`).
let vm = null;

// Optional lightweight demo harness for the CPU worker (threaded build only).
let cpuDemo = null;

let guestMemory = null;
let status = null; // Int32Array view into `controlSab`

self.onmessage = async (event) => {
    const msg = event.data;

    switch (msg.kind) {
        case "init": {
            guestMemory = msg.guestMemory;
            status = new Int32Array(msg.controlSab, STATUS_OFFSET_BYTES, STATUS_INTS);

            const { api } = await initWasmForContext({
                variant: msg.wasmVariant ?? "auto",
                memory: guestMemory,
                module: msg.wasmModule,
            });
            wasm = api;

            // Guest RAM layout is published by the coordinator in the status SAB.
            const layout = readGuestRamLayoutFromStatus(status);

            // Create a view of guest RAM (paddr 0..guest_size) inside the module's
            // shared wasm32 linear memory.
            const guestU8 = new Uint8Array(guestMemory.buffer, layout.guest_base, layout.guest_size);

            // Minimal Tier-0 VM wrapper: reset to real mode and start at 0x7C00.
            if (wasm.WasmVm) {
                vm = new wasm.WasmVm(layout.guest_base, layout.guest_size);

                // In the real CPU worker, a boot sector is loaded from disk into
                // guest memory at 0x7C00 before resetting.
                guestU8.set([0x90, 0xF4], 0x7c00); // NOP; HLT (placeholder)
                vm.reset_real_mode(0x7c00);
            }

            // Optional CPU worker demo (writes frames/counters inside the module's
            // linear memory).
            if (wasm.CpuWorkerDemo && msg.sharedFramebufferOffsetBytes) {
                const ramSizeBytes = guestMemory.buffer.byteLength >>> 0;
                const framebufferLinearOffset = msg.sharedFramebufferOffsetBytes >>> 0;
                const guestCounterLinearOffset =
                    (layout.guest_base + CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES) >>> 0;

                cpuDemo = new wasm.CpuWorkerDemo(
                    ramSizeBytes,
                    framebufferLinearOffset,
                    CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
                    CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
                    CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
                    guestCounterLinearOffset,
                );
            }
            break;
        }
        case "run":
            runEmulationLoop();
            break;
        case "stop":
            Atomics.store(status, StatusIndex.StopRequested, 1);
            break;
    }
};

function runEmulationLoop() {
    while (Atomics.load(status, StatusIndex.StopRequested) === 0) {
        // Execute a slice of guest work (Tier-0 VM path).
        //
        // `run_slice` returns an object with a small enum-like `kind` field
        // (Completed/Halted/ResetRequested/Assist/Exception) plus an instruction count.
        if (vm) {
            const exit = vm.run_slice(10_000);
            exit.free();
        }

        // Optional demo harness path (shared-memory pattern generator).
        if (cpuDemo) {
            const now = performance.now();
            cpuDemo.tick(now);
            cpuDemo.render_frame(0 /* frameSeq */, now);
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
> - `underrunCount` (bytes 8..12, total missing output frames rendered as silence due to underruns; wraps at 2^32)
> - `overrunCount` (bytes 12..16, total frames dropped by the producer due to buffer full; wraps at 2^32)
>
> followed by interleaved `f32` samples at byte 16. See:
> `web/src/audio/audio_worklet_ring.ts`, `web/src/platform/audio_worklet_ring_layout.js`, `web/src/platform/audio.ts`, and `web/src/platform/audio-worklet-processor.js`.
>
> Note: In production builds, `audio-worklet-processor.js` is emitted as a standalone asset and imports
> `./audio_worklet_ring_layout.js` at runtime; see `web/vite.config.ts` and `vite.harness.config.ts` for the explicit
> asset emission workaround (Vite does not automatically follow ESM imports from AudioWorklet modules).
>
> The canonical Aero worklet supports a small **control channel**: the main thread may post
> `{ type: "ring.reset" }` to the `AudioWorkletNode.port` to discard any buffered playback backlog
> (`readFrameIndex := writeFrameIndex`).
>
> Aero’s canonical integration uses the **main-thread** `CreateAudioOutputOptions.discardOnResume`
> option in `createAudioOutput` (default: `true`) to post `{ type: "ring.reset" }` on
> `AudioContext` **suspend → running** transitions (except the first-ever start) to avoid stale
> latency after interruptions/tab backgrounding. `createAudioOutput` does **not** currently set
> `processorOptions.discardOnResume`.
>
> Separately, the worklet supports an optional worklet-side **auto-discard heuristic** for
> integrations that want self-healing purely from the audio thread: if
> `processorOptions.discardOnResume === true` is set on `AudioWorkletNode` construction
> (default: `false`), the worklet will discard buffered backlog after detecting a large wall-clock
> gap between `process()` callbacks (a proxy for suspend/resume or extreme scheduling stalls).
>
> If you manually construct an `AudioWorkletNode`, you may either enable
> `processorOptions.discardOnResume` **or** implement the main-thread `ring.reset` posting
> (`discardOnResume`-style behavior). The main-thread reset is recommended when you can wire it,
> because it is deterministic; the worklet heuristic is best-effort.
>
> The canonical Aero worklet can also post `type: "underrun"` messages for debugging/telemetry, but
> these are disabled by default (and rate-limited when enabled) to avoid overhead under persistent
> underrun. See `CreateAudioOutputOptions.sendUnderrunMessages` /
> `CreateAudioOutputOptions.underrunMessageIntervalMs` in `web/src/platform/audio.ts`.

### Processor Registration

> Note: The code below is illustrative. The canonical implementation lives in:
>
> - `web/src/platform/audio.ts` (ring buffer layout + producer)
> - `web/src/audio/audio_worklet_ring.ts` + `web/src/platform/audio_worklet_ring_layout.js` (ring buffer layout/constants + helper math)
> - `web/src/platform/audio-worklet-processor.js` (AudioWorklet consumer)
>
> The playback ring buffer uses a **16-byte header** (`u32[4]`):
> `readFrameIndex`, `writeFrameIndex`, `underrunCount`, `overrunCount`, followed by interleaved
> `f32` samples starting at byte offset 16.

```javascript
// audio-worklet-processor.js
import {
    READ_FRAME_INDEX,
    WRITE_FRAME_INDEX,
    UNDERRUN_COUNT_INDEX,
    OVERRUN_COUNT_INDEX,
    HEADER_U32_LEN,
    HEADER_BYTES,
    framesAvailableClamped,
} from './audio_worklet_ring_layout.js';

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
        
        // Shared ring buffer for audio data (SPSC):
        // - producer: emulator worker
        // - consumer: AudioWorklet
        //
        // Indices are monotonic u32 *frame counters* (wrapping naturally at 2^32),
        // not modulo indices.
        this.ringBuffer = options.processorOptions.ringBuffer;
        this.header = new Uint32Array(this.ringBuffer, 0, HEADER_U32_LEN);
        this.audioData = new Float32Array(this.ringBuffer, HEADER_BYTES);
        this.channelCount = options.processorOptions.channelCount ?? 2;
        this.capacityFrames =
            options.processorOptions.capacityFrames ?? Math.floor(this.audioData.length / this.channelCount);
    }
    
    process(inputs, outputs, parameters) {
        const output = outputs[0];
        const volume = parameters.volume[0];
        
        const cc = Math.min(this.channelCount, output.length);
        const framesNeeded = output[0].length;
        const capacityFrames = this.capacityFrames;
        
        const readFrameIndex = Atomics.load(this.header, READ_FRAME_INDEX) >>> 0;
        const writeFrameIndex = Atomics.load(this.header, WRITE_FRAME_INDEX) >>> 0;
        const availableFrames = framesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames);
        const framesToRead = Math.min(framesNeeded, availableFrames);

        const readPos = readFrameIndex % capacityFrames;
        const firstFrames = Math.min(framesToRead, capacityFrames - readPos);
        const secondFrames = framesToRead - firstFrames;

        // Copy first contiguous chunk.
        for (let i = 0; i < firstFrames; i++) {
            const base = (readPos + i) * cc;
            for (let c = 0; c < cc; c++) {
                output[c][i] = this.audioData[base + c] * volume;
            }
        }

        // Copy wrapped chunk.
        for (let i = 0; i < secondFrames; i++) {
            const base = i * cc;
            for (let c = 0; c < cc; c++) {
                output[c][firstFrames + i] = this.audioData[base + c] * volume;
            }
        }

        // Underrun: render silence for missing frames and increment the underrun *frame* counter.
        if (framesToRead < framesNeeded) {
            const missing = framesNeeded - framesToRead;
            for (let c = 0; c < output.length; c++) {
                output[c].fill(0, framesToRead);
            }
            const prev = Atomics.add(this.header, UNDERRUN_COUNT_INDEX, missing);
            const newTotal = (prev + missing) >>> 0;
            this.port.postMessage({
                type: 'underrun',
                underrunFramesAdded: missing,
                underrunFramesTotal: newTotal,
                // Backwards-compatible field name; this is a frame counter (not events).
                underrunCount: newTotal,
            });
        }

        if (framesToRead > 0) {
            Atomics.store(this.header, READ_FRAME_INDEX, readFrameIndex + framesToRead);
        }
        
        return true;
    }
}

registerProcessor('aero-audio-processor', AeroAudioProcessor);
```

### Audio Context Setup

> In Aero, prefer using the canonical helper `createAudioOutput` (`web/src/platform/audio.ts`) instead of wiring Web Audio
> directly. It handles:
>
> - `AudioContext` vs `webkitAudioContext` (Safari/WebKit)
> - fallback constructor option sets (some browsers throw on certain `AudioContext` option combinations)
> - optional startup/latency-smoothing policies like `startupPrefillFrames` and `discardOnResume` (see `docs/06-audio-subsystem.md`)
> - `AudioWorkletNode` option compatibility fallbacks (e.g. `outputChannelCount` support in older Safari/WebKit)

```javascript
async function setupAudio() {
    // NOTE: browsers may ignore the requested sample rate. Always use
    // `audioContext.sampleRate` (the *actual* rate) when sizing buffers or
    // configuring resamplers to avoid pitch/speed shifts (Safari/iOS commonly
    // runs at 44.1kHz even if 48kHz is requested).
    const requestedSampleRate = 48000;
    const audioContext = new AudioContext({
        sampleRate: requestedSampleRate,
        latencyHint: 'interactive'
    });

    // Optional diagnostics: some browsers expose output latency estimates.
    // Aero surfaces these (when available) via `AudioOutputMetrics`:
    //   - baseLatencySeconds (`AudioContext.baseLatency`)
    //   - outputLatencySeconds (`AudioContext.outputLatency`)
    // See: `web/src/platform/audio.ts` (`getMetrics`, `startAudioPerfSampling`).
     
    await audioContext.audioWorklet.addModule('audio-worklet-processor.js');
    
    const sampleRate = audioContext.sampleRate;

    // Shared buffer for audio data (example: ~1 second of stereo frames).
    const channelCount = 2;
    const capacityFrames = sampleRate;
    // The playback ring uses a fixed u32[4] header (16 bytes). Prefer importing
    // `HEADER_BYTES` from `audio_worklet_ring_layout.js` in real code.
    const ringBufferBytes = 16 + capacityFrames * channelCount * Float32Array.BYTES_PER_ELEMENT;
    const ringBuffer = new SharedArrayBuffer(ringBufferBytes);
    
    const audioNode = new AudioWorkletNode(audioContext, 'aero-audio-processor', {
        processorOptions: { ringBuffer, channelCount, capacityFrames },
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
    canvas.addEventListener('click', (event) => {
        if (document.pointerLockElement !== canvas) {
            // Avoid bubbling the click to app-level handlers while transitioning
            // into capture mode.
            event.preventDefault();
            event.stopPropagation();
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
        // Prevent page-level listeners from observing pointer-locked movement.
        event.preventDefault();
        event.stopPropagation();
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
        event.stopPropagation();
        
        const scancode = keyCodeToScancode(event.code);
        if (scancode !== 0) {
            emulator.keyDown(scancode);
        }
    });
    
    canvas.addEventListener('keyup', (event) => {
        event.preventDefault();
        event.stopPropagation();
        
        const scancode = keyCodeToScancode(event.code);
        if (scancode !== 0) {
            emulator.keyUp(scancode);
        }
    });
}
```

> Note: if your app also installs **global hotkeys** (e.g. `window.addEventListener('keydown', ...)`),
> always check `event.defaultPrevented` and return early when it is `true`. This ensures VM input
> capture (which calls `preventDefault()`/`stopPropagation()` for swallowed events) can reliably
> suppress host/UI shortcuts during capture.

---

## Network APIs

### Network tracing (PCAPNG export)

For packet-level debugging (capturing the guest↔tunnel Ethernet frames and exporting a `.pcapng`
file for Wireshark), see:

- [`07-networking.md`](./07-networking.md#network-tracing-pcappcapng-export) – **Network Tracing (PCAP/PCAPNG Export)**
  - Includes the browser UI panel and `window.aero.netTrace` automation API.

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
         const url = new URL(this.gatewayWsBaseUrl);
         // `gatewayWsBaseUrl` may include a base path (e.g. `wss://example.com/aero`).
         // Append `/tcp` without dropping the base path.
         url.search = '';
         url.hash = '';
         const basePath = url.pathname.replace(/\/$/, '');
         url.pathname = basePath.endsWith('/tcp') ? basePath : `${basePath}/tcp`;
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

Server-side inbound filtering note: `proxy/webrtc-udp-relay` defaults to `UDP_INBOUND_FILTER_MODE=address_and_port`
(only accept inbound UDP from remote address+port tuples the guest previously sent to). If you need full-cone
behavior (accept inbound UDP from any remote), set `UDP_INBOUND_FILTER_MODE=any` (**less safe**; see the relay README).

Server-side DoS hardening note: `proxy/webrtc-udp-relay` configures pion/SCTP message-size caps to prevent malicious peers
from sending extremely large WebRTC DataChannel messages that would otherwise be buffered/allocated before `DataChannel.OnMessage` runs.
Oversized messages may cause the relay to close the DataChannel or the entire session. Relevant knobs:

- `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (SDP `a=max-message-size` hint; 0 = auto)
- `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` (hard receive-side cap; 0 = auto; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`)
- `WEBRTC_SESSION_CONNECT_TIMEOUT` (close server-side PeerConnections that never reach a connected state; default `30s`)

```javascript
async function setupUdpProxy(signalingUrl) {
    const pc = new RTCPeerConnection({
        iceServers: [{ urls: 'stun:stun.l.google.com:19302' }]
    });
    
    // Note: this DataChannel config is for the UDP relay, where best-effort/lossy semantics are
    // acceptable. If you carry the **L2 tunnel** over WebRTC, the channel MUST be reliable and
    // ordered (do NOT set `maxRetransmits`/`maxPacketLifeTime`, and do not set `ordered: false`).
    // See ADR 0013 and `docs/l2-tunnel-protocol.md`.
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
            issues.push('OPFS not supported - IndexedDB fallback is async-only (Rust sync controller path unavailable)');
        }
        
        return issues;
    },
    
    async getStorageBackend() {
        if (navigator.storage?.getDirectory) {
            return new OpfsStorage();
        }
        // IndexedDB is async-only; if the runtime uses a synchronous Rust controller stack,
        // it cannot “block on IndexedDB” in the same worker without deadlocking.
        // See: docs/19-indexeddb-storage-story.md and docs/20-storage-trait-consolidation.md
        return new IndexedDbStorage();
    }
};
```

---

## Next Steps

- See [Testing Strategy](./12-testing-strategy.md) for browser testing
- See [Performance Optimization](./10-performance-optimization.md) for API-specific optimizations

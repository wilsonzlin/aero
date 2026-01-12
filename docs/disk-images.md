# Disk Images: Local vs Streaming (HTTP Range / Chunked)

## Overview

Aero can use **raw disk images** in two ways:

1. **Local images**: you provide a file (or one is generated) and Aero stores it in browser storage
   (OPFS preferred; IndexedDB fallback in some environments).
2. **Streaming images**: you provide a **URL** to a remote image and Aero reads it lazily, caching only the blocks/chunks it actually touches.
   - **HTTP Range** (single file): fetches with `Range: bytes=...`
   - **Chunked manifest** (many files): fetches `manifest.json` + `chunks/*.bin` with plain `GET` (no `Range` header)

Streaming is essential for very large images (20GB+) because it avoids a full upfront download.

Note: OPFS is the preferred backend for local images. Aero can fall back to IndexedDB for some
host-side storage flows when OPFS sync access handles are unavailable, but IndexedDB is async-only
and does not currently back the synchronous Rust disk/controller path; see
[`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md).

## Legal / Responsible Use (Important)

You must only use disk images that you **own** or are otherwise **licensed to use**.

Do **not** use Aero’s streaming support to access or distribute pirated Windows installers or disk images. If you don’t have explicit rights to the content, don’t point Aero at it.

## Streaming images: server requirements

### Mode A: HTTP Range (single object)

To stream a remote image, the server **must** support byte-range requests:

- `Accept-Ranges: bytes`
- `Content-Length` on `HEAD`/`GET`
- Correct `206 Partial Content` responses to `Range: bytes=start-end`
- Correct `Content-Range: bytes start-end/total`

Notes:

- Some servers disallow `HEAD`. Aero can fall back to a small `Range: bytes=0-0` probe,
  but that requires a valid `Content-Range` header (and appropriate CORS exposure).

### CORS headers (browser requirement)

Browsers will block cross-origin reads unless the server is configured for CORS.

For a self-contained local setup (MinIO + optional reverse proxy) to validate Range + CORS behavior, see:
[`infra/local-object-store/README.md`](../infra/local-object-store/README.md).

Because `Range` is not a CORS-safelisted request header, cross-origin reads will trigger an `OPTIONS`
preflight. At minimum, the server should respond with headers similar to:

*Preflight (`OPTIONS`) response*:

```
Access-Control-Allow-Origin: https://your-aero-origin.example
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since
```

*Disk bytes (`GET`/`HEAD`) response*:

```
Access-Control-Allow-Origin: https://your-aero-origin.example
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Last-Modified
```

Notes:

- `Access-Control-Allow-Origin: *` is acceptable for public, non-credentialed access.
- `Content-Range` is not a “simple” header, so it must be **exposed** if the UI needs to read it.
  - This matters for HTTP Range mode. Chunked mode does not read `Content-Range`.

## Streaming images: caching behavior

The streaming backend downloads data in fixed-size **blocks** (default: **1 MiB** for HTTP Range mode, or **4 MiB** chunks for the chunked format).

- On a cache miss, Aero fetches the required block/chunk from the remote server (Range `GET` or chunk `GET`).
- Blocks/chunks are stored locally and reused on subsequent runs.
- A cache size limit can be configured; when exceeded, least-recently-used blocks/chunks are evicted.

### Mode B: Chunked manifest (no `Range`)

Chunked streaming avoids `Range` requests entirely (and therefore can avoid CORS preflight in many deployments):

- You host a `manifest.json` plus fixed-size `chunks/00000000.bin`, `chunks/00000001.bin`, etc.
- The client reads the manifest once, then fetches chunks with plain `GET`.

See the full format spec: [`18-chunked-disk-image-format.md`](./18-chunked-disk-image-format.md).

## Inspecting streaming performance (telemetry + controls)

The dev UI includes a **Remote disk image (streaming)** panel that can open a remote disk via the runtime disk worker and display live stats:

- total image size
- cached bytes + configured cache limit
- cache hit rate
- bytes downloaded + request counts
- outstanding in-flight fetches

It also provides buttons to **flush metadata**, **clear cache**, and **close** the streaming handle so you can tune block/chunk sizing and cache limits for 20GB+ boot scenarios.

Additional tuning knobs:

- **Credentials mode** (`same-origin` / `include` / `omit`) for cookie-auth / credentialed CORS setups.
- **Cache key override** (`cacheImageId`, `cacheVersion`) so you can pin cache identity separately from the URL
  (useful when URLs include ephemeral auth query params or when you want to force a cache bust by bumping a version).
- **Reset stats**: records a baseline snapshot and shows deltas for cumulative counters without clearing the cache.
- **Settings persistence**: the panel stores its last-used options in `localStorage` (URLs are stored without query/hash).
- **Cache limit**: set a positive MiB value to enable LRU eviction; set `0` to disable eviction (unbounded cache growth).

## Security / UX expectations

Remote image support should be gated behind explicit user action:

- A dedicated “Use remote image” toggle (off by default).
- A URL input field.
- A clear warning that remote images can be untrusted and may leak request metadata to the host.
- A cache/progress indicator (downloaded blocks, cache size, etc.).

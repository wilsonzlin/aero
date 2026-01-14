# 18 - Chunked Disk Image Format (No-Range HTTP Delivery)

## Overview

When streaming a large disk image into the browser, the most natural approach is `Range: bytes=...` reads against a single object. In practice, `Range` creates two recurring deployment problems:

1. **CORS preflight:** `Range` is not a CORS-safelisted request header, so cross-origin `fetch()` of ranges triggers an `OPTIONS` preflight. This adds latency and can fail in environments where preflights are blocked or poorly cached.
2. **CDN/object-store limitations:** many CDNs do not cache `206 Partial Content` responses well, vary cache keys in surprising ways, impose size/offset limits, or require special configuration to forward `Range` at all.
   - See: [17 - HTTP Range + CDN Behavior](./17-range-cdn-behavior.md) (CloudFront/Cloudflare behavior and size limits).

This document specifies a complete alternative delivery format for *read-only base images*: store the disk image as many fixed-size **chunk objects** plus a small **manifest**. Clients then fetch data using only plain `GET` requests (no `Range` header), which improves CDN compatibility and avoids CORS preflight.

This format is designed to back the `StreamingDisk` concept described in [05 - Storage Subsystem](./05-storage-subsystem.md#image-download-and-streaming).

For how disk images are uploaded/imported and kept private in a hosted service (ownership/sharing/writeback strategies), see: [Disk Image Lifecycle and Access Control](./17-disk-image-lifecycle-and-access-control.md).

For auth/CORS/COOP/COEP guidance that also applies to chunk manifests and chunk objects (even though this format avoids `Range`), see: [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md).

---

## Goals / Non-goals

### Goals

- Enable random-access reads of large disk images using only `GET` requests.
- Allow aggressive CDN caching (`immutable`) for both chunks and manifest.
- Avoid requiring `LIST` operations on object stores (manifest-driven addressing only).
- Keep the format simple enough to implement in any language.

### Non-goals

- This format does **not** support remote writes. Writes still go to a local overlay
  (typically an OPFS-backed sparse file). IndexedDB-based caches are async-only and do not
  currently back the synchronous Rust disk/controller path; see
  [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) and the canonical trait mapping
  in [`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).
- This format does **not** attempt to deduplicate across images (that is a separate concern).

---

## Authorization and privacy (hosted service)

Chunked delivery changes *how* bytes are fetched (plain `GET` of chunk objects), but it does not change the fact that:

- Private per-user images must remain private.
- A crossOriginIsolated app still needs COEP-compatible responses.
- The browser must be able to stream bytes without leaking long-lived credentials.

### What must be protected

For a private image, treat all of these as sensitive:

- `manifest.json`
- every chunk object under `chunks/`

The manifest is effectively an index of where to fetch bytes from; if a private manifest is publicly readable, the chunks must not be, and vice versa.

### How to authorize chunk reads without reintroducing preflight

The main goal of chunked delivery is to avoid `Range` preflights, so prefer auth mechanisms that do not require non-safelisted headers:

- **Same-origin + session cookies** (no CORS, no preflight): best when you can serve chunks from the same origin as the app.
- **Signed URLs / signed cookies at the CDN**: allows cross-origin delivery while keeping requests as “simple” `GET`s.

Avoid relying on `Authorization: Bearer ...` for cross-origin chunk fetches if you are trying to eliminate preflights, because `Authorization` is not CORS-safelisted and will trigger preflight even without `Range`.

### CORS and COEP notes

- Cross-origin chunk fetches still require correct CORS headers if the bytes are read by JavaScript (which they are for `StreamingDisk`).
- If the app uses `COEP: require-corp` to enable `SharedArrayBuffer`, chunk and manifest responses must be COEP-compatible (CORS-enabled and/or appropriate `Cross-Origin-Resource-Policy`), just like a Range-based disk bytes endpoint.

---

## 1) Format Specification

### 1.1 Chunk size

The disk image is split into fixed-size chunks:

- `chunkSize` is **configurable** per image/version.
- `chunkSize` **MUST** be > 0.
- `chunkSize` **MUST** be a multiple of 512 bytes (ATA sector size).
- `totalSize` **MUST** be > 0.
- `totalSize` **MUST** be a multiple of 512 bytes (disk images are sector-addressed).
- Default: **4 MiB** (`4 * 1024 * 1024`) for a good balance of request count vs. over-fetch (this is also the default used by `aero-image-chunker`).
- Larger values (e.g. **8 MiB**) can be reasonable if you expect predominantly sequential reads and want fewer requests/objects.

Terminology:

- `chunkIndex` is **0-based**.
- All chunks except the final chunk have size exactly `chunkSize`.
- The final chunk has size `totalSize - chunkSize * (chunkCount - 1)` (may be smaller).

### 1.2 Object naming scheme

Chunks and manifest are stored under a stable prefix that includes a version identifier.

Recommended layout (path-style; works on S3/GCS/R2/CDNs):

```
images/<imageId>/<version>/
  manifest.json
  chunks/
    00000000.bin
    00000001.bin
    ...
```

Where:

- `<imageId>` is a stable identifier (e.g., `win7-sp1-x64`, `windows7-base`, etc).
- `<version>` is a content-addressed or otherwise immutable identifier (see §1.4).
- Chunk filenames are `chunkIndex` formatted as **zero-padded decimal**, followed by `.bin`.
  - `chunkIndexWidth` is recommended to be **8** (up to 99,999,999 chunks).
  - `chunkIndexWidth` **MUST** be large enough to represent `chunkCount - 1` without truncation:
    - `chunkIndexWidth >= len(str(chunkCount - 1))`
    - To minimize padding, set `chunkIndexWidth` to this minimum value (e.g. `chunkCount=100` ⇒ `chunkIndexWidth=2` ⇒ `00.bin`…`99.bin`).
  - Larger fixed widths (e.g. 8) are recommended for predictable URL formatting and for lexicographic ordering in tooling.

Example URLs:

```
https://cdn.example.com/images/win7-sp1-x64/sha256-4b3c.../manifest.json
https://cdn.example.com/images/win7-sp1-x64/sha256-4b3c.../chunks/0000002a.bin
```

### 1.3 Manifest JSON schema

The manifest is a single JSON document that describes how to fetch chunks.

It **MUST** include:

- `totalSize` (bytes, integer)
- `chunkSize` (bytes, integer)
- `chunkCount` (integer)
- `version` (string; opaque to the client)
- `mimeType` (string; for chunk objects)

It **MAY** include:

- per-chunk `sha256` (hex string) for integrity checking
- per-chunk `size` (bytes) to make final-chunk sizing explicit (recommended)

Recommended schema (v1):

```json
{
  "schema": "aero.chunked-disk-image.v1",
  "imageId": "win7-sp1-x64",
  "version": "sha256-4b3c3d8b0a2c4f5b...",
  "mimeType": "application/octet-stream",
  "totalSize": 32212254720,
  "chunkSize": 4194304,
  "chunkCount": 7680,
  "chunkIndexWidth": 8,
  "chunks": [
    { "size": 4194304, "sha256": "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae" },
    { "size": 4194304, "sha256": "..." },
    { "size": 1234567, "sha256": "..." }
  ]
}
```

Notes:

- `chunks.length` **MUST** equal `chunkCount` if present.
- `chunkCount` **MUST** be > 0.
- `chunkIndexWidth` **MUST** be > 0.
- If `chunks` is present, each entry may omit `size` and/or `sha256`:
  - missing `size` defaults to `chunkSize` (or the derived final chunk size for the last chunk)
  - missing `sha256` means “no integrity check for this chunk”
- If `chunks` is omitted, the client assumes all chunks are size `chunkSize` except the last (derived from `totalSize`), and no per-chunk checksums.
- `totalSize` is the size in bytes of the **logical disk byte stream** (what the guest sees / `VirtualDisk::capacity_bytes()`).
  - For raw disk images, this is typically the same as the input file length.
  - For sparse/container formats (qcow2/VHD/AeroSparse), this may differ from the on-disk container file size.
- `sha256` is optional to reduce manifest size and hashing cost; it is strongly recommended when serving from untrusted infrastructure.
- Implementations should apply reasonable defensive limits when handling untrusted manifests (e.g.
  bounds on `chunkSize`, `chunkCount`, `chunkIndexWidth`, and the manifest JSON size). Aero’s
  reference clients currently enforce:
  - `chunkSize <= 64 MiB`
  - `chunkCount <= 500,000`
  - `chunkIndexWidth <= 32`
  - manifest JSON size `<= 64 MiB`

### 1.4 Versioning / immutability

To safely apply long-lived caching (`immutable`), chunk URLs must never change content.

Recommended approach:

- `version` is derived from the *entire* logical disk byte stream content (e.g., `sha256-<digest>` over the same bytes that will be served in chunks).
- All objects are stored under `images/<imageId>/<version>/...`.

This ensures:

- Changing the disk image produces a new version prefix (no cache poisoning).
- Old versions remain valid and cacheable indefinitely.

If you need a stable “latest” pointer, publish a separate small JSON file (e.g., `images/<imageId>/latest.json`) with a short TTL that points to the latest versioned `manifest.json`. Clients should then fetch the versioned manifest and only cache that aggressively.

### 1.5 Mapping byte offset → chunk index + in-chunk offset

Given:

- `chunkSize` (bytes)
- a byte offset `pos` (0 ≤ `pos` < `totalSize`)

Compute:

```
chunkIndex      = floor(pos / chunkSize)
offsetInChunk   = pos % chunkSize
```

To cover a requested range `[start, end)` (end exclusive):

```
firstChunk = floor(start / chunkSize)
lastChunk  = floor((end - 1) / chunkSize)   // if end > start
```

The client fetches all chunks in `[firstChunk, lastChunk]` and then slices out the requested bytes from the concatenated data (or slices per chunk).

---

## 2) Caching + CORS

### 2.1 Why plain `GET` avoids CORS preflight

Browsers only send an automatic CORS preflight (`OPTIONS`) when the request is not a “simple request”.

`Range` is **not** a CORS-safelisted request header, so `fetch(url, { headers: { Range: ... } })` becomes a non-simple request and triggers preflight on cross-origin URLs.

With the chunked format:

- Each chunk is fetched with a normal `GET` to a unique URL
- The client does **not** send a `Range` header (or other non-safelisted headers)

This keeps requests “simple” and avoids preflight in the common case.

> Note: You still need standard CORS response headers (e.g., `Access-Control-Allow-Origin`) when fetching cross-origin. This format specifically avoids the *preflight*, not CORS itself.

### 2.2 Recommended HTTP headers

Because chunk URLs are versioned and immutable, they should be cached very aggressively:

**Chunks (`*.bin`):**

- `Content-Type: application/octet-stream`
- `Content-Encoding: identity` (or omit the header; required: avoid transparent compression)
  - Aero’s reference clients and tooling treat any non-identity `Content-Encoding` as a protocol error (i.e. do not gzip/br-encode chunks in transit).
- `Cache-Control: public, max-age=31536000, immutable, no-transform`
  - Aero’s reference clients treat missing `Cache-Control` / missing `no-transform` as a protocol error (defence-in-depth against intermediary transforms that would break byte-addressed reads).
- `ETag: "<strong etag>"` (optional but recommended; quoted entity-tag, visible ASCII)
- `Access-Control-Allow-Origin: *` (if served cross-origin without credentials)
- `Access-Control-Expose-Headers: Content-Encoding` (recommended when serving cross-origin and you include `Content-Encoding`, so browser JS can detect non-identity encodings)

**Manifest (`manifest.json`):**

- `Content-Type: application/json`
- `Content-Encoding: identity` (or omit the header; required for compatibility with Aero’s reference clients + tooling)
- `Cache-Control: public, max-age=31536000, immutable, no-transform` (when versioned/immutable as described above)
  - Aero’s reference clients treat missing `Cache-Control` / missing `no-transform` as a protocol error.
- `ETag: "<strong etag>"` (optional; quoted entity-tag, visible ASCII)
- `Access-Control-Allow-Origin: *` (same policy as chunks)
- `Access-Control-Expose-Headers: Content-Encoding` (recommended when serving cross-origin and you include `Content-Encoding`)

---

## 3) Upload / Publish Pipeline (Server-side)

This section describes a high-level pipeline for taking an uploaded disk image and producing chunk objects + manifest.

This repo includes a reference publisher CLI at [`tools/image-chunker/`](../tools/image-chunker/) (`aero-image-chunker publish`) that implements this pipeline for S3-compatible object stores (AWS S3, MinIO, etc).

For CI and deployment validation, the same tool also provides `aero-image-chunker verify` to
re-download a published `manifest.json` + `chunks/*.bin` and validate schema, chunk sizes, and
optional per-chunk checksums end-to-end (supports S3-backed verification, direct HTTP verification
via `--manifest-url` for public/CDN-hosted images, and local verification via `--manifest-file`).

Verification is **fail-fast**: it stops on the first mismatch and reports it. For quick smoke
checks, use chunk sampling (`--chunk-sample N`) to verify a handful of random chunks plus the final
chunk.

Note on input formats:

- The chunked format is defined in terms of the **logical disk byte stream** (what the guest sees),
  not the bytes of any particular container file format.
- `aero-image-chunker` defaults to `--format auto` (format detection) and treats unknown images as
  `raw` (treat the input file bytes as the logical disk bytes). It can also open other formats
  (e.g. qcow2/VHD/AeroSparse) via `--format` and publish the expanded logical disk view.
- Images that require an explicit parent (QCOW2 backing files, VHD differencing) should be flattened
  to a standalone image before chunking.

### 3.1 Pipeline steps

1. **Ingest disk image**
    - Accept an uploaded image (raw or already-prepared format).
    - Determine `totalSize` (bytes of the logical disk byte stream / disk capacity).

2. **Choose chunk parameters**
   - Select `chunkSize` (configurable; default 4 MiB).
   - Compute `chunkCount = ceil(totalSize / chunkSize)`.
   - Choose `chunkIndexWidth` (recommend 8).

3. **Split into chunks + hash**
    - Stream-read the logical disk byte stream sequentially.
    - For each `chunkIndex`:
      - Read `min(chunkSize, remainingBytes)` into a buffer.
      - Optionally compute `sha256` over the chunk bytes.
      - Record `{ size, sha256? }` in an in-memory manifest structure.

4. **Upload chunks (parallel)**
   - Upload each chunk as an independent object at the computed name.
   - Use parallelism (bounded concurrency) to increase throughput.
   - For object stores like S3, use multipart upload where it helps (e.g., large chunk sizes, high latency links), but note that each chunk is already independently addressable; multipart is an implementation detail.

5. **Write and upload manifest**
   - Compute `version` (recommended: hash of full disk image, or hash of manifest).
   - Upload `manifest.json` under the same `<version>` prefix.
   - Set caching and CORS headers.

6. **(Optional) Publish “latest” pointer**
   - Upload/update `images/<imageId>/latest.json` containing the new versioned manifest URL.
    - Give this pointer a short TTL (e.g., `Cache-Control: public, max-age=60, no-transform`).

### 3.2 Failure handling / atomicity

Recommended publish semantics:

- Upload all chunks first.
- Upload the versioned manifest last.

Clients should treat the manifest as authoritative: if the manifest exists, all chunks referenced by `chunkCount` are expected to exist.

---

## 4) Client Integration Guidance

This repo includes a reference browser implementation:

- `web/src/storage/remote_chunked_disk.ts` (`RemoteChunkedDisk`)
  - Supports persistent caching (OPFS when available, otherwise an in-memory test store).
  - Exposes lightweight telemetry via `getTelemetrySnapshot()` (hits/misses, bytes downloaded, inflight fetches).

### 4.1 `StreamingDisk`: API changes

In the `StreamingDisk` example in [05 - Storage Subsystem](./05-storage-subsystem.md#image-download-and-streaming), the remote path currently uses:

- `fetch_range(start, end)` with `Range: bytes=start-end`

With chunked delivery, replace that with:

- `fetch_chunk(chunkIndex)` (plain `GET`)

The client should:

1. `GET manifest.json` once on startup
2. Cache `chunkSize`, `chunkCount`, and URL prefix
3. Serve `read_sectors()` by fetching and caching whole chunks

### 4.2 Internal caching alignment

Even without HTTP Range, the internal caching strategy remains the same:

- Align fetches to chunk boundaries (as in the existing `chunk_start/chunk_end` logic).
- Store fetched chunks in the local cache at offset `chunkIndex * chunkSize`.
- Track downloaded content in **chunk units** (e.g., a bitset or a `HashSet<u32>` of downloaded chunk indexes), instead of arbitrary byte ranges.

Suggested pseudo-logic:

```rust
let pos = lba * 512;
let end = pos + buffer.len() as u64;

let first_chunk = pos / chunk_size;
let last_chunk = (end - 1) / chunk_size;

for idx in first_chunk..=last_chunk {
    ensure_chunk_cached(idx).await?;
}

// Read requested bytes from local cache file.
local_cache.read_at(pos, buffer)?;
```

### 4.3 Optional integrity checking

If the manifest includes `sha256` per chunk, clients can verify integrity before writing to local cache:

- In browsers: use WebCrypto `crypto.subtle.digest("SHA-256", bytes)`.
- Reject mismatched chunks and retry (or fail loudly).

This is especially useful when chunks are served from multiple CDNs or via untrusted mirrors.

---

## 5) Tradeoffs

### 5.1 Request count vs. wasted bytes

Compared to HTTP Range:

- **More requests:** random I/O can trigger many chunk `GET`s. HTTP/2 and HTTP/3 mitigate head-of-line blocking, but request overhead still exists.
- **Over-fetch:** reading 4 KiB of data can require fetching a full multi-megabyte chunk.

Mitigations:

- Tune `chunkSize` for your expected access pattern.
- Implement read-ahead (prefetch the next N chunks on sequential access).
- Keep a local persistent cache so each chunk is fetched at most once per user.

### 5.2 Compression options

Possible approaches:

- **No compression (recommended default):** simplest; best for random access and CPU usage.
- **Per-chunk compression:** compress each chunk independently (e.g., gzip/zstd) so random access still works.
  - Note: Aero’s reference clients currently require `Content-Encoding` to be absent/`identity` for both `manifest.json` and chunk objects, so HTTP `Content-Encoding`-based compression is not compatible with the current ecosystem.
  - If storing custom-compressed bytes, the client must decompress in JS/WASM before use.

Because disk images often contain already-compressed data and need random access, compression may offer limited wins; measure before adopting.

### 5.3 Storage/object count overhead

Storing many chunk objects increases:

- object count (storage overhead, per-object request billing)
- upload time (many PUTs)

However, it avoids operational issues with Range and enables simpler CDN caching behavior.

Important: avoid object-store `LIST` operations:

- Clients should never enumerate chunks via listing.
- The manifest’s `chunkCount` plus the naming convention is sufficient to compute every chunk URL deterministically.

---

## Appendix: Minimal Example Manifest + URLs

Assume:

- `imageId = "demo"`
- `version = "sha256-acde..."`
- `chunkSize = 4` bytes (tiny for the example)
- `totalSize = 10` bytes ⇒ `chunkCount = 3`

Manifest URL:

```
GET https://cdn.example.com/images/demo/sha256-acde.../manifest.json
```

Chunk URLs:

```
GET https://cdn.example.com/images/demo/sha256-acde.../chunks/00000000.bin  // bytes 0..4
GET https://cdn.example.com/images/demo/sha256-acde.../chunks/00000001.bin  // bytes 4..8
GET https://cdn.example.com/images/demo/sha256-acde.../chunks/00000002.bin  // bytes 8..10 (final, smaller)
```

Example manifest:

```json
{
  "schema": "aero.chunked-disk-image.v1",
  "imageId": "demo",
  "version": "sha256-acde...",
  "mimeType": "application/octet-stream",
  "totalSize": 10,
  "chunkSize": 4,
  "chunkCount": 3,
  "chunkIndexWidth": 8,
  "chunks": [
    { "size": 4, "sha256": "..." },
    { "size": 4, "sha256": "..." },
    { "size": 2, "sha256": "..." }
  ]
}
```

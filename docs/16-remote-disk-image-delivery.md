# 16 - Remote Disk Image Delivery (Object Store + CDN + HTTP Range)

## Overview

Aero supports **streaming 20GB+ disk images** into the browser without downloading the whole file up front. The core idea is:

- The browser exposes a block-device-like API (`StreamingDisk`) that performs **random-access reads**.
- Reads are satisfied by issuing `GET` requests with an `HTTP Range` header against an immutable disk image stored in an **S3-compatible object store**.
- A **CDN** (CloudFront is the primary reference) sits in front of the object store to cache hot ranges and reduce origin load.
- Downloaded ranges are persisted locally (OPFS) so repeated reads stop hitting the network.

Implementation note: in Rust (native/non-wasm32), the streaming-disk implementation and cache
traits live under `crates/aero-storage` (e.g. `aero_storage::StreamingDisk` /
`aero_storage::ChunkStore`).
For the repo-wide canonical disk/backend trait mapping, see
[`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

This document defines the **production contract** for the `remote_url` used by `StreamingDisk` and provides deployment guidance (caching, CORS, security).

For the normative auth + CORS + COOP/COEP behavior of the disk bytes endpoint, see: [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md).

If you want a CDN-friendlier alternative that avoids `Range` (and therefore avoids `Range`-triggered CORS preflight in cross-origin deployments), see: [Chunked Disk Image Format](./18-chunked-disk-image-format.md).

## Reference infrastructure in this repo

- Local development (MinIO + optional reverse proxy for edge/CORS emulation):
  - [`infra/local-object-store/README.md`](../infra/local-object-store/README.md)
- AWS production reference (S3 + CloudFront tuned for Range + CORS):
  - [`infra/aws-s3-cloudfront-range/README.md`](../infra/aws-s3-cloudfront-range/README.md)
- Reference backend for user-uploaded/private images (S3 multipart upload + CloudFront signed access):
  - [`services/image-gateway/`](../services/image-gateway/)

Companion tools:

- Correctness + CORS conformance checks: [`tools/disk-streaming-conformance/`](../tools/disk-streaming-conformance/README.md)
- Range throughput + CDN cache probing (`X-Cache`): [`tools/range-harness/`](../tools/range-harness/README.md)
- Chunked disk publisher + verifier (no-`Range` delivery): [`tools/image-chunker/`](../tools/image-chunker/README.md)
  - `publish --format <raw|qcow2|vhd|aerosparse|auto>` (alias: `aerospar`): publish the logical disk byte stream for common formats.
  - `verify`: validate a published `manifest.json` + `chunks/*.bin` end-to-end (supports S3-backed verification, direct HTTP via `--manifest-url`, and local verification via `--manifest-file`).

---

## End-to-end architecture

```
┌───────────────────────────────────────────────────────────────────────────┐
│ Browser (Aero)                                                            │
│                                                                           │
│  Storage stack:                                                          │
│   VirtualDrive → StreamingDisk → (OPFS AEROSPAR sparse cache)             │
│                                                                           │
│  Network behavior:                                                       │
│   1) HEAD remote_url  -> discover Content-Length + ETag/Last-Modified     │
│   2) GET Range bytes=a-b -> fetch aligned CHUNK_SIZE blocks as needed     │
│                                                                           │
└───────────────────────────────┬───────────────────────────────────────────┘
                                │ HTTPS (GET/HEAD/OPTIONS)
                                ▼
┌───────────────────────────────────────────────────────────────────────────┐
│ CDN (CloudFront / equivalent)                                             │
│                                                                           │
│ - Optionally enforces authorization (signed cookies/URLs, JWT at edge, …) │
│ - Caches immutable objects and/or aligned byte ranges                      │
│ - Forwards Range requests to origin                                        │
└───────────────────────────────┬───────────────────────────────────────────┘
                                │ Origin fetch (Range-aware)
                                ▼
┌───────────────────────────────────────────────────────────────────────────┐
│ Object store (S3-compatible)                                              │
│                                                                           │
│ - Stores disk image blobs (20GB+)                                          │
│ - Must support HEAD and Range GET (206 + Content-Range)                    │
│ - Provides ETag/Last-Modified for cache validation                         │
└───────────────────────────────────────────────────────────────────────────┘
```

Key design principle: **the disk image URL should be stable and immutable** (versioned path). This enables aggressive CDN caching and makes local OPFS caches safe.

Implementation note: in the current browser stack, the OPFS cache format is Aero sparse (`AEROSPAR`)
as implemented by [`web/src/storage/opfs_sparse.ts`](../web/src/storage/opfs_sparse.ts).

---

## Deployment modes

### Mode A: Public/shared images (demo/test OS images)

Use this for images that are identical for many users (e.g., a Windows 7 demo image).

Goals:

- Maximize cache sharing across users.
- Keep URLs stable and cacheable for a long time.
- Avoid any user-specific cache keys.

Recommended characteristics:

- Immutable, versioned key (e.g. `/images/win7-sp1-x64/2026-01-10/disk.img`)
- `Cache-Control: public, max-age=31536000, immutable, no-transform`
- No authorization required (or optional “soft gating” on the app side; see below)

### Mode B: Private per-user images (user uploads)

Use this for images that must not be shared across users.

For the full hosted-service model (upload/import flows, ownership/visibility/sharing, and writeback strategies), see: [Disk Image Lifecycle and Access Control](./17-disk-image-lifecycle-and-access-control.md).

Goals:

- Strong access control (only the owning user can fetch ranges).
- Prevent URL guessing from becoming a data leak.
- Still leverage CDN performance features (TLS, edge POPs, origin shielding), even if cache sharing is minimal.

Recommended characteristics:

- Objects stored under a per-user prefix, but **authorization must still be enforced**:
  - `/users/<userId>/images/<imageId>/<version>/disk.img`
- Authorization enforced at the CDN (signed cookies/URLs, or an edge auth layer).
- Conservative caching (often short TTL), unless you intentionally want CDN caching for repeat access.

> Note: “Private” does not necessarily mean “uncacheable”.
>
> If the CDN enforces access (e.g., CloudFront signed cookies), it is still safe to use
> `Cache-Control: public` (ideally with `no-transform`) on a private object. The CDN will store bytes, but only serve them
> to authorized viewers. This is useful for **private-but-shared** images (e.g., paid tier),
> and can still be acceptable for per-user images if your threat model allows it.

---

## Why HTTP Range (and why fixed chunk alignment matters)

### Random access is required

Windows will perform a lot of small reads spread across the disk image:

- filesystem metadata (NTFS MFT, directories)
- pagefile reads/writes (depending on configuration)
- DLL and executable paging
- registry hive access
- boot-time file access patterns

Downloading a 20–40GB disk image before boot is not viable. `StreamingDisk` therefore reads only the bytes it needs, on demand.

### `StreamingDisk` uses fixed-size aligned chunks

`StreamingDisk` maps arbitrary reads into **aligned fixed-size chunks** (see `CHUNK_SIZE` in the storage doc). Benefits:

- Improves CDN cache hit rate (many clients will request identical ranges).
- Reduces request fan-out by batching adjacent reads.
- Keeps local storage simple (direct mapping to the local sparse-cache block size, i.e. `AEROSPAR`
  blocks).

### Recommended default `CHUNK_SIZE`

Recommended starting point:

- `CHUNK_SIZE = 1 MiB` (1,048,576 bytes)

Rationale:

- Small enough to avoid excessive over-fetch on random reads.
- Large enough that request overhead (headers, latency, TLS, CDN processing) is amortized.
- Aligns well with `AEROSPAR` block sizes that are typically ~1 MiB.
- Creates a manageable number of total chunks:
  - 20 GiB image ≈ 20,480 chunks
  - 40 GiB image ≈ 40,960 chunks

### Chunk size tuning

`CHUNK_SIZE` is a trade-off between request rate and over-fetch:

| If you choose… | You get… | You risk… |
|---|---|---|
| Smaller chunks (128–512 KiB) | Less wasted bandwidth on scattered reads | Higher request rate, higher CDN/S3 request costs, more CPU overhead |
| Larger chunks (2–8 MiB) | Fewer requests, better throughput | More wasted bandwidth, larger OPFS footprint, slower “first useful byte” |

Operational guidance:

- Start at **1 MiB**.
- If you see high request rate / high RTT overhead (especially on mobile), try **2 MiB**.
- If you see significant wasted transfer vs. useful bytes (many “cold” reads), try **512 KiB**.
- Keep `CHUNK_SIZE` a power-of-two and a multiple of 512 bytes.

---

## HTTP semantics: required behavior

### HEAD for size discovery and versioning

Before range reads, `StreamingDisk` needs the object size and a stable validator (ETag/Last-Modified).

Requirements:

- `HEAD <remote_url>` must return:
  - `200 OK`
  - `Content-Length: <total-bytes>`
  - `Accept-Ranges: bytes` (or `Accept-Ranges: bytes` on subsequent GETs)
  - `ETag` and/or `Last-Modified`

Example:

```http
HEAD /images/win7/2026-01-10/disk.img HTTP/1.1
Host: aero.example.com
```

```http
HTTP/1.1 200 OK
Content-Length: 21474836480
Accept-Ranges: bytes
ETag: "2f8c3f2a0a4d9b0b0f..."
Last-Modified: Fri, 10 Jan 2026 00:00:00 GMT
Cache-Control: public, max-age=31536000, immutable, no-transform
```

How Aero should use it (conceptually):

- Use `Content-Length` as `total_size`.
- Use `ETag`/`Last-Modified` as a cache key for OPFS:
  - If the validator changes, the local cached chunks must be treated as stale.

### GET with Range

For chunk fetches, the browser sends:

```http
GET /images/win7/2026-01-10/disk.img HTTP/1.1
Host: aero.example.com
Range: bytes=1048576-2097151
```

Required origin/CDN behavior:

- Respond with `206 Partial Content`
- Include `Content-Range` describing the served range and total size
- Include `Content-Length` equal to the returned byte count
- Include `Accept-Ranges: bytes`
- Include `Cache-Control` containing `no-transform` (Aero clients enforce this to prevent intermediary transforms)

Example response:

```http
HTTP/1.1 206 Partial Content
Content-Type: application/octet-stream
Accept-Ranges: bytes
Content-Range: bytes 1048576-2097151/21474836480
Content-Length: 1048576
Cache-Control: public, max-age=31536000, immutable, no-transform
ETag: "2f8c3f2a0a4d9b0b0f..."
Last-Modified: Fri, 10 Jan 2026 00:00:00 GMT
```

Edge cases:

- If the requested range starts at or beyond the end of the file:
  - Respond with `416 Range Not Satisfiable`
  - Include: `Content-Range: bytes */<total-size>`

```http
HTTP/1.1 416 Range Not Satisfiable
Content-Range: bytes */21474836480
```

> Aero should never intentionally request invalid ranges, but correctness here matters:
> bugs, race conditions, or stale metadata can otherwise degrade into silent data corruption.

### Cache validators (ETag / Last-Modified)

For immutable, versioned objects:

- `ETag` and `Last-Modified` should remain stable for the lifetime of that version.

For mutable objects (not recommended for disk images):

- Changing bytes must update `ETag` and/or `Last-Modified`.
- Clients and CDNs may revalidate using `If-None-Match` / `If-Modified-Since`.

Important caveat for S3:

- S3 `ETag` is **not guaranteed to be an MD5 checksum**:
  - Multipart uploads typically produce an ETag that encodes part count.
  - Some encryption modes and proxies can also affect ETag semantics.
- Treat `ETag` as a **version/validator**, not a cryptographic integrity hash.
  - If you need an integrity hash, store a separate SHA-256 in metadata or a manifest file.

---

## CORS and preflight (Range is not “simple”)

### Why `Range` causes a preflight

When `remote_url` is cross-origin relative to the web app, the browser will send an `OPTIONS` preflight because:

- `Range` is not a “simple” request header under the Fetch/CORS rules.

Without mitigation, that means each disk chunk fetch can be preceded by an `OPTIONS` request, which is catastrophic for performance.

### Mitigations (preferred order)

1. **Same-origin hosting (preferred)**
   - Serve the disk image from the same origin as the app (same scheme/host/port).
   - This avoids CORS entirely and eliminates preflights.
   - With CloudFront, this usually means: one distribution + one domain, with path-based routing.
2. **Aggressive preflight caching**
   - Configure `Access-Control-Max-Age` to a large value (e.g., 86400 seconds).
   - Real-world browsers cap this, but it still helps dramatically.
3. **Chunk-object alternative (if preflight remains a problem)**
   - Pre-split the disk image into fixed-size objects (`chunk_000000`, `chunk_000001`, …).
   - Fetch chunks with plain `GET` (no `Range` header) to keep requests “simple” cross-origin.
   - Costs: many objects, a manifest, more operational complexity.

### S3 CORS configuration (copy-paste)

If you must fetch cross-origin from S3 (directly or via a CDN that forwards preflights), configure CORS so that:

- `GET`, `HEAD`, and `OPTIONS` are allowed
- request header `Range` is allowed
- response headers needed by Aero are exposed:
  - `Content-Range`, `Accept-Ranges`, `Content-Length`, `ETag`, `Last-Modified`

#### S3 CORS XML example

```xml
<?xml version="1.0" encoding="UTF-8"?>
<CORSConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <CORSRule>
    <AllowedOrigin>https://aero.example.com</AllowedOrigin>
    <AllowedMethod>GET</AllowedMethod>
    <AllowedMethod>HEAD</AllowedMethod>
    <AllowedMethod>OPTIONS</AllowedMethod>
    <AllowedHeader>Range</AllowedHeader>
    <AllowedHeader>If-Range</AllowedHeader>
    <AllowedHeader>If-None-Match</AllowedHeader>
    <AllowedHeader>If-Modified-Since</AllowedHeader>
    <AllowedHeader>Origin</AllowedHeader>
    <AllowedHeader>Access-Control-Request-Method</AllowedHeader>
    <AllowedHeader>Access-Control-Request-Headers</AllowedHeader>
    <ExposeHeader>Accept-Ranges</ExposeHeader>
    <ExposeHeader>Content-Range</ExposeHeader>
    <ExposeHeader>Content-Length</ExposeHeader>
    <ExposeHeader>Content-Encoding</ExposeHeader>
    <ExposeHeader>ETag</ExposeHeader>
    <ExposeHeader>Last-Modified</ExposeHeader>
    <MaxAgeSeconds>86400</MaxAgeSeconds>
  </CORSRule>
</CORSConfiguration>
```

#### S3 CORS JSON example (AWS CLI / IaC friendly)

```json
{
  "CORSRules": [
    {
      "AllowedOrigins": ["https://aero.example.com"],
      "AllowedMethods": ["GET", "HEAD", "OPTIONS"],
      "AllowedHeaders": [
        "Range",
        "If-Range",
        "If-None-Match",
        "If-Modified-Since",
        "Origin",
        "Access-Control-Request-Method",
        "Access-Control-Request-Headers"
      ],
      "ExposeHeaders": [
        "Accept-Ranges",
        "Content-Range",
        "Content-Length",
        "Content-Encoding",
        "ETag",
        "Last-Modified"
      ],
      "MaxAgeSeconds": 86400
    }
  ]
}
```

Notes:

- If you need multiple app origins (staging + prod), add multiple `AllowedOrigin` entries or multiple rules.
- If you use `fetch(..., { credentials: "include" })`, you cannot use `Access-Control-Allow-Origin: *` and must use an explicit origin.

---

## CDN caching strategy (CloudFront reference implementation)

### Baseline: CloudFront in front of a private S3 bucket (OAC)

Recommended baseline even for “public” images:

- Keep the S3 bucket private (Block Public Access).
- Serve images only through CloudFront.
- Use **Origin Access Control (OAC)** so CloudFront can read from the bucket, but the public internet cannot.

Benefits:

- Avoids direct-to-S3 hotlinking and accidental public exposure.
- Lets you enforce consistent caching/CORS headers at the CDN layer.

### Signed cookies vs signed URLs (private access)

CloudFront offers two viewer authorization mechanisms that work well for range-based streaming:

- **Signed cookies**
  - Best when the app and disk image share the same origin.
  - Keeps the disk URL stable and cacheable.
  - Authorization is attached to the browser session instead of each URL.
- **Signed URLs**
  - Useful for one-off sharing or when you can’t/won’t set cookies.
  - Signatures are usually query parameters.
  - Important: do **not** include user-specific query parameters in the cache key, or cache sharing is destroyed.

For both mechanisms:

- CloudFront still validates the signature/cookies on every request.
- Cached objects/ranges can be safely reused across authorized viewers if the object itself is not user-specific.

### Caching and 206 responses

Range fetches return `206 Partial Content`. Your CDN must handle these correctly:

- Forward the `Range` header to the origin.
- Cache must not confuse different ranges for the same object.

In CloudFront, keep the cache key minimal (path + version) and **do not** vary the cache key on `Range`.

- CloudFront supports byte-range requests and can satisfy subsequent ranges from its edge cache.
- Including `Range` in the cache key typically explodes cache cardinality for random access patterns.

If you run your own proxy cache (e.g., Nginx), use a **slice** strategy so the cache stores fixed-size slices and the cache key varies by slice range (not arbitrary viewer ranges). See: [17 - HTTP Range + CDN Behavior](./17-range-cdn-behavior.md).

### Recommended CloudFront behavior policies (outline)

You can implement this with CloudFront “Policies” (console) or in IaC.

**Cache Policy (for disk images)**

- Query strings: **none** (or only those that are stable and not user-specific)
- Cookies: **none**
- Headers in cache key:
  - (Optional) `Origin` if you serve CORS responses that vary by origin
- TTL:
  - For immutable objects: allow long TTLs (up to 1 year) and respect origin `Cache-Control`
  - For mutable objects: keep TTL low and rely on validators (`ETag`, `Last-Modified`)

**Origin Request Policy**

- Forward to origin:
  - `Range`
  - For CORS preflight: `Origin`, `Access-Control-Request-Method`, `Access-Control-Request-Headers`
- Do not forward unnecessary viewer headers (reduces cache fragmentation and origin load)

**Response Headers Policy**

- Ensure responses include:
  - `Access-Control-Allow-Origin` (if cross-origin; otherwise omit)
  - `Access-Control-Expose-Headers: Content-Range, Accept-Ranges, Content-Length, ETag, Content-Encoding`
    - Note: `Last-Modified` is CORS-safelisted and does not need explicit exposure.
  - `Access-Control-Max-Age` for preflights (if applicable)
- Add security headers as appropriate for your app.

Also verify:

- CloudFront behavior allows `GET`, `HEAD`, and `OPTIONS`.
- “Compress objects automatically” is **disabled** for disk images (or ensure the content-type will never be compressed).

### “Verify in practice” checklist

Use `curl` against the CDN URL (not the S3 origin) to confirm behavior:

1. HEAD works and returns size + validators:

   ```bash
   curl -I https://aero.example.com/images/win7/2026-01-10/disk.img
   ```

   Check for:

   - `200 OK`
   - `Content-Length`
   - `Accept-Ranges: bytes`
   - `ETag` and/or `Last-Modified`

2. Range GET works and returns 206 + Content-Range:

   ```bash
   curl -I -H 'Range: bytes=0-1048575' \
     https://aero.example.com/images/win7/2026-01-10/disk.img
   ```

   Check for:

   - `206 Partial Content`
   - `Content-Range: bytes 0-1048575/<total>`
   - `Content-Length: 1048576`

3. CDN caching is actually happening:

   Run the same request twice and compare `X-Cache`:

   - First request: `X-Cache: Miss from cloudfront` (expected)
   - Second request: `X-Cache: Hit from cloudfront` (desired)

   Also check `Age` increasing on repeated hits.

4. Cache key sanity:

   - Request two different ranges and ensure you don’t get the same bytes back.
   - Ensure query strings are not unintentionally fragmenting the cache.

5. Observe metrics:

   - CloudFront `CacheHitRate`
   - `BytesDownloadedFromOrigin` (should drop after warm-up for public/shared images)
   - Origin (S3) request counts

---

## Object layout and versioning

### Use immutable, versioned keys

Recommended key layout:

```
/images/<imageId>/<version>/disk.img
```

Examples:

- `/images/win7-sp1-x64/2026-01-10/disk.img`
- `/images/tiny-linux/2026-01-10/disk.img`

Why:

- Allows **infinite CDN TTLs** without worrying about stale bytes.
- Avoids cache invalidations/purges (which are slow and error-prone).
- Makes client-side OPFS caching safe and simple (validator changes only when version changes).

### Invalidation strategy

- Publish a new version by writing to a new path.
- Update your application’s “latest version” pointer (often a small JSON manifest with short TTL):

  - `/images/win7-sp1-x64/latest.json` (cache for seconds/minutes)
  - contents point to `/images/win7-sp1-x64/2026-01-10/disk.img`

Avoid:

- Overwriting `disk.img` in-place under a stable URL with long cache headers.
- Relying on CDN “purge” as the normal update mechanism.

---

## Recommended object metadata

Set these on the disk image object (or inject at the CDN):

- `Content-Type: application/octet-stream`
- `Cache-Control`:
  - Immutable versioned objects:
    - `Cache-Control: public, max-age=31536000, immutable, no-transform`
  - Mutable paths (not recommended for disk images):
    - `Cache-Control: no-cache, no-transform` (forces revalidation)
    - or a short TTL (`max-age=60, no-transform`) if you must

Strong warning:

- Do **not** serve disk images with `Content-Encoding: gzip/br` or any automatic compression.
  - Range requests operate on the encoded bytes. Compression changes byte offsets and breaks random-access semantics.
  - Ensure your CDN is not performing “helpful” compression based on incorrect `Content-Type`.

---

## Security and privacy guidance

Threats to consider:

- **Unauthorized guessing** of disk image URLs (especially for per-user objects)
- **Cache confusion** where an authorization token ends up in the cache key or a shared cache serves private data
- **Origin bypass** (direct S3 access) circumventing CDN auth
- **Data at rest exposure** (misconfigured buckets, backups, logs)

Recommended controls:

1. **Use a private bucket + OAC**
   - Block direct public access to S3.
   - Allow reads only from the CDN origin identity.
2. **Enforce auth at the CDN for private images**
   - CloudFront signed cookies/URLs, or an edge auth layer (JWT validation, etc).
3. **Do not put user identity or auth tokens into the cache key**
   - Avoid user-specific query params in cache key.
   - Avoid forwarding cookies/authorization headers into the cache key unless you intend per-user caching.
4. **Object key hygiene**
   - Use non-guessable IDs for `imageId` (UUIDs) even if you also enforce auth.
   - Keep per-user objects under distinct prefixes.
5. **Encrypt at rest**
   - S3 SSE-S3 or SSE-KMS (or equivalent in your object store).
6. **Audit logging**
   - Enable access logs (CloudFront standard logs or equivalent).
   - Monitor for unusual range-scraping patterns.

---

## Cost and performance considerations

### Request-rate vs chunk size

Range streaming trades bandwidth for requests:

- Requests per GiB ≈ `GiB / (CHUNK_SIZE in GiB)`
  - 1 MiB chunks: ~1024 requests per GiB
  - 4 MiB chunks: ~256 requests per GiB

Costs to watch:

- **Origin request costs** (e.g., S3 charges per GET/HEAD).
- **CDN request costs** (requests and bytes).
- **Cache fragmentation** if ranges are not aligned.

### S3/object-store request pricing (why alignment matters financially)

Object stores typically bill `GET` and `HEAD` per request. Range requests are usually billed as a normal `GET`.

For a rough order-of-magnitude estimate:

- With `CHUNK_SIZE = 1 MiB`, fetching 1 GiB of unique data is ~1024 range `GET`s (+ a small number of `HEAD`/`OPTIONS`).
- S3 Standard pricing is region-dependent but commonly on the order of **$0.0004 per 1,000 GET requests**.

The important part isn’t the exact number; it’s that:

- Misaligned ranges dramatically reduce CDN cache hit rate, which increases origin requests.
- Good alignment + immutable caching pushes almost all repeated reads onto the CDN and client OPFS cache.

### CDN egress vs origin egress

For Mode A public/shared images, caching is where the money and performance is:

- First user warms the cache (origin egress).
- Subsequent users hit edge cache (CDN egress, minimal origin egress).

For Mode B per-user images:

- Cache sharing is naturally limited.
- Consider whether CDN caching should be enabled at all; the browser’s OPFS cache already eliminates most repeat reads.

### Client-side OPFS caching effect

Once a chunk is written to the local `AEROSPAR` sparse cache file in OPFS:

- repeat reads for the same bytes do not incur network cost
- boot performance becomes much less sensitive to CDN cache hit rate after the first run

This makes it reasonable to optimize primarily for **first-boot latency** (good RTT, good throughput) and correct HTTP semantics.

---

## Alternative stacks (what to ensure)

The design works with any object store + CDN combination that supports the required knobs:

- **Cloudflare R2 + Cloudflare CDN**
  - Must support Range GET, correct 206 semantics, and header-based cache key control.
  - Must support a secure origin access pattern (or make R2 private behind the CDN).
- **Fastly + S3 (or other origin)**
  - VCL can explicitly handle `Range` and cache segmentation; ensure correct cache keying.
- **MinIO (self-hosted S3-compatible) + CDN**
  - Verify Range support and HEAD behavior.
  - Ensure consistent ETag/Last-Modified behavior and CORS.

Minimal “must support” checklist:

- `HEAD` returns `Content-Length` for large objects
- `GET` with `Range` returns `206` + correct `Content-Range`
- CDN can forward `Range` to origin
- CDN cache key can vary on `Range` (or otherwise safely cache partial content)
- Ability to disable compression / ensure `Content-Encoding` is not applied
- Ability to serve CORS headers (or host same-origin and avoid CORS)

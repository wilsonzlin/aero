# 16 - Disk Image Streaming (HTTP Range + Auth + COOP/COEP)

## Overview

The Aero emulator frequently needs **random access** to very large disk images (20–40GB+). This document specifies an interoperable, browser-compatible way to fetch disk image bytes from a remote service while:

1. Using HTTP `Range` efficiently (`206 Partial Content`).
2. Enforcing access control for **private** per-user images.
3. Remaining compatible with **cross-origin isolation** (COOP/COEP) required for `SharedArrayBuffer` and WASM threads.
4. Working with CDNs (cacheability, edge authorization, signed URLs/cookies).

This spec is written for both:

* **Client implementers** (browser code that reads disk bytes), and
* **Server/CDN implementers** (the “disk bytes endpoint” that serves ranges).

**Normative language:** The terms **MUST**, **SHOULD**, and **MAY** are used as in RFC 2119.

Related:
- [Storage Subsystem](./05-storage-subsystem.md) (client-side streaming disk design)
- [Storage trait consolidation](./20-storage-trait-consolidation.md) (repo-wide canonical disk/backend traits)
- [Disk Image Lifecycle and Access Control](./17-disk-image-lifecycle-and-access-control.md) (uploads/ownership/sharing/writeback)
- [Remote Disk Image Delivery](./16-remote-disk-image-delivery.md) (object store + CDN deployment contract)
- [HTTP Range + CDN Behavior](./17-range-cdn-behavior.md) (CloudFront/Cloudflare caching/limits considerations)
- [Chunked Disk Image Format](./18-chunked-disk-image-format.md) (CDN-friendly alternative to `Range`)
- [deployment/cloudfront-disk-streaming.md](./deployment/cloudfront-disk-streaming.md) (concrete AWS CloudFront setup)
- [`services/image-gateway`](../services/image-gateway/README.md) (reference backend: multipart upload + signed CloudFront access)
- [backend/disk-image-streaming-service.md](./backend/disk-image-streaming-service.md) (ops/runbook: headers, reverse proxy pitfalls, troubleshooting)
- [Browser APIs](./11-browser-apis.md) (threads + cross-origin isolation context)

---

## 1) Terminology and threat model

### Terminology

* **Disk image**: A byte-addressable, immutable (for the duration of a session/lease) representation of a virtual disk. Typically a raw image, qcow2, vhd, or a custom sparse format.
* **Disk bytes endpoint**: An HTTP resource that serves the disk image as a byte stream and supports HTTP `Range` requests. Example: `GET /disks/{diskId}/bytes`.
* **Public image**: An image intended to be readable by anyone who can discover its URL (or identifier), without per-user authorization. Public images are typically CDN-cacheable for long periods.
* **Private image**: An image readable only by an authenticated user (typically the owner) and/or explicitly shared users.
* **Owner**: The principal (user/account) that controls a private image.
* **Sharing** (future): Allowing an owner to delegate read access to another principal, usually by minting a separate capability or share policy. This spec treats sharing as a variation of the same **lease/capability** concept.
* **Disk access lease / capability**: A short-lived, scoped credential authorizing reads of a specific disk image (and optionally specific operations such as `read`, `write`, or a byte-range budget). The lease is represented to the browser as either:
  * a URL containing an embedded token (signed URL), and/or
  * a cookie (session/signed cookie), and/or
  * a header value (e.g., `Authorization: Bearer …`).

The **lease** concept is recommended because it separates:

* the **control plane** (user authentication, authorization decisions, logging), from
* the **data plane** (high-volume `Range` reads, CDN delivery).

### Threat model

This spec assumes:

* All traffic uses **HTTPS** (TLS). Plain HTTP is not supported.
* Attackers can run arbitrary JavaScript on **other web origins** and attempt to exfiltrate disk bytes.
* Attackers may obtain URLs via:
  * copy/paste and accidental sharing,
  * `Referer` leakage (if query tokens are used),
  * logs/analytics at intermediaries,
  * browser history, or
  * XSS in the application origin (out of scope to fully mitigate).

Primary threats and required mitigations:

1. **Unauthorized read of private images**
   *Private images MUST require a valid lease/capability or equivalent authorization on every request.*
2. **Capability leakage** (especially with query tokens)
   *Leases SHOULD be short-lived, tightly scoped, and revocable (by expiry).*
3. **Cross-origin confusion and CORS bypass**
   *Cross-origin deployments MUST implement correct CORS behavior and MUST NOT rely on “opaque” `no-cors` fetches.*
4. **COOP/COEP violations breaking `SharedArrayBuffer`**
   *All disk fetches used by a crossOriginIsolated app MUST be COEP-compatible (same-origin or explicitly allowed by CORS/CORP).*
5. **Caching private data incorrectly**
   *Private images MUST default to `Cache-Control: no-store, no-transform` unless authorization is enforced at the edge (signed URL/cookie) and cache behavior is intentionally configured.*

---

## 2) Recommended deployment patterns

### Preferred: same-origin disk endpoints (no CORS / no preflight)

**Goal:** Avoid CORS preflights entirely. `Range` is not a CORS-safelisted request header, so cross-origin requests will preflight unless the resource is same-origin.

Recommended topology:

* App: `https://app.example.com/` (HTML/JS/WASM)
* Disk bytes endpoint: `https://app.example.com/disks/{diskId}/bytes`

This can still be CDN-backed by using a single CDN hostname (same origin) with path-based routing:

* `/assets/*` → static bucket/origin
* `/disks/*` → disk bytes origin (object storage, NGINX, CloudFront/S3 behavior, etc.)
* `/api/*` → application API origin

Benefits:

* No CORS preflight overhead.
* Works naturally with cross-origin isolation (`COOP/COEP`), because disk responses are same-origin.
* Cookie-based auth (“session cookies”) is straightforward.

### Supported: cross-origin disk endpoints (CORS + COEP/CORP)

When disks must be served from a different origin (e.g., dedicated CDN domain):

* App: `https://app.example.com/`
* Disks: `https://disks.examplecdn.com/…`

Requirements for cross-origin disk origins:

* Correct **CORS** handling for:
  * `OPTIONS` preflights,
  * `Range` and optional `If-Range`, and
  * optional `Authorization` header.
* Responses compatible with the app’s **COEP** policy:
  * Same-origin is always OK.
  * Cross-origin resources must be CORS-enabled and/or send `Cross-Origin-Resource-Policy` (details in §6).

Operational guidance:

* Configure `Access-Control-Max-Age` for preflights to reduce request amplification (see §5).
* Prefer auth mechanisms that avoid cookies for cross-origin data plane (see §3) unless you intentionally want credentialed requests.

---

## 3) Auth mechanisms and tradeoffs

All mechanisms below can implement a “disk access lease/capability”. The choice impacts:

* CORS preflight frequency,
* cacheability (browser + CDN),
* token leakage risk, and
* operational complexity.

### Summary comparison

| Mechanism | Typical use | Cross-origin CORS complexity | CDN caching friendliness | Leakage risk | Notes |
|---|---|---:|---:|---:|---|
| Session cookies (same-origin) | Default for apps with login | Low (same-origin: none) | Medium (edge auth requires extra config) | Low | Best when disk endpoint is same-origin. |
| JWT bearer (`Authorization`) | API-style auth, non-browser clients | High (preflight; allow `Authorization`) | Low/Medium | Low | Avoid for CDN-cached byte ranges unless auth is validated at the edge. |
| Signed URL (token in query) | “Lease” URL from API | Medium (preflight for `Range`) | High | Medium | Works well with CDNs; keep tokens short-lived. |
| CloudFront signed URL | Signed URL, CloudFront-native | Medium | High | Medium | Similar to signed URL; uses CloudFront policies/keys. |
| CloudFront signed cookies | Cookie-based edge gating | High (credentialed CORS if cross-origin) | High | Low | Best cache hit rate for private content when using CloudFront; URL stays stable. |

### Recommended default

**Default recommendation:** **Same-origin disk bytes endpoint + session cookies**, optionally combined with a **lease API** that returns a short-lived URL for the current session.

Rationale:

* Same-origin avoids CORS preflight overhead from `Range`.
* Session cookies keep credentials out of URLs and support standard web auth.
* A lease layer provides a clean abstraction for future sharing, for revocation-by-expiry, and for switching to signed URL/cookie delivery without changing client code.

### When to use each mechanism

#### Session cookies (same-origin)
Use when:

* The disk bytes endpoint can be served from the **same origin** as the app.
* You want the simplest setup with minimal browser edge cases.

Notes:

* Prefer `SameSite=Lax` or `SameSite=Strict` cookies to reduce cross-site request abuse.
* Consider a separate “disk lease” cookie scoped to `/disks/*` with short TTL for tighter control.

#### JWT bearer (`Authorization: Bearer …`)
Use when:

* You need a unified auth story across web + non-web clients.
* You are not relying on shared CDN caching of private disk bytes (or you validate auth at the edge and strip/normalize the header for caching).

Tradeoffs:

* Cross-origin requests MUST preflight for `Authorization`.
* Forwarding `Authorization` to a CDN cache typically destroys cache hit rate (cache key varies per user/token) unless carefully configured.

#### Signed URLs (token in query)
Use when:

* You want the data plane to be **stateless** and/or served directly from object storage / CDN.
* You want **non-credentialed** fetches (`credentials: "omit"`) without cookies.

Tradeoffs:

* Tokens can leak via logs and `Referer` headers. Mitigations:
  * Use short expirations (minutes, not hours/days).
  * Scope the token to `{diskId, method=GET/HEAD, expiry}` at minimum.
  * Set the app `Referrer-Policy` to avoid leaking full URLs to other origins (e.g., `strict-origin-when-cross-origin` or `no-referrer`).

#### CloudFront signed URLs
Use when:

* You are on AWS/CloudFront and want CloudFront to enforce authorization at the edge.
* You want private images to remain CDN-cacheable without forwarding cookies/headers to origin.

#### CloudFront signed cookies
Use when:

* You need high cache hit rates for private images with stable URLs (no query tokens).
* You are willing to configure cookie distribution securely and handle credentialed requests if cross-origin.

Tradeoffs:

* If disks are on a different origin than the app, you MUST use credentialed CORS (`Access-Control-Allow-Credentials: true`) and cannot use `Access-Control-Allow-Origin: *`.
* `COEP: credentialless` is generally incompatible with cookie-based cross-origin disk fetches; prefer `COEP: require-corp` (see §6).

---

## 4) HTTP protocol for disk bytes

### Resource shape

The disk bytes endpoint is conceptually a static file:

* `GET /disks/{diskId}/bytes` returns bytes from offset `0..(size-1)`.
* `HEAD /disks/{diskId}/bytes` returns metadata (size, ETag, cache policy) without a body.

The resource **MUST be immutable for the lifetime of a lease**. If the underlying image changes, the URL and/or ETag MUST change (see ETag rules below).

### `GET` semantics

#### No `Range` header

Servers MAY return `200 OK` with the entire disk image. Clients SHOULD NOT use full downloads for large images; prefer ranges.

#### Single-range requests (required)

Clients MUST use a single `Range` of the form:

* `Range: bytes=<start>-<end>` (inclusive indices, zero-based)

Servers MUST support this form and respond with:

* `206 Partial Content`
* `Content-Range: bytes <start>-<end>/<size>`
* `Accept-Ranges: bytes`
* `Content-Length: <end - start + 1>`

Servers MAY support `bytes=<start>-` (open-ended) but clients SHOULD NOT rely on it unless explicitly supported.

Multi-range requests (e.g., `bytes=0-1,4-5`) are **not** supported by this spec. Servers SHOULD reject them with `416 Range Not Satisfiable`.

### `HEAD` semantics (required)

`HEAD` MUST be supported and MUST return:

* `200 OK`
* `Content-Length: <size>` (full resource size)
* `Accept-Ranges: bytes`
* `ETag` (see below)
* Appropriate cache headers (`Cache-Control`, etc.)

Clients SHOULD use `HEAD` to learn the disk size and current ETag before issuing `Range` reads.

### Invalid ranges

If `Range` is syntactically invalid or not satisfiable, servers MUST return:

* `416 Range Not Satisfiable`
* `Content-Range: bytes */<size>`

### ETag + `If-Range` (recommended)

Servers SHOULD return a strong `ETag` that changes whenever the disk image bytes change. Good candidates:

* a content hash (sha256), or
* a storage version identifier (S3 version ID), or
* `{diskId}:{generation}`.

Clients MAY use:

* `If-Range: "<etag>"` with a `Range` request.
* (Optional) `If-Range: <http-date>` when a `Last-Modified` value is available.

Behavior:

* If `If-Range` matches the current ETag, return `206` for the requested range.
* If it does not match (or the validator is invalid/weak), return `200 OK` with the full content (or `412 Precondition Failed` if your API prefers; if using `412`, document it and keep client code consistent).

Notes:

* The entity-tag form is strongly recommended for disk streaming (it allows safe resume even when filesystem/object-store mtimes are missing or coarse).
* The HTTP-date form compares at 1-second granularity (HTTP date resolution). Servers should compare `Last-Modified` and `If-Range` at second granularity to avoid false mismatches when the underlying store provides sub-second mtimes.

### Compression must be disabled

Disk images are binary; servers and intermediaries MUST NOT apply compression transforms because it breaks deterministic byte offsets. Requirements:

* `Content-Encoding: identity`
* `Cache-Control` MUST include `no-transform`.
  * Aero's browser clients reject `206` responses without it (defence-in-depth against intermediary transforms).

### Content type

Use:

* `Content-Type: application/octet-stream`
* `X-Content-Type-Options: nosniff` (recommended)

---

## 5) CORS requirements (when cross-origin)

If the disk bytes endpoint is not same-origin with the app, the browser will perform CORS checks.

### Why preflight happens

For Aero disk streaming, cross-origin `GET` requests typically include:

* `Range` (not CORS-safelisted) → triggers an `OPTIONS` preflight.
* Optionally `If-Range` (also not safelisted) → preflight.
* Optionally `Authorization` → preflight.
* Optionally `If-None-Match` / `If-Modified-Since` for conditional revalidation → preflight.

### Required `OPTIONS` handling

The disk bytes endpoint MUST respond to `OPTIONS` (preflight) with at least:

* `Access-Control-Allow-Methods: GET, HEAD, OPTIONS`
* `Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since, Authorization` (include only those you actually use, but `Range` is required)
* `Access-Control-Max-Age: <seconds>` (recommended; see below)

Origin handling:

* **Non-credentialed mode (recommended for signed URLs):**
  * `Access-Control-Allow-Origin: *`
  * Do NOT set `Access-Control-Allow-Credentials`
* **Credentialed mode (cookies):**
  * `Access-Control-Allow-Origin: https://app.example.com` (MUST NOT be `*`)
  * `Access-Control-Allow-Credentials: true`
  * `Vary: Origin`

### Required headers on `GET`/`HEAD` responses

For successful responses (`200`, `206`, and `416`), the disk bytes endpoint MUST include:

* `Access-Control-Allow-Origin: …` (either `*` or the specific origin)
* `Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Content-Encoding`

`Access-Control-Expose-Headers` is required so browser code can read:

* the returned range (`Content-Range`),
* the resource size (`Content-Length` on `HEAD` or `Content-Range` total size), and
* cache validators (`ETag`).
* and verify no compression transforms were applied (`Content-Encoding` should be absent or `identity`).

### Preflight caching (`Access-Control-Max-Age`)

Preflights can be expensive with many small range reads. Set:

* `Access-Control-Max-Age: 600` (10 minutes) as a conservative default.
* Higher values (e.g., `86400`) MAY be used, but browsers may cap caching duration.

---

## 6) COOP/COEP / cross-origin isolation requirements

To use `SharedArrayBuffer` (and thus WASM threads), the top-level app must be **cross-origin isolated**:

* `Cross-Origin-Opener-Policy: same-origin`
* `Cross-Origin-Embedder-Policy: require-corp` (recommended default)
  or `Cross-Origin-Embedder-Policy: credentialless` (alternative; see notes below)

### Requirements for disk bytes responses

In a crossOriginIsolated app:

* Same-origin disk responses are always allowed.
* Cross-origin disk responses MUST be explicitly permitted. For `fetch()`-based byte reads, the practical requirement is:
  * the response is **CORS-enabled** (i.e., it passes CORS checks for the requesting origin), and
  * the response is not blocked by COEP.

For defence-in-depth and clarity, disk byte responses SHOULD also include:

* `Cross-Origin-Resource-Policy: same-site` when the disk origin is a subdomain of the app’s site (eTLD+1 matches), and you only intend same-site apps to read it.
* `Cross-Origin-Resource-Policy: cross-origin` when disks are intentionally served to multiple unrelated origins (e.g., a dedicated CDN domain used by multiple apps).

### When to “rely on CORS” vs CORP

* CORS is required for JavaScript to read cross-origin bytes at all.
* CORP is primarily needed to satisfy `COEP: require-corp` for non-CORS subresource loads, but including it on disk responses is low-cost and prevents accidental breakage if the resource is consumed in other ways.

### Notes on `COEP: credentialless`

`COEP: credentialless` can reduce some deployment friction by ensuring cross-origin subresource requests are made without cookies by default. However:

* Aero disk streaming still requires CORS for JS-readable responses.
* If you rely on **cookie-based** auth for a cross-origin disk endpoint (session cookies or signed cookies), prefer `COEP: require-corp` to avoid surprises; test carefully across browsers.

---

## 7) CDN caching strategy

### Public images

For publicly readable images (no per-user authorization), prefer long-lived immutable caching:

* `Cache-Control: public, max-age=31536000, immutable, no-transform`
* Stable `ETag` (content hash or generation ID)

### Private images (safe defaults)

Default safe posture for private images:

* `Cache-Control: no-store, no-transform`

This prevents browsers and intermediary caches from storing private bytes where authorization is not enforced.

### Private images with edge authorization (signed URL/cookie)

If authorization is validated at the edge (CDN) and the cached object is identical for all authorized users, you MAY use CDN caching for private images. Typical examples:

* Signed URL validated by CDN (token is part of cache key).
* Signed cookie validated by CDN (URL stable; CDN checks cookie before serving cached bytes).

In these cases, you can often use the same caching headers as public content **at the CDN layer**. Be explicit about where caching happens:

* At the **browser**: it is usually fine to keep `Cache-Control: no-store, no-transform` for private disks to avoid local persistence surprises.
* At the **CDN**: configure edge caching policies, and use origin headers appropriately for your CDN/provider.

### Range caching and chunk alignment

Even with a CDN, `Range` caching can be inefficient if clients request arbitrary offsets. Clients SHOULD:

* Choose a fixed **chunk size** (default: **1 MiB** / 1,048,576 bytes).
* Align reads to chunk boundaries: request `bytes = floor(offset/chunk)*chunk … +chunk-1`.

This increases cache hit rates and reduces origin load because repeated reads map to identical range requests.

---

## 8) Concrete examples

### Example: `206 Partial Content` range response

Request:

```http
GET /disks/disk_123/bytes HTTP/1.1
Host: disks.examplecdn.com
Origin: https://app.example.com
Range: bytes=1048576-2097151
If-Range: "disk_123:gen_42"
```

Response:

```http
HTTP/1.1 206 Partial Content
Content-Type: application/octet-stream
Content-Encoding: identity
Cache-Control: no-transform
Accept-Ranges: bytes
Content-Range: bytes 1048576-2097151/42949672960
Content-Length: 1048576
ETag: "disk_123:gen_42"
Cross-Origin-Resource-Policy: same-site
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Content-Encoding
Vary: Origin
```

### Example: invalid range (`416`)

```http
HTTP/1.1 416 Range Not Satisfiable
Accept-Ranges: bytes
Content-Range: bytes */42949672960
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Content-Encoding
Vary: Origin
```

### Example: `OPTIONS` preflight (cross-origin + Range)

Preflight request:

```http
OPTIONS /disks/disk_123/bytes HTTP/1.1
Host: disks.examplecdn.com
Origin: https://app.example.com
Access-Control-Request-Method: GET
Access-Control-Request-Headers: range, if-range
```

Preflight response:

```http
HTTP/1.1 204 No Content
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since, Authorization
Access-Control-Max-Age: 600
Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers
```

### Example: lease API response schema

The control-plane API returns a short-lived lease that the client uses for subsequent `Range` reads.

```json
{
  "diskId": "disk_123",
  "url": "https://disks.examplecdn.com/disks/disk_123/bytes?cap=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
  "expiresAt": "2026-01-10T12:34:56Z"
}
```

Notes:

* `cap` is an example capability token; the exact format is implementation-defined.
* Leases SHOULD be valid for minutes, not hours, and SHOULD be renewable by re-calling the lease API.

---

## 9) Deployment profiles

- **AWS/CloudFront profile:** [`./deployment/cloudfront-disk-streaming.md`](./deployment/cloudfront-disk-streaming.md)

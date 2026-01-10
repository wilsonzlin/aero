# 17 - HTTP Range + CDN Behavior (CloudFront Focus)

## Overview

Aero streams large disk images to the browser using HTTP `Range` requests (`206 Partial Content`). Whether this is fast and cost-effective depends heavily on:

1. **Whether the CDN caches Range responses**, and
2. **Whether the CDN has a hard maximum cacheable response size** (a common gotcha for multi‑GB images).

This document turns the “will the CDN do the right thing?” uncertainty into concrete guidance:

- Clear yes/no answers for Amazon CloudFront (based on AWS documentation + common field observations).
- A safe **chunked-object strategy** to avoid running into CloudFront/CDN cacheable-size ceilings and to keep cache behavior predictable.
- A summary of one alternative cache (Nginx reverse proxy cache).
- A checklist that operators can run with `curl` to validate their own deployment.

If you are looking for a **deployment runbook** (S3 + CloudFront + signed URLs/cookies, plus CORS/COEP/CORP headers), see:

- [`docs/deployment/cloudfront-disk-streaming.md`](./deployment/cloudfront-disk-streaming.md)

> **Assumptions / limitations**
>
> This repo does not contain AWS credentials or live infrastructure, so CloudFront behavior here is based on published AWS documentation and widely observed headers. Use the checklist below to validate the behavior in *your* distribution and region.

## Reference infrastructure in this repo

- Local development (MinIO + optional reverse proxy for edge/CORS emulation):
  - [`infra/local-object-store/README.md`](../infra/local-object-store/README.md)
- AWS production reference (S3 + CloudFront tuned for Range + CORS):
  - [`infra/aws-s3-cloudfront-range/README.md`](../infra/aws-s3-cloudfront-range/README.md)

---

## CloudFront: direct answers

### Q1: Does CloudFront cache `206 Partial Content` per-range automatically? Do we need `Range` in the cache key?

- **Answer:** **Yes, CloudFront supports byte-range requests and caches content for them**, but **you should *not* include `Range` in the cache key**.
- **Why:** CloudFront treats Range requests as a first-class feature; caching is still keyed on the object URL (and whatever else you include in your cache key), and CloudFront can satisfy subsequent Range requests from the edge cache.
  - AWS docs: on a `Range GET`, CloudFront checks the edge cache; if the requested range is missing, it forwards the request to the origin and **“caches it for future requests.”**

**Recommendation for Aero:** Keep the cache key minimal (path + version). Do *not* vary/cache-key on `Range`; doing so will fragment the cache and destroy hit ratio.

**Important operational note:** when validating `206` caching, make sure you’re comparing requests handled by the **same edge cache node**. CloudFront POPs often return multiple IPs and the `Via:` value can change between connections; caches are not necessarily shared across those nodes.

- If you run `curl` twice as separate processes, you can hit different cache nodes and see `X-Cache: Miss from cloudfront` both times even though caching is working.
- Prefer to either:
  - reuse the same connection (example below), or
  - pin to one CloudFront edge IP with `curl --resolve`.

Example (single `curl` process, same connection → you should typically see `Miss` then `Hit`):

```bash
URL='https://d3njjcbhbojbot.cloudfront.net/webapps/r2-builds/front-page/en.app.1cc5a48243bc23fab536.js?cachebust=aero-range-test'
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" \
  --next -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" \
  | grep -iE '^(x-cache|age|via|x-amz-cf-pop|content-range):'
```

### Q2: Can CloudFront serve/caches Range requests for objects larger than CloudFront’s maximum cacheable file size?

- **Answer:** **Yes, as long as you only retrieve the object in parts using Range requests.**
- **CloudFront limit:** CloudFront’s **maximum cacheable file size per HTTP GET response is 50&nbsp;GB** (historically 20&nbsp;GB; see current CloudFront limits). With caching enabled, a **non-Range `GET`** for an object larger than this can fail.
- **Why Range helps:** A `Range` response’s `Content-Length` is only the requested slice. AWS documents that CloudFront will **cache the requested range** and can serve subsequent requests for the same range from the edge cache.
- **Mitigation for very large images:** If your disk image might exceed 50&nbsp;GB, either:
  - (A) enforce **range-only** access (no full `GET`; avoid `HEAD`—use `Range: bytes=0-0` to learn total size via `Content-Range`), or
  - (B) publish the image as **chunk objects** (details below) for simpler operational behavior and portability across CDNs.

### Q3: What about `Cache-Control: immutable` and long TTLs for `206`?

- **Answer:** CloudFront caching is governed by **TTL (`max-age` / `s-maxage` / `Expires`) and CloudFront cache policy caps**, not by the `immutable` token.
- **Practical effect:** You can (and should) use long-lived immutable caching headers on disk image chunks. Just ensure URLs are versioned (hash in the path) so you never need to “update in place.”

### Q4: What headers indicate cache hits/misses for `206`?

CloudFront typically returns the same debugging headers for `206` as for `200`:

- `X-Cache`: `Miss from cloudfront`, `Hit from cloudfront`, `RefreshHit from cloudfront`, `Error from cloudfront`
- `Age`: present/increasing on cache hits (per edge location)
- `Via`, `X-Amz-Cf-Pop`, `X-Amz-Cf-Id`: useful for correlating POP behavior

**Spot-check (public CloudFront distribution, Jan 2026):**

```bash
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' \
  'https://d3njjcbhbojbot.cloudfront.net/favicon.ico?cachebust=aero-range-test' \
  | grep -iE '^(http/|x-cache|age|content-range):'
```

On first request: `x-cache: Miss from cloudfront` (often `age` absent). Repeating the same request typically returns `x-cache: Hit from cloudfront` with a small `age: <n>`.

---

## CloudFront: what to configure for Aero

### 1) Origin must support byte ranges correctly

The origin must return correct `206` responses when given `Range: bytes=...`, including:

- `Accept-Ranges: bytes` (strongly recommended)
- `Content-Range: bytes START-END/TOTAL`
- A stable validator (`ETag` is ideal)

For maximum CDN compatibility, clients should send **a single byte range per request** (avoid multipart `Range: bytes=a-b,c-d`).

S3 origins satisfy this naturally for normal objects.

### 2) Cache policy (cache key + TTL)

For a disk image or chunk object that never changes once published:

- **Cache key:** path only (and optionally a *version* query param if you can’t put the version in the path).
  - Avoid including headers/cookies in the cache key.
  - Do **not** include `Range` in the cache key.
- **TTL:** set for long-lived caching:
  - Prefer `Cache-Control: public, max-age=31536000, immutable`
  - Ensure CloudFront cache policy **Maximum TTL** is >= your origin’s `max-age` or CloudFront will cap it.

**Why this matters for Aero:** random-access reads cause many repeated small Range reads. Any extra cache-key variance (cookies, headers) will cause near-0% hit ratio.

### 3) CORS (browser requirement)

`Range` is not a CORS-safelisted request header, so browser fetches will trigger a preflight.

Ensure:

- `Access-Control-Allow-Methods: GET, HEAD, OPTIONS`
- `Access-Control-Allow-Headers: Range`
- `Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag`

If using S3 as origin, configure the bucket CORS rules accordingly. If using a custom origin, ensure OPTIONS is handled.

---

## CloudFront limits: max cacheable size (50&nbsp;GB) and what to do about it

CloudFront has a documented **maximum cacheable file size per HTTP GET response of 50&nbsp;GB**.

- If the object is **≤ 50&nbsp;GB**, CloudFront can cache a normal `200 OK` response and then satisfy subsequent `Range` requests from the cached object (best-case behavior).
- If the object is **> 50&nbsp;GB** and caching is enabled, a non-Range `GET` can fail. However, AWS documents that you can still use CloudFront by retrieving the object with **multiple Range GETs**, each returning a response `< 50&nbsp;GB`; CloudFront caches each requested part for future requests.

For Aero disk streaming (many small random-access reads), depending on Range-caching behavior at your POP, relying on per-Range caching can create many cache entries. A more predictable approach is to publish the image as fixed-size **chunk objects** (or adopt the no-Range format in [`18-chunked-disk-image-format.md`](./18-chunked-disk-image-format.md)).

### Recommended mitigation (portable): store disk images as chunk objects

Instead of one URL for the entire disk image:

```
/images/<image_id>/disk.img
```

publish a directory of fixed-size chunks:

```
/images/<image_id>/chunks/000000.bin
/images/<image_id>/chunks/000001.bin
...
```

And a small manifest:

```
/images/<image_id>/manifest.json
```

#### Chunk size recommendations

Choose a chunk size that:

- Is **well under** CloudFront’s max cacheable response size (50&nbsp;GB) and any object-store limits.
- Is not so large that a single cache miss is painful.

Practical ranges:

- **64 MiB – 512 MiB** per chunk (typical CDN-friendly range)
- If you want to avoid `Range` preflights entirely, you can set chunk size to your client fetch unit and do plain `GET` of whole chunk objects.

#### Client mapping: offset → chunk + intra-chunk range

Given:

- `chunkSize` (bytes)
- `offset` (bytes into the virtual disk)
- `length` (bytes to read)

Compute:

```
chunkIndexStart = floor(offset / chunkSize)
chunkIndexEnd   = floor((offset + length - 1) / chunkSize)
```

For each chunk `i` in `[chunkIndexStart, chunkIndexEnd]`:

```
chunkBase = i * chunkSize
rangeStartInChunk = max(offset, chunkBase) - chunkBase
rangeEndInChunk   = min(offset + length, chunkBase + chunkSize) - 1 - chunkBase

GET /images/<image_id>/chunks/<i>.bin
Range: bytes=<rangeStartInChunk>-<rangeEndInChunk>
```

This strategy:

- Works on CDNs with cacheable-size limits.
- Lets the CDN cache each chunk independently.
- Makes origin load and cache hit ratios predictable.

---

## Alternative: Nginx reverse proxy cache (`proxy_cache`) + Range

If you run your own CDN-like edge or regional caching layer with Nginx:

- Nginx can serve Range requests for static files easily.
- For **proxy caching large objects with Range**, the robust approach is to use the **`slice`** module so the cache stores fixed-size slices rather than arbitrary user-requested ranges.

### Recommended Nginx config pattern (slice caching)

```nginx
# Cache storage (size/tuning are examples)
proxy_cache_path /var/cache/nginx/aero
  levels=1:2
  keys_zone=aero:1g
  max_size=500g
  inactive=30d
  use_temp_path=off;

server {
  location /images/ {
    proxy_pass https://origin.example.com;

    # Slice into fixed 1 MiB subrequests.
    # This normalizes all arbitrary viewer Range requests into stable cache keys.
    slice 1m;
    proxy_set_header Range $slice_range;

    proxy_cache aero;
    proxy_cache_lock on;

    # Cache both 200 and 206.
    proxy_cache_valid 200 206 30d;

    # Include slice range in the cache key.
    proxy_cache_key "$scheme://$host$uri$is_args$args|$slice_range";

    add_header X-Cache-Status $upstream_cache_status always;
  }
}
```

**Why `slice` matters:** without slicing, every distinct `Range: bytes=a-b` can become a distinct cache entry, which explodes cache cardinality and destroys hit ratio for random access patterns.

---

## Operator validation checklist (copy/paste)

Run these tests against your **CDN URL** (not the origin), ideally from two different machines/regions:

### 1) Basic Range correctness

```bash
URL="https://YOUR_DOMAIN/images/IMAGE_ID/chunks/000000.bin"
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL"
```

Confirm:

- `HTTP/* 206`
- `Content-Range: bytes 0-1023/<total>`
- `Accept-Ranges: bytes` (often present on `HEAD`/`200`; may not appear on `206` for some origins)

### 2) Cache hit behavior for `206`

Repeat the same request twice.

> Tip: if you run two separate `curl` commands, CloudFront may route you to different edge cache nodes. For the cleanest signal, reuse the same connection with `--next`:

```bash
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" \
  --next -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" \
  | grep -iE '^(x-cache|age|via|x-amz-cf-pop):'
```

Expect on CloudFront:

- First request: `X-Cache: Miss from cloudfront` (often `Age` absent/0)
- Second request: `X-Cache: Hit from cloudfront` and `Age: <n>`

If the second request is still a `Miss`, run a full `GET` and try again:

```bash
curl -fsS -D - -o /dev/null "$URL" | grep -iE '^(x-cache|age|via|x-amz-cf-pop):'
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" | grep -iE '^(x-cache|age|via|x-amz-cf-pop):'
```

If `GET` becomes a hit but `Range` stays a miss, assume `206` caching is not working for your workload and use chunk objects / no-Range delivery.

### 3) Different range behavior

```bash
curl -fsS -D - -o /dev/null -H 'Range: bytes=1048576-1049599' "$URL" | grep -iE '^(http/|x-cache|age|content-range):'
```

This should still be `206`, with an appropriate `Content-Range`, and after repeating it should become a hit.

### 4) Verify the CDN isn’t “cheating” by fetching full objects on a Range miss

Enable origin access logs (or per-request logging) and inspect what the CDN requested from the origin when the viewer asked for `Range: bytes=0-0`.

- CloudFront may request a **larger range than the viewer asked for** (AWS documents this as an optimization). This is usually fine; you’re checking for “downloaded the whole object” vs “downloaded a bounded range.”
- If the origin sees `Range: bytes=0-0`, the CDN is fetching only the requested bytes (good).
- If the origin sees a full `GET` with a large transfer size, the CDN is filling cache by downloading the entire object on first Range miss (still works, but makes large single-objects very expensive).

### 5) Object size ceiling check (CloudFront-specific)

Publish (or identify) an object whose total size is **> 50 GB** and attempt a tiny range:

```bash
curl -v -o /dev/null -H 'Range: bytes=0-0' "https://YOUR_CLOUDFRONT_DOMAIN/path/to/oversize-object"
```

If the `Range` request fails, you likely need chunk objects (or a different delivery mechanism).

Also test how a non-Range request behaves (avoid downloading the full body; use `HEAD`):

```bash
curl -v -I "https://YOUR_CLOUDFRONT_DOMAIN/path/to/oversize-object"
```

If `HEAD` fails for oversized objects in your configuration, avoid `HEAD` in clients and use `Range: bytes=0-0` to discover total size via `Content-Range`.

---

## References (vendor docs)

- AWS CloudFront Developer Guide (Range GETs): https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/RangeGETs.html
- AWS CloudFront quotas/limits (max cacheable file size per GET response): https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/cloudfront-limits.html
- RFC 9110 (HTTP Semantics, Range): https://www.rfc-editor.org/rfc/rfc9110.html
- RFC 9111 (HTTP Caching, partial responses): https://www.rfc-editor.org/rfc/rfc9111.html

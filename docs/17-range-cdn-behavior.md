# 17 - HTTP Range + CDN Behavior (CloudFront Focus)

## Overview

Aero streams large disk images to the browser using HTTP `Range` requests (`206 Partial Content`). Whether this is fast and cost-effective depends heavily on:

1. **Whether the CDN caches Range responses**, and
2. **Whether the CDN has a hard maximum object size** (a common gotcha for multi‑GB images).

This document turns the “will the CDN do the right thing?” uncertainty into concrete guidance:

- Clear yes/no answers for Amazon CloudFront (based on AWS documentation + common field observations).
- A safe **chunked-object strategy** to avoid CloudFront’s object-size ceiling.
- A summary of one alternative cache (Nginx reverse proxy cache).
- A checklist that operators can run with `curl` to validate their own deployment.

> **Assumptions / limitations**
>
> This repo does not contain AWS credentials or live infrastructure, so CloudFront behavior here is based on published AWS documentation and widely observed headers. Use the checklist below to validate the behavior in *your* distribution and region.

---

## CloudFront: direct answers

### Q1: Does CloudFront cache `206 Partial Content` per-range automatically? Do we need `Range` in the cache key?

- **Answer:** **Yes, CloudFront supports byte-range requests and caches content for them**, but **you should *not* include `Range` in the cache key**.
- **Why:** CloudFront treats Range requests as a first-class feature; caching is still keyed on the object URL (and whatever else you include in your cache key), and CloudFront can satisfy subsequent Range requests from the edge cache.

**Recommendation for Aero:** Keep the cache key minimal (path + version). Do *not* vary/cache-key on `Range`; doing so will fragment the cache and destroy hit ratio.

### Q2: Can CloudFront serve Range requests for objects larger than the CloudFront max object size (historically 20 GB)?

- **Answer:** **No. Range requests do not let you bypass CloudFront’s maximum object size.**
- **Mitigation:** Store disk images as **multiple smaller objects** (“chunked objects”) and map byte offsets to chunk IDs + intra-chunk ranges (details below).

### Q3: What about `Cache-Control: immutable` and long TTLs for `206`?

- **Answer:** CloudFront caching is governed by **TTL (`max-age` / `s-maxage` / `Expires`) and CloudFront cache policy caps**, not by the `immutable` token.
- **Practical effect:** You can (and should) use long-lived immutable caching headers on disk image chunks. Just ensure URLs are versioned (hash in the path) so you never need to “update in place.”

### Q4: What headers indicate cache hits/misses for `206`?

CloudFront typically returns the same debugging headers for `206` as for `200`:

- `X-Cache`: `Miss from cloudfront`, `Hit from cloudfront`, `RefreshHit from cloudfront`, `Error from cloudfront`
- `Age`: present/increasing on cache hits (per edge location)
- `Via`, `X-Amz-Cf-Pop`, `X-Amz-Cf-Id`: useful for correlating POP behavior

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

## CloudFront hard limits: why chunked objects are required

CloudFront has a documented maximum object size (commonly cited as **20 GB**). This is a *hard ceiling* for “one object behind one URL.”

**Key point:** Even if the viewer only requests a tiny Range, a `206` response still includes the **total object length** (in `Content-Range`), and CloudFront’s object-size enforcement applies to the *whole object*, not just the requested slice.

### Recommended mitigation: store disk images as chunk objects

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

- Is **well under** CloudFront’s max object size.
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

- Works on CDNs with object-size limits.
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
- `Accept-Ranges: bytes`

### 2) Cache hit behavior for `206`

Repeat the same request twice:

```bash
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" | grep -iE '^(x-cache|age|via|x-amz-cf-pop):'
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-1023' "$URL" | grep -iE '^(x-cache|age|via|x-amz-cf-pop):'
```

Expect on CloudFront:

- First request: `X-Cache: Miss from cloudfront` (often `Age` absent/0)
- Second request: `X-Cache: Hit from cloudfront` and `Age: <n>`

### 3) Different range behavior

```bash
curl -fsS -D - -o /dev/null -H 'Range: bytes=1048576-1049599' "$URL" | grep -iE '^(http/|x-cache|age|content-range):'
```

This should still be `206`, with an appropriate `Content-Range`, and after repeating it should become a hit.

### 4) Verify the CDN isn’t “cheating” by fetching full objects on a Range miss

Enable origin access logs (or per-request logging) and inspect what the CDN requested from the origin when the viewer asked for `Range: bytes=0-0`.

- If the origin sees `Range: bytes=0-0`, the CDN is fetching only the requested bytes (good).
- If the origin sees a full `GET` with a large transfer size, the CDN is filling cache by downloading the entire object on first Range miss (still works, but makes large single-objects very expensive).

### 5) Object size ceiling check (CloudFront-specific)

Publish (or identify) an object whose total size is **> 20 GB** and attempt:

```bash
curl -v -o /dev/null -H 'Range: bytes=0-1023' "https://YOUR_CLOUDFRONT_DOMAIN/path/to/oversize-object"
```

If CloudFront fails or returns an error, chunked objects are mandatory.

---

## References (vendor docs)

- AWS CloudFront Developer Guide (Range GETs): https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/RangeGETs.html
- AWS CloudFront quotas/limits (max object size): https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/cloudfront-limits.html
- RFC 9110 (HTTP Semantics, Range): https://www.rfc-editor.org/rfc/rfc9110.html
- RFC 9111 (HTTP Caching, partial responses): https://www.rfc-editor.org/rfc/rfc9111.html

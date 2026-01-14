# CloudFront: authenticated Range streaming for disk images

This guide describes a concrete AWS setup for serving **large, private disk images** (S3) through **CloudFront** with **authenticated `Range` requests** that work in browsers with **COOP/COEP** (for `SharedArrayBuffer`) and do not leak private data through caching.

Related docs:
- [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](../16-disk-image-streaming-auth.md) (normative protocol + CORS/COEP requirements)
- [Disk Image Lifecycle and Access Control](../17-disk-image-lifecycle-and-access-control.md) (uploads/ownership/sharing/writeback scopes for hosted service)
- [Remote Disk Image Delivery](../16-remote-disk-image-delivery.md) (object store + CDN delivery contract)
- [Storage trait consolidation](../20-storage-trait-consolidation.md) (repo-wide canonical disk/backend traits)

The intended use case is:

- Disk images stored in **S3** (private bucket).
- Browser reads the image via many **`GET` + `Range: bytes=…`** requests (random access).
- Access control enforced at the **CloudFront edge** using **signed URLs** or **signed cookies**.

---

## A) Architecture

### High-level diagram

```
Browser (app) ── Range GET/HEAD/OPTIONS ──▶ CloudFront (cdn.example.com)
     ▲                                              │
     │                                              │ (OAC-signed origin request)
     │ Set-Cookie (signed cookies)                  ▼
Backend (auth) ───────────────────────────────▶ S3 bucket (private)
```

The “Backend (auth)” box can be implemented by the reference service at
[`services/image-gateway/`](../../services/image-gateway/) (S3 multipart upload + CloudFront signed cookies/URLs).

### S3 layout: public vs private keys

Use S3 object keys that make “public vs private” explicit and keep private objects isolated by key prefix:

- **Public** (no auth at CloudFront):
  - `public/base-images/win7-sp1-amd64.raw`
- **Per-user** (CloudFront signed URL/cookie required):
  - `users/<uid>/disk.raw`
  - `users/<uid>/snapshots/<snapshot-id>.raw`

This path split enables two CloudFront cache behaviors:

- `/public/*` → public behavior, long cache TTLs
- `/users/*` → private behavior, viewer access restricted

### CloudFront cacheable size limits (Range helps; chunking is still a good default)

CloudFront enforces a maximum **cacheable file size per HTTP response**. If a client ever triggers a full-object `GET` (no `Range`) for a very large disk, you can run into CloudFront size-limit behavior (cache bypass or errors depending on settings and size).

`Range` requests help because a `206 Partial Content` response’s `Content-Length` is only the requested slice; CloudFront can cache those slices and serve subsequent requests for the same ranges from the edge cache.

Practical guidance:

- Ensure the browser always reads disk bytes via `Range`.
- For very large objects, prefer learning total size via `Range: bytes=0-0` + `Content-Range` instead of relying on `HEAD` (see [`docs/17-range-cdn-behavior.md`](../17-range-cdn-behavior.md)).
- For the most predictable behavior across CDNs (and to avoid “first Range miss downloads a lot” surprises), consider storing the disk as **multiple fixed-size chunk objects** instead of one giant object:

```
users/<uid>/images/<image-id>/manifest.json
users/<uid>/images/<image-id>/chunks/000000.bin
users/<uid>/images/<image-id>/chunks/000001.bin
...
```

Then the client maps `byteOffset → chunkIndex + offsetWithinChunk`.

Notes:

- This still works with signed cookies/URLs (scope the policy to `users/<uid>/*`).
- If you fetch whole chunk objects (no `Range` header), you can avoid `Range`-triggered CORS preflights for cross-origin deployments.
- See [`docs/17-range-cdn-behavior.md`](../17-range-cdn-behavior.md) for more detailed CloudFront Range behavior and chunking guidance.

### Keep S3 private (OAC/OAI)

S3 must **not** be public. Configure CloudFront to be the only reader:

- Prefer **Origin Access Control (OAC)** (newer).
- Origin Access Identity (OAI) is legacy; use only if you must.

With OAC:

1. Create a CloudFront distribution with an S3 origin.
2. Create/attach an **OAC** to that origin.
3. Add an S3 bucket policy that allows **only** that distribution to `s3:GetObject`.

Example bucket policy (replace placeholders):

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "AllowCloudFrontRead",
      "Effect": "Allow",
      "Principal": { "Service": "cloudfront.amazonaws.com" },
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::MY_BUCKET_NAME/*",
      "Condition": {
        "StringEquals": {
          "AWS:SourceArn": "arn:aws:cloudfront::MY_AWS_ACCOUNT_ID:distribution/MY_DISTRIBUTION_ID"
        }
      }
    }
  ]
}
```

### CloudFront distribution setup (concrete)

#### 1) Use a custom domain (strongly recommended)

Signed cookies are easiest when the CDN hostname is under your domain so cookies are first-party:

- App: `https://app.example.com`
- Disk CDN: `https://cdn.example.com`

Using the default `*.cloudfront.net` hostname makes signed cookies difficult (you generally cannot set cookies for `cloudfront.net` from your own site).

#### 2) Create the distribution and S3 origin

In the distribution:

- **Origin**: your S3 bucket (no public access)
- **Origin access**: attach the OAC created above
- **Viewer protocol policy**: redirect HTTP → HTTPS

#### 3) Add two cache behaviors

Create two behaviors that map directly to the S3 prefixes described above:

- Behavior: `/public/*`
  - Viewer access restriction: **off**
  - Cache TTLs: long (public base images should be immutable; version keys if you update them)
- Behavior: `/users/*`
  - Viewer access restriction: **on** (trusted key group)
  - Cache TTLs: can still be long if per-user images are immutable/versioned

For both behaviors, allow:

- **Allowed HTTP methods**: `GET, HEAD, OPTIONS`
  - `OPTIONS` is required if you will do cross-origin `Range` fetches (preflight).

#### 4) Create signing keys + a key group (for signed URLs/cookies)

CloudFront signed URLs/cookies use an RSA key pair:

1. Generate a key pair:

   ```bash
   openssl genrsa -out cloudfront_private_key.pem 2048
   openssl rsa -pubout -in cloudfront_private_key.pem -out cloudfront_public_key.pem
   ```

2. In CloudFront:
   - Create a **Public key** using `cloudfront_public_key.pem`
   - Create a **Key group** and add that public key
3. In the `/users/*` cache behavior:
   - Set **Trusted key groups** to the key group you created

Your backend stores `cloudfront_private_key.pem` securely and uses the public key ID (often called the “key pair ID” in signing libraries).

#### 5) Forward `Range` to S3 (origin request policy)

Create an **origin request policy** for disk objects that forwards:

- `Range`
- `If-Range` (optional)

If you use an **S3 origin** and expect S3 to answer browser CORS preflights, also forward the CORS preflight headers so S3 can evaluate its CORS rules:

- `Origin`
- `Access-Control-Request-Method`
- `Access-Control-Request-Headers`

Do not forward cookies/query strings to S3 unless you have a specific need.

#### 6) Response headers policy (CORS + CORP)

Attach a response headers policy to disk behaviors that sets:

- CORS headers (if cross-origin)
- `Cross-Origin-Resource-Policy` (CORP) for COEP compatibility

See [Headers policy](#d-headers-policy-cors--corp--coep-compatibility) below.

---

## B) Access control options (signed URLs vs signed cookies)

CloudFront “viewer access restriction” is the key feature here: CloudFront validates the signature **at the edge** before serving *any* bytes (cache hit or origin fetch).

### Option 1: CloudFront signed URLs

**When to use**

- Cross-origin fetches where cookies are undesirable/unavailable.
- One-off downloads.
- Environments where you cannot or do not want to set cookies (some embedded contexts).

**How it works**

- Your backend generates a URL like:
  - `https://cdn.example.com/users/123/disk.raw?Expires=...&Signature=...&Key-Pair-Id=...`
- Browser uses the same URL for all range reads; the `Range` header changes per request.

**Risks / operational costs**

- Token in URL can leak via:
  - CDN/server logs
  - copy/paste
  - referrers if the URL ever lands in HTML/navigation (mitigate with `Referrer-Policy: no-referrer`)
- Cache fragmentation if you include query strings in the cache key (see caching section).

### Option 2: CloudFront signed cookies

**When to use (recommended for “browser fetches many ranges”)**

- The emulator will fetch **hundreds/thousands** of ranges.
- You can serve the CDN from a **custom domain on your site** (recommended):
  - `https://cdn.example.com/...` (instead of `https://d123.cloudfront.net/...`)
- You want to avoid secrets in URLs.

**How it works**

- Backend sets 3 cookies (names are fixed by CloudFront):
  - `CloudFront-Policy` (or `CloudFront-Expires` for canned policies)
  - `CloudFront-Signature`
  - `CloudFront-Key-Pair-Id`
- Browser then fetches:
  - `https://cdn.example.com/users/123/disk.raw` with `Range` headers
  - the cookies ride along automatically (same-site), or with `fetch(..., { credentials: "include" })` (cross-origin)

**Pros**

- No token in URL.
- One mint per session instead of per request.
- Better cache hit ratio (cache key does not need to include auth material).

**Cons**

- Requires a custom domain you control if you want cookies to be first-party.
- If the CDN is cross-site, third-party cookie restrictions can break it.

### Default recommendation

For the browser disk streaming path, default to:

- **Signed cookies**, served from the **same site** as the app (e.g. `app.example.com` + `cdn.example.com`), or ideally the **same origin** (single CloudFront distribution for both app + disks).

Use **signed URLs** when:

- You cannot rely on cookies being sent (cross-site, third-party cookie blocking).
- You need to hand out a link to another client (download tool).

---

## C) Caching & cache keys (and how to avoid private data leakage)

### Key safety property

With CloudFront signed URLs/cookies, **authorization happens at the edge**, *before* cache lookup is served to the client. That means:

- It is safe for CloudFront to cache `/users/<uid>/disk.raw` and serve it from cache later
  **as long as CloudFront viewer restriction remains enabled** for that path.
- You do **not** need to include auth tokens (cookies/query string) in the cache key to prevent leaks.

### What to avoid (common footgun)

Do **not** implement disk authorization at the origin using `Authorization: Bearer ...` (or a session cookie) *while leaving CloudFront caching enabled* unless you also:

- include `Authorization` (and/or the cookie) in the cache key **or**
- disable caching for that behavior

Otherwise user A can populate a cached object and user B can receive it without being authorized by the origin.

### Recommended CloudFront policies (disk behaviors)

**Cache policy**

- **Query strings in cache key:** `None` (recommended)
  - Especially important for signed URLs, where the signature is in the query string.
  - CloudFront still validates the signature; it just won’t create a new cache entry per token.
- **Cookies in cache key:** `None`
  - Signed cookies are validated at edge; including them destroys cache efficiency.
- **Headers in cache key:**
  - Prefer `None`, *except* when required for correctness (see CORS note below).
  - **Do not** include `Range` in the cache key for CloudFront. CloudFront can satisfy later byte-range requests from cache without varying the cache key by `Range`, and varying by `Range` will usually explode cache cardinality for random access.

**Origin request policy**

Forward only what S3 needs for correct partial responses:

- `Range` (required for efficient streaming; otherwise S3 will return the entire object)
- `If-Range` (optional; useful for resumable requests and ETag-based validation)

**CORS note (cache correctness)**

If you set `Access-Control-Allow-Origin` dynamically (echoing the incoming `Origin`), then you must vary by `Origin`:

- include the `Origin` header in the cache key **or**
- don’t echo; instead send a fixed `Access-Control-Allow-Origin: https://app.example.com`

For disk streaming, prefer a fixed allowlist to avoid `Origin` cache fragmentation.

### Range caching behavior and `CHUNK_SIZE`

Browsers will request many distinct `Range` values.

For CloudFront specifically, keep the cache key minimal (path + version), and rely on CloudFront’s built-in byte-range caching behavior (see also [`docs/17-range-cdn-behavior.md`](../17-range-cdn-behavior.md)).

On the client side, it is still worth using a fixed `CHUNK_SIZE` to reduce redundant downloads and improve your local caching hit rate (OPFS/sparse cache):

- Choose a fixed `CHUNK_SIZE` in the client (default: **1 MiB**; tune if needed).
- Align all requested ranges to `CHUNK_SIZE` boundaries.

Example alignment logic (matches the approach in `StreamingDisk` in `docs/05-storage-subsystem.md`):

```
chunk_start = floor(byte_offset / CHUNK_SIZE) * CHUNK_SIZE
chunk_end   = ceil(byte_end / CHUNK_SIZE)   * CHUNK_SIZE
Range: bytes={chunk_start}-{chunk_end-1}
```

This makes the client-side “fetch unit” stable, which reduces overlap when different reads land near each other.

---

## D) Headers policy (CORS + CORP + COEP compatibility)

### Why headers matter here

- `Range` is **not** a CORS-safelisted request header → cross-origin `fetch()` with `Range` triggers a **preflight `OPTIONS`**.
- `Authorization` is also **not** safelisted → if you send it, you trigger preflight as well.
- The main app typically needs **COOP/COEP** to enable `SharedArrayBuffer`. With `Cross-Origin-Embedder-Policy: require-corp`, cross-origin resources must be delivered with **CORS** and/or **CORP** headers that allow them.

### Main app: COOP/COEP (for `SharedArrayBuffer`)

The emulator page itself (HTML + any workers/scripts that need `SharedArrayBuffer`) is typically served with:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Disk byte responses do not need to set COOP/COEP, but they must be **compatible** with the app’s COEP policy:

- same-origin disk responses are always fine
- cross-origin disk responses must be CORS-enabled and/or send a permissive CORP header (see below)

### CloudFront Response Headers Policy (disk objects)

Attach a response headers policy to the `/users/*` and `/public/*` cache behaviors with:

#### Byte-stability / anti-transform headers (required for `Range`)

Because disk streaming uses byte offsets, intermediaries **must not** change the wire representation.
Ensure the disk object responses include:

```
Cache-Control: no-transform
Content-Encoding: identity
Content-Type: application/octet-stream
X-Content-Type-Options: nosniff
```

Notes:

- `Cache-Control: no-transform` is defence-in-depth; it tells CDNs/proxies not to apply compression or other transforms.
- Avoid any “automatic compression” features on these cache behaviors. For CloudFront, disable compression for disk behaviors (or ensure it is not applied to `application/octet-stream`).
- `Content-Type` can be set on the S3 object metadata (recommended) or overridden at the edge.
- `Content-Encoding` should be absent or `identity`. Do **not** serve disks as `gzip`/`br`.

#### CORS (for cross-origin disk fetches)

If using signed cookies across origins, you must allow credentials (and you must not use `*` for allow-origin):

```
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type
Access-Control-Allow-Credentials: true
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Content-Encoding
Access-Control-Max-Age: 86400
```

If you are using signed URLs and do not need cookies, you can use:

```
Access-Control-Allow-Origin: *
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag, Content-Encoding
Access-Control-Max-Age: 86400
```

Note: Omit `Access-Control-Allow-Credentials` unless you need credentialed requests; browsers only accept `Access-Control-Allow-Credentials: true`.

#### CORP (for COEP)

Recommended for app+cdn on subdomains of the same registrable domain:

```
Cross-Origin-Resource-Policy: same-site
```

If your app truly is on a different “site” (different eTLD+1), you’ll need:

```
Cross-Origin-Resource-Policy: cross-origin
```

Do **not** use `same-origin` unless the disk responses are same-origin with the app.

### S3 CORS configuration (when CloudFront forwards preflight)

If CloudFront forwards browser preflights (`OPTIONS`) to S3, the bucket must allow them.

S3 CORS rules are evaluated using:

- `Origin`
- `Access-Control-Request-Method`
- `Access-Control-Request-Headers` (e.g. `range, if-range, authorization`)

A permissive starting point for a single app origin:

```json
[
  {
    "AllowedOrigins": ["https://app.example.com"],
    "AllowedMethods": ["GET", "HEAD", "OPTIONS"],
    "AllowedHeaders": ["Range", "If-Range", "If-None-Match", "If-Modified-Since", "Authorization", "Content-Type", "Origin"],
    "ExposeHeaders": ["Accept-Ranges", "Content-Range", "Content-Length", "ETag", "Content-Encoding"],
    "MaxAgeSeconds": 86400
  }
]
```

Notes:

- If you use **signed cookies** and need credentials, you must use a specific `AllowedOrigins` value (no `*`).
- If you have multiple app origins, either list them explicitly or handle CORS at CloudFront with fixed allow-origins per distribution.

### Preflight requirements (explicit)

If the browser is cross-origin to the disk URL, a typical preflight looks like:

```
OPTIONS /users/123/disk.raw
Origin: https://app.example.com
Access-Control-Request-Method: GET
Access-Control-Request-Headers: range
```

If you also send `If-Range` (recommended for resumable reads), it becomes:

```
Access-Control-Request-Headers: range, if-range
```

If you send `Authorization`, it becomes:

```
Access-Control-Request-Headers: range, authorization
```

If you also send `If-Range`, include it as well:

```
Access-Control-Request-Headers: range, if-range, authorization
```

Your CloudFront behavior must allow `OPTIONS`, and your response headers must allow `Range` (and `Authorization` if used), otherwise the browser will fail before the first byte is fetched.

---

## E) Example backend code: minting signed cookies / signed URLs

Below is a Node.js example using `@aws-sdk/cloudfront-signer`. The important bits are:

- Use a **custom policy** for signed cookies so you can scope access to a user prefix.
- Set cookies for a domain the browser will send to CloudFront (e.g. `Domain=.example.com` for `cdn.example.com`).

```js
import fs from "node:fs";
import { getSignedCookies, getSignedUrl } from "@aws-sdk/cloudfront-signer";

const keyPairId = process.env.CLOUDFRONT_KEY_PAIR_ID;
const privateKey = fs.readFileSync(process.env.CLOUDFRONT_PRIVATE_KEY_PEM, "utf8");

function epochSeconds(date) {
  return Math.floor(date.getTime() / 1000);
}

// Recommended: signed cookies for many Range requests.
export function mintDiskSignedCookies({ uid, ttlSeconds }) {
  const expires = new Date(Date.now() + ttlSeconds * 1000);
  const resource = `https://cdn.example.com/users/${uid}/*`;

  const policy = JSON.stringify({
    Statement: [
      {
        Resource: resource,
        Condition: {
          DateLessThan: { "AWS:EpochTime": epochSeconds(expires) }
        }
      }
    ]
  });

  // Returns { "CloudFront-Policy": "...", "CloudFront-Signature": "...", "CloudFront-Key-Pair-Id": "..." }
  return getSignedCookies({ keyPairId, privateKey, policy });
}

// Alternative: signed URL (token-in-URL).
export function mintDiskSignedUrl({ uid, ttlSeconds }) {
  const url = `https://cdn.example.com/users/${uid}/disk.raw`;
  const expires = new Date(Date.now() + ttlSeconds * 1000);
  return getSignedUrl({ url, keyPairId, privateKey, dateLessThan: expires });
}
```

Example cookie attributes to use when setting them from your backend:

- `Secure`
- `HttpOnly`
- `Path=/users/<uid>/` (limits where the cookies are sent)
- `Domain=.example.com` (so `app.example.com` can set cookies for `cdn.example.com`)
- `SameSite=Lax` (works for same-site subdomains; for cross-site you may need `SameSite=None; Secure` but this is frequently blocked)

---

## Minimal “do this” checklist

1. S3 bucket is private + blocked public access.
2. CloudFront distribution has S3 origin with **OAC** and bucket policy allows only that distribution.
3. `/users/*` behavior:
   - viewer restriction enabled (trusted key group)
   - allowed methods: `GET, HEAD, OPTIONS`
   - origin request policy forwards `Range` (and optionally `If-Range`)
   - response headers policy sets CORS + `Cross-Origin-Resource-Policy`
4. Backend mints **signed cookies** scoped to `https://cdn.example.com/users/<uid>/*`.
5. Client aligns reads to a fixed `CHUNK_SIZE` to reduce redundant downloads and improve local caching behavior (independent of CloudFront’s cache key).
6. If disks are large enough that CloudFront size limits become relevant, enforce **range-only** access (for non-`HEAD` size discovery, use `Range: bytes=0-0`) or publish them as multiple chunk objects (see [CloudFront cacheable size limits](#cloudfront-cacheable-size-limits-range-helps-chunking-is-still-a-good-default)).

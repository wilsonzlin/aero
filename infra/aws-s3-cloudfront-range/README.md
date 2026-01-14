# AWS S3 + CloudFront (HTTP Range) Terraform module

Reference infrastructure-as-code for hosting **large, immutable disk images** in a **private S3 bucket** and delivering them through a **CloudFront distribution** tuned for **HTTP Range requests** (partial downloads) and caching.

This is AWS-specific (uses CloudFront **Origin Access Control (OAC)**, not legacy OAI).

For a fully local Range + CORS validation setup (no AWS required), see
[`infra/local-object-store/README.md`](../local-object-store/README.md).

If you need a backend for **user uploads** (S3 multipart upload + CloudFront signed cookies/URLs),
see the reference implementation at [`services/image-gateway/`](../../services/image-gateway/).

## Architecture

- **S3 bucket (private)**
  - Block all public access.
  - Default server-side encryption (SSE-S3 by default; optional SSE-KMS).
  - Optional versioning.
  - Lifecycle rules:
    - Abort incomplete multipart uploads (important for very large uploads).
    - Optional transition/expiration knobs.
  - Optional CORS configuration (secure default requires an explicit allowlist of origins; can be disabled if CORS is handled fully at CloudFront).
- **CloudFront distribution**
  - Origin is the S3 bucket, access controlled by **OAC**.
  - `http_version = http2and3` (HTTP/2 + HTTP/3).
  - Cache behavior for `/images/*`:
    - `GET`, `HEAD`, `OPTIONS`
    - Only `GET`/`HEAD` responses are cached by default (OPTIONS preflight is forwarded to origin; browsers cache via `Access-Control-Max-Age`).
    - Compression disabled (disk images are already compressed or not worth compressing).
    - Origin request policy forwards headers needed for **CORS preflight** (unless edge-handled preflight is enabled).
    - `Range`/`If-Range` are forwarded to S3 so byte-range reads work, but **are not included in the CloudFront cache key** (avoids cache fragmentation for random-access workloads).
    - Two cache policies: `immutable` (long TTL) vs `mutable` (short TTL), selectable via variable.
    - Optional CloudFront **response headers policy** for injecting CORS headers and optional security headers (e.g. `Cross-Origin-Resource-Policy`) at the edge.
    - Optional CloudFront **Function** to answer CORS preflight (`OPTIONS`) at the edge for `/images/*`, avoiding OPTIONS to S3.

## CloudFront object size limits (important for very large disks)

CloudFront enforces a maximum object size per URL. If you plan to serve **very large** disk images, ensure your objects stay within CloudFront’s limits, or publish images as **chunk objects** instead of a single monolithic file.

For background and practical guidance (including a chunked-object strategy), see:

- [`docs/17-range-cdn-behavior.md`](../../docs/17-range-cdn-behavior.md)

## SSE-KMS note (if using `kms_key_arn`)

If you set `kms_key_arn` to enable SSE-KMS encryption for the bucket, ensure the referenced KMS key policy allows decrypt access for reads originating from this CloudFront distribution (otherwise CloudFront will receive `AccessDenied` when fetching objects from S3).

## Prerequisites

- Terraform **0.13+**
- Optional: [`tflint`](https://github.com/terraform-linters/tflint) (this repo pins config via `.tflint.hcl`)
- AWS credentials configured for Terraform (env vars, profile, or IAM role)
- AWS provider plugin will be downloaded during `terraform init`

## CI validation (Terraform + tflint)

CI runs:

- `terraform fmt -check -recursive`
- `terraform init -backend=false`
- `terraform validate`
- `tflint` (AWS ruleset)

To reproduce locally:

```bash
cd infra/aws-s3-cloudfront-range
terraform fmt -check -recursive
terraform init -backend=false -input=false -lockfile=readonly
terraform validate
tflint --init
tflint
```

Tip: from the repo root you can also run the full CI reproduction helper:

```bash
bash ./scripts/ci/check-iac.sh
# Or: just check-iac
```

## Quick start

```bash
cd infra/aws-s3-cloudfront-range
cp terraform.tfvars.example terraform.tfvars

# Edit terraform.tfvars, then:
terraform fmt
terraform init
terraform validate
terraform apply
```

After apply, Terraform prints:

- CloudFront distribution domain name
- S3 bucket name
- Recommended image base URL

## Example configuration

See `terraform.tfvars.example`.

## Uploading an image

Upload disk images under the `images/` prefix (so they are reachable at `/images/...`).

### Immutable (recommended)

Use a **versioned path** (e.g. `windows7-v1.img`, `windows7/2026-01-10/disk.img`, etc) and set a long cache-control:

```bash
aws s3 cp ./windows7.img "s3://YOUR_BUCKET/images/windows7-v1.img" \
  --content-type "application/octet-stream" \
  --cache-control "public, max-age=31536000, immutable, no-transform"
```

### Mutable

If you overwrite the same key (not recommended for very large artifacts), set a short cache-control and use the module’s `cache_policy_mode = "mutable"`:

```bash
aws s3 cp ./windows7.img "s3://YOUR_BUCKET/images/windows7-latest.img" \
  --content-type "application/octet-stream" \
  --cache-control "public, max-age=60, no-transform"
```

If clients might have cached old bytes, consider changing the object key instead of overwriting.

## Verifying Range delivery

Replace `BASE_URL` with the module output `image_base_url` (for example, `https://d123.cloudfront.net/images/`).

### 1) Verify object headers (HEAD)

```bash
curl -I "${BASE_URL}windows7-v1.img"
```

Look for headers similar to:

- `HTTP/2 200` (or `HTTP/3 200`)
- `Accept-Ranges: bytes`
- `Content-Length: ...`

### 2) Verify partial content (Range request)

```bash
curl -v -H "Range: bytes=0-1048575" "${BASE_URL}windows7-v1.img" -o /dev/null
```

Expected:

- `HTTP/2 206` (Partial Content)
- `Content-Range: bytes 0-1048575/…`

### 3) Verify CloudFront caching

Run the same request twice. On a cache hit you should see:

```bash
curl -I "${BASE_URL}windows7-v1.img" | grep -i '^x-cache:'
# X-Cache: Hit from cloudfront
```

The very first request is often `Miss from cloudfront`. Subsequent requests from the same edge location should become `Hit`.

### 4) Benchmark Range throughput + cache hit rate (recommended)

For deeper performance and cache analysis, use the repo’s Range harness:

```bash
# From the repo root:
node tools/range-harness/index.js \
  --url "${BASE_URL}windows7-v1.img" \
  --chunk-size 1048576 \
  --count 32 \
  --concurrency 4 \
  --passes 2 \
  --random \
  --seed 12345
```

Pass 1 typically warms the CDN. Pass 2 should skew toward `X-Cache: Hit from cloudfront` if byte-range caching is configured correctly.

## Custom domain / “same-origin” notes

This module can optionally attach one or more `custom_domain_names` (CNAMEs) to the CloudFront distribution.

### DNS

- **CNAME**: `images.example.com` → `<distribution_domain_name>`
- **Route53 alias (recommended)**: Alias `A/AAAA` to the CloudFront distribution domain

If you set `custom_domain_names`, you must also set `acm_certificate_arn` (an ACM certificate **in `us-east-1`**, as required by CloudFront).

### CORS

- If your web app loads disk image bytes from a **different origin** (e.g. app at `https://example.com`, images at `https://images.example.com`), the browser will require **CORS**.
- Fetching with `Range` headers typically triggers **CORS preflight** (`OPTIONS`) requests.
- Configure `cors_allowed_origins` accordingly.

If you allow **multiple** origins and rely on **S3** to emit CORS headers (`enable_edge_cors = false`), be aware that S3 will echo `Access-Control-Allow-Origin` based on the incoming `Origin` header, and CloudFront may cache that header along with the object. For multi-origin setups, prefer `enable_edge_cors = true` (and optionally `enable_edge_cors_preflight = true`) so CloudFront can add consistent CORS headers at the edge without fragmenting the cache.

If you truly need *same-origin* (same scheme/host/port as your app), you usually need to serve both your app and `/images/*` through the **same CloudFront distribution**. You can still use this module as a reference for the S3 + OAC + caching pieces.

#### Edge-handled preflight (`OPTIONS`) for `/images/*` (optional)

When `enable_edge_cors_preflight = true`, a CloudFront Function responds to CORS preflight requests for `/images/*` at the edge (viewer request), reducing origin load and latency for the first `Range` fetch.

Behavior summary:

- Only handles CORS preflight requests (requests with `Origin` and `Access-Control-Request-Method`).
- Validates `Origin` against `cors_allowed_origins` (exact origin match like `https://app.example.com`, or `*`).
- Returns `204` and includes:
  - `Access-Control-Allow-Origin: <origin>`
  - `Access-Control-Allow-Methods: GET,HEAD,OPTIONS`
  - `Access-Control-Allow-Headers: ...` (from `cors_allowed_headers`, must include `Range`)
  - `Access-Control-Allow-Credentials: true|false` (from `cors_allow_credentials`)
  - `Access-Control-Max-Age: <seconds>` (from `cors_max_age_seconds`)
  - `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`
- Non-CORS `OPTIONS` requests to `/images/*` return `404` (to avoid forwarding `OPTIONS` to S3).

Example:

```bash
curl -i -X OPTIONS "https://$CLOUDFRONT_DOMAIN/images/example.bin" \
  -H "Origin: https://app.example.com" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: Range"
```

Expected response headers (abridged):

```
HTTP/2 204
access-control-allow-origin: https://app.example.com
access-control-allow-methods: GET,HEAD,OPTIONS
access-control-allow-headers: Range,If-Range,Content-Type,If-None-Match,If-Modified-Since
access-control-allow-credentials: false
access-control-max-age: 86400
vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers
```

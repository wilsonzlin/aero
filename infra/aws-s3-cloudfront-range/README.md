# AWS S3 + CloudFront (HTTP Range) Terraform module

Reference infrastructure-as-code for hosting **large, immutable disk images** in a **private S3 bucket** and delivering them through a **CloudFront distribution** tuned for **HTTP Range requests** (partial downloads) and caching.

This is AWS-specific (uses CloudFront **Origin Access Control (OAC)**, not legacy OAI).

For a fully local Range + CORS validation setup (no AWS required), see
[`infra/local-object-store/README.md`](../local-object-store/README.md).

## Architecture

- **S3 bucket (private)**
  - Block all public access.
  - Default server-side encryption (SSE-S3 by default; optional SSE-KMS).
  - Optional versioning.
  - Lifecycle rules:
    - Abort incomplete multipart uploads (important for very large uploads).
    - Optional transition/expiration knobs.
  - Optional CORS configuration (secure default requires an explicit allowlist of origins).
- **CloudFront distribution**
  - Origin is the S3 bucket, access controlled by **OAC**.
  - `http_version = http2and3` (HTTP/2 + HTTP/3).
  - Cache behavior for `/images/*`:
    - `GET`, `HEAD`, `OPTIONS`
    - Only `GET`/`HEAD` responses are cached by default (OPTIONS preflight is forwarded to origin; browsers cache via `Access-Control-Max-Age`).
    - Compression disabled (disk images are already compressed or not worth compressing).
    - Origin request policy forwards headers needed for **CORS preflight**.
    - Cache policy includes `Range`/`If-Range` in the cache key for HTTP Range streaming + caching.
    - Two cache policies: `immutable` (long TTL) vs `mutable` (short TTL), selectable via variable.
    - Optional CloudFront **response headers policy** for injecting CORS headers at the edge.

## Prerequisites

- Terraform **0.13+**
- AWS credentials configured for Terraform (env vars, profile, or IAM role)
- AWS provider plugin will be downloaded during `terraform init`

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
  --cache-control "public, max-age=31536000, immutable"
```

### Mutable

If you overwrite the same key (not recommended for very large artifacts), set a short cache-control and use the module’s `cache_policy_mode = "mutable"`:

```bash
aws s3 cp ./windows7.img "s3://YOUR_BUCKET/images/windows7-latest.img" \
  --content-type "application/octet-stream" \
  --cache-control "public, max-age=60"
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

If you truly need *same-origin* (same scheme/host/port as your app), you usually need to serve both your app and `/images/*` through the **same CloudFront distribution**. You can still use this module as a reference for the S3 + OAC + caching pieces.

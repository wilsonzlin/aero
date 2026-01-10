# Range Harness (HTTP Range + CDN cache probing)

`tools/range-harness/` contains a small, dependency-free Node.js CLI for validating and benchmarking HTTP `Range` requests against large disk-image URLs (S3 / CloudFront / MinIO / etc).

It is useful for:

- Verifying that a URL correctly supports byte-range reads (`206 Partial Content` + correct `Content-Range`)
- Measuring latency/throughput for different `Range` chunk sizes and concurrency levels
- Observing CDN cache behavior via response headers like `X-Cache` (CloudFront)

## Requirements

- Node.js **18+**

No external npm dependencies are required.

## Usage

```bash
node tools/range-harness/index.js --url <URL> \
  --chunk-size 8388608 \
  --count 32 \
  --concurrency 4 \
  --random
```

### Machine-readable output (JSON)

```bash
node tools/range-harness/index.js --url <URL> --random --json > range-results.json
```

### Options

- `--url <URL>` (required): HTTP/HTTPS URL to probe
- `--chunk-size <bytes>`: size of each `Range` request (default: `8388608` = 8MiB)
- `--count <N>`: number of requests to perform (default: `32`)
- `--concurrency <N>`: number of in-flight requests (default: `4`)
- `--passes <N>`: repeat the same range plan `N` times (default: `1`). Useful for verifying that a CDN caches byte ranges (pass 1 is typically `Miss`, pass 2 should become `Hit` if caching is configured).
- `--header "Name: value"`: extra request header (repeatable). Useful for authenticated endpoints.
- `--json`: emit machine-readable JSON (suppresses the per-request text output)
- `--strict`: exit non-zero if any request fails correctness checks (bad `Content-Range`, `416`, etc)
- `--random`: pick random aligned chunks
- `--sequential`: walk aligned chunks from the start (wraps around)

## Example: Direct S3 object URL

If your object is public:

```bash
node tools/range-harness/index.js \
  --url "https://my-bucket.s3.amazonaws.com/windows7.img" \
  --chunk-size 8388608 --count 32 --concurrency 4 --random
```

For private objects, use a **pre-signed URL** and paste it into `--url`.

## Example: Authenticated endpoint (Authorization header)

If your disk image URL requires an `Authorization` header (or any other custom header), pass it via `--header`:

```bash
node tools/range-harness/index.js \
  --url "https://example.com/private/windows7.img" \
  --header "Authorization: Bearer eyJ..." \
  --chunk-size 8388608 --count 32 --concurrency 4 --random
```

## Example: CloudFront distribution URL

```bash
node tools/range-harness/index.js \
  --url "https://d111111abcdef8.cloudfront.net/windows7.img" \
  --chunk-size 8388608 --count 64 --concurrency 8 --random
```

CloudFront often includes an `X-Cache` header like:

- `Hit from cloudfront`
- `Miss from cloudfront`
- `RefreshHit from cloudfront`

The harness will print both an aggregated hit/miss count and an exact-value breakdown.

## Example: CDN cache hit verification (repeat same ranges)

To verify that your CDN is caching byte-range responses, repeat the same range plan multiple times with `--passes`.

Example: request the same 32 chunks twice (pass 1 warms the cache; pass 2 should skew toward `Hit` if caching is working):

```bash
node tools/range-harness/index.js \
  --url "https://d111111abcdef8.cloudfront.net/windows7.img" \
  --chunk-size 1048576 --count 32 --passes 2 --concurrency 4 --random
```

The CLI prints per-pass summaries so you can compare pass 1 vs pass 2 `X-Cache` counts.

## Example: Local MinIO

One easy local setup is via Docker:

```bash
docker run --rm -p 9000:9000 -p 9001:9001 \
  -e "MINIO_ROOT_USER=minioadmin" \
  -e "MINIO_ROOT_PASSWORD=minioadmin" \
  quay.io/minio/minio server /data --console-address ":9001"
```

Then upload an object (replace paths as needed). If you have the MinIO client (`mc`) installed:

```bash
mc alias set local http://localhost:9000 minioadmin minioadmin
mc mb local/images
mc anonymous set download local/images
mc cp ./windows7.img local/images/windows7.img
```

Probe it:

```bash
node tools/range-harness/index.js \
  --url "http://localhost:9000/images/windows7.img" \
  --chunk-size 4194304 --count 32 --concurrency 4 --sequential
```

## Interpreting output

### Per-request lines

Each request prints a line including:

- The requested `Range` (e.g. `bytes=0-8388607`)
- `status` (expected: `206`; `200` indicates the server ignored the `Range`)
- `content-range` (expected to match the requested offsets when `status=206`)
- `bytes` read
- `time` (end-to-end latency for downloading the response body)
- `rate` (per-request throughput, derived from `bytes/time`)
- `x-cache` (if present)
- `WARN=...` when the harness detects an issue (bad/missing `Content-Range`, `416`, etc)

If a server ignores `Range` and returns `200`, the harness will warn and abort the response body read early (after the expected chunk size) to avoid accidentally downloading a multi-GB disk image.

### Summary

The summary includes:

- Average + median latency across all requests
- Aggregate throughput across the full run (total bytes / wall time), which captures the effect of concurrency
- Status code breakdown
- `X-Cache` hit/miss breakdown (plus exact values)

Example (trimmed):

```text
URL: https://d111111abcdef8.cloudfront.net/windows7.img
Config: chunkSize=8.00MiB count=32 concurrency=4 passes=1 mode=random
...
Summary
-------
Requests: 32 ok=32 withWarnings=0
Latency: avg=120ms median=98ms
Throughput: bytes=256MiB wall=6.50s aggregate=39.4MiB/s
Status codes: 206:32
X-Cache: hit=28 miss=4 other=0 missing=0
```

## Related tools

- For strict correctness + CORS/COEP validation (CI-friendly), see [`tools/disk-streaming-conformance/`](../disk-streaming-conformance/README.md).

## Recommended chunk-size starting points (Aero)

For large disk images, a good starting range for benchmarking is:

- **1MiB** (`1048576`) to match common `StreamingDisk` chunk sizes (good cacheability / less over-fetch)
- **4MiB** (`4194304`) if you expect high RTT / want to reduce request rate
- **8MiB** (`8388608`) as a balanced default for pure throughput benchmarking
- **16MiB** (`16777216`) when optimizing for peak throughput on low-latency links

Typical workflow:

1. Start with **1MiB** (realistic client behavior) or **8MiB** (throughput-focused) and `--concurrency 4`
2. Increase concurrency until throughput stops improving (or latencies become too spiky)
3. Try 1MiB vs 8MiB vs 16MiB to see if the bottleneck is per-request overhead vs bandwidth

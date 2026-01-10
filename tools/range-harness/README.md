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

### Options

- `--url <URL>` (required): HTTP/HTTPS URL to probe
- `--chunk-size <bytes>`: size of each `Range` request (default: `8388608` = 8MiB)
- `--count <N>`: number of requests to perform (default: `32`)
- `--concurrency <N>`: number of in-flight requests (default: `4`)
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

## Recommended chunk-size starting points (Aero)

For large disk images, a good starting range for benchmarking is:

- **4MiB** (`4194304`) if you expect high RTT / want finer-grained I/O
- **8MiB** (`8388608`) as a balanced default
- **16MiB** (`16777216`) when optimizing for peak throughput on low-latency links

Typical workflow:

1. Start with **8MiB** chunks and `--concurrency 4`
2. Increase concurrency until throughput stops improving (or latencies become too spiky)
3. Try 4MiB vs 16MiB to see if the bottleneck is per-request overhead vs bandwidth

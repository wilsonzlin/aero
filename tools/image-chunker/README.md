# `aero-image-chunker`

Chunks a large **raw disk image** into fixed-size objects and uploads them to an **S3-compatible object store** (AWS S3, MinIO, etc.), along with a `manifest.json`.

This enables CDN-friendly delivery without relying on HTTP `Range` requests: clients fetch `manifest.json`, then fetch `chunks/00000000.bin`, `chunks/00000001.bin`, … as needed.

## Build

```bash
cargo build --release --manifest-path tools/image-chunker/Cargo.toml
```

Binary path:

```bash
./tools/image-chunker/target/release/aero-image-chunker
```

## Publish (AWS S3)

Credentials are resolved via the standard AWS SDK chain (env vars, `~/.aws/config`, profiles, IAM role, etc.).

Note: `--chunk-size` must be a multiple of **512 bytes** (ATA sector size).

Example:

```bash
./tools/image-chunker/target/release/aero-image-chunker publish \
  --file ./disk.img \
  --bucket my-bucket \
  --prefix images/<imageId>/<version>/ \
  --image-id <imageId> \
  --image-version <version> \
  --chunk-size 8388608 \
  --region us-east-1 \
  --concurrency 8
```

`--image-id`/`--image-version` can be omitted if `--prefix` already ends with `/<imageId>/<version>/`.

Artifacts uploaded under the given prefix:

- `chunks/00000000.bin`, `chunks/00000001.bin`, …
- `manifest.json`
- `meta.json` (unless `--no-meta`)

## Publish (MinIO)

Assuming MinIO is running locally on `http://localhost:9000` and your bucket already exists:

```bash
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin

./tools/image-chunker/target/release/aero-image-chunker publish \
  --file ./disk.img \
  --bucket my-bucket \
  --prefix images/<imageId>/<version>/ \
  --image-id <imageId> \
  --image-version <version> \
  --chunk-size 8388608 \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --concurrency 8
```

## Verifying with `curl`

If your bucket/prefix is publicly readable (or your local MinIO is configured to allow anonymous GETs), you can verify that the manifest and some chunks exist:

```bash
curl -fSs http://localhost:9000/my-bucket/images/<imageId>/<version>/manifest.json | head
curl -fSsI http://localhost:9000/my-bucket/images/<imageId>/<version>/chunks/00000000.bin
curl -fSsI http://localhost:9000/my-bucket/images/<imageId>/<version>/chunks/00000001.bin
```

If your objects are private, generate a presigned URL and `curl` that instead:

```bash
aws s3 presign s3://my-bucket/images/<imageId>/<version>/manifest.json --expires-in 600
```

Then:

```bash
curl -fSs "<presigned-url>"
```

## Manifest format

`manifest.json` is a single JSON document that describes the chunked image (see [`docs/18-chunked-disk-image-format.md`](../../docs/18-chunked-disk-image-format.md)):

- `schema`: `aero.chunked-disk-image.v1`
- `imageId` and `version`: identifiers for the image/version
- `mimeType`: MIME type for chunk objects
- `totalSize`: original file size in bytes
- `chunkSize`: the chosen chunk size in bytes
- `chunkCount`: total number of chunk objects
- `chunkIndexWidth`: decimal zero-padding width (8)
- `chunks[i].sha256`: per-chunk checksum (present when `--checksum sha256`, omitted when `--checksum none`)

Example (abridged):

```json
{
  "schema": "aero.chunked-disk-image.v1",
  "imageId": "win7-sp1-x64",
  "version": "sha256-...",
  "mimeType": "application/octet-stream",
  "totalSize": 123456789,
  "chunkSize": 8388608,
  "chunkCount": 15,
  "chunkIndexWidth": 8,
  "chunks": [
    {
      "size": 8388608,
      "sha256": "…"
    }
  ]
}
```

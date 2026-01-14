# `aero-image-chunker`

Chunks a disk image (raw/qcow2/vhd/aerosparse) into fixed-size objects representing the **guest-visible logical disk bytes**, and uploads them to an **S3-compatible object store** (AWS S3, MinIO, etc.), along with a `manifest.json`.

This enables CDN-friendly delivery without relying on HTTP `Range` requests: clients fetch `manifest.json`, then fetch `chunks/00000000.bin`, `chunks/00000001.bin`, … as needed.

Important: chunks represent the **guest-visible raw byte stream** (what the VM sees), not
necessarily the container file bytes. For a raw `.img`, those are the same; for sparse/container
formats (qcow2/vhd/…), the chunker must read the image as a virtual disk and treat unallocated
regions as zeros.

See also: [`docs/18-chunked-disk-image-format.md`](../../docs/18-chunked-disk-image-format.md).

Implementation note (for tooling authors): on native targets, prefer using the `aero-storage`
stack (`aero_storage::StorageBackend` via a filesystem backend such as `aero_storage::FileBackend`,
then `aero_storage::DiskImage::open_auto`) so tools reuse the same disk format support as the
emulator.

## Build

```bash
cargo build --release --locked --manifest-path tools/image-chunker/Cargo.toml
```

Binary path:

```bash
./tools/image-chunker/target/release/aero-image-chunker
```

## Publish (AWS S3)

Credentials are resolved via the standard AWS SDK chain (env vars, `~/.aws/config`, profiles, IAM role, etc.).

Note: `--chunk-size` must be a multiple of **512 bytes** (ATA sector size). The default is **4 MiB** (`4194304`).

### Input format (`--format`)

By default, the tool uses `--format auto` (format detection) and treats unknown images as `raw`.

To publish other disk image formats, use `--format`:

- `raw`: treat the input as a raw disk image (`.img`).
- `qcow2`: open QCOW2 v2/v3 and publish the *expanded disk view*.
- `vhd`: open VHD (fixed/dynamic) and publish the *expanded disk view*.
- `aerosparse`: open Aero sparse (`AEROSPAR`) and publish the *expanded disk view*.
- `auto`: auto-detect the format from magic values.

For container formats (`qcow2`, `vhd`, `aerosparse`), the published chunks correspond to the **logical disk byte stream** (what the guest sees), not the on-disk container file bytes.

### CDN-ready HTTP metadata (Cache-Control, Content-Encoding)

`aero-image-chunker publish` sets object metadata so the uploaded artifacts are ready to serve through a CDN without extra configuration:

- Chunks (`chunks/*.bin`):
  - `Content-Type: application/octet-stream`
  - `Content-Encoding: identity`
  - `Cache-Control: public, max-age=31536000, immutable, no-transform`
- JSON (`manifest.json`, `meta.json`):
  - `Content-Type: application/json`
  - `Cache-Control: public, max-age=31536000, immutable`

These defaults match [`docs/18-chunked-disk-image-format.md`](../../docs/18-chunked-disk-image-format.md) and can be overridden with:

- `--cache-control-chunks <value>`
- `--cache-control-manifest <value>`
- `--cache-control-latest <value>` (only used with `--publish-latest`)

### Example

```bash
./tools/image-chunker/target/release/aero-image-chunker publish \
  --file ./disk.img \
  --bucket my-bucket \
  --prefix images/<imageId>/<version>/ \
  --image-id <imageId> \
  --image-version <version> \
  --chunk-size 4194304 \
  --region us-east-1 \
  --concurrency 8
```

`--image-id` can be omitted if `--prefix` already includes `<imageId>` (either as
`images/<imageId>/` or `images/<imageId>/<version>/`).

`--image-version` can be omitted in two cases:

- When `--compute-version none` (default) and `--prefix` already ends with `/<imageId>/<version>/` (the version is inferred from the prefix).
- When `--compute-version sha256` is enabled (the version is computed as `sha256-<digest>` over the entire **logical disk** content).

If you enable `--compute-version sha256` and also provide `--image-version`, the tool validates they match.

### Example: compute a content-addressed version automatically

This computes `sha256-<digest>` from the input image, uploads under `images/<imageId>/<sha256-...>/`,
and optionally updates `images/<imageId>/latest.json`:

```bash
./tools/image-chunker/target/release/aero-image-chunker publish \
  --file ./disk.img \
  --format raw \
  --bucket my-bucket \
  --prefix images/<imageId>/ \
  --compute-version sha256 \
  --publish-latest \
  --chunk-size 4194304 \
  --region us-east-1
```

Artifacts uploaded under the given prefix:

- `chunks/00000000.bin`, `chunks/00000001.bin`, …
- `manifest.json`
- `meta.json` (unless `--no-meta`)

### Optional: publish a `latest.json` pointer

For public/demo images, you can publish a short-lived pointer file:

```bash
./tools/image-chunker/target/release/aero-image-chunker publish \
  ... \
  --publish-latest
```

This uploads `images/<imageId>/latest.json` with:

- `Cache-Control: public, max-age=60` (default; configurable via `--cache-control-latest`)
- `Content-Type: application/json`

The JSON contains the version and the object key of the versioned `manifest.json`.

## Publish (MinIO)

This repo includes a ready-to-run local MinIO setup at `infra/local-object-store/`.

Start it:

```bash
cd infra/local-object-store
docker compose up -d
```

Then publish to the default bucket (`disk-images`):

```bash
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin

./tools/image-chunker/target/release/aero-image-chunker publish \
  --file ./disk.img \
  --bucket disk-images \
  --prefix images/<imageId>/<version>/ \
  --image-id <imageId> \
  --image-version <version> \
  --chunk-size 4194304 \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --concurrency 8
```

## Verify (manifest + chunks)

After publishing, you can re-download and validate the published artifacts end-to-end:

- `manifest.json` schema and internal consistency
- chunk object existence
- chunk sizes (`Content-Length` and streamed length)
- optional per-chunk `sha256` (streamed hashing)
- optional `latest.json` pointer (if present under `images/<imageId>/latest.json`)

### Example: verify a versioned prefix

```bash
./tools/image-chunker/target/release/aero-image-chunker verify \
  --bucket disk-images \
  --prefix images/<imageId>/<version>/ \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --concurrency 8
```

### Example: quick smoke test (sample a few chunks)

Verify 8 random chunks plus the final chunk:

```bash
./tools/image-chunker/target/release/aero-image-chunker verify \
  --bucket disk-images \
  --prefix images/<imageId>/<version>/ \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --chunk-sample 8
```

To only verify the final chunk (fastest possible smoke check), use `--chunk-sample 0`.

For deterministic sampling (useful in CI), provide a seed:

```bash
./tools/image-chunker/target/release/aero-image-chunker verify \
  --bucket disk-images \
  --prefix images/<imageId>/<version>/ \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --chunk-sample 8 \
  --chunk-sample-seed 123
```

### Example: verify by image root + explicit version

If you prefer to point at an image root prefix (`images/<imageId>/`), provide `--image-version`:

```bash
./tools/image-chunker/target/release/aero-image-chunker verify \
  --bucket disk-images \
  --prefix images/<imageId>/ \
  --image-version <version> \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1
```

### Example: verify the `latest.json` pointer

If you published a `latest.json` pointer (with `publish --publish-latest`), you can also verify the
current latest version by pointing `verify` at the image root prefix without specifying a version:

```bash
./tools/image-chunker/target/release/aero-image-chunker verify \
  --bucket disk-images \
  --prefix images/<imageId>/ \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1
```

### Example: verify using an explicit manifest object key

If you already know the object key of the versioned manifest (for example, from a `latest.json`
pointer), you can skip prefix inference and verify directly:

```bash
./tools/image-chunker/target/release/aero-image-chunker verify \
  --bucket disk-images \
  --manifest-key images/<imageId>/<version>/manifest.json \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1
```

## Verifying with `curl`

If your bucket/prefix is publicly readable (or your local MinIO is configured to allow anonymous GETs), you can verify that the manifest and some chunks exist:

```bash
curl -fSs http://localhost:9000/my-bucket/images/<imageId>/<version>/manifest.json | head
curl -fSsI http://localhost:9000/my-bucket/images/<imageId>/<version>/chunks/00000000.bin
curl -fSsI http://localhost:9000/my-bucket/images/<imageId>/<version>/chunks/00000001.bin
```

In the `-I` output, check that the uploaded objects have the expected CDN-friendly metadata:

- `Cache-Control: ...immutable...`
- for chunks, `Content-Encoding: identity` and `Cache-Control: ...no-transform`

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
- `totalSize`: logical disk size in bytes (`VirtualDisk::capacity_bytes()`), which may differ from the on-disk container file size for sparse formats
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
  "chunkSize": 4194304,
  "chunkCount": 30,
  "chunkIndexWidth": 8,
  "chunks": [
    {
      "size": 4194304,
      "sha256": "…"
    }
  ]
}
``` 

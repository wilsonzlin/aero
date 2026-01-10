# Disk Images: Local vs Streaming (HTTP Range)

## Overview

Aero can use **raw disk images** in two ways:

1. **Local images**: you provide a file (or one is generated) and Aero stores it in browser storage (OPFS).
2. **Streaming images**: you provide a **URL** to a remote raw image and Aero reads it using **HTTP Range requests**, caching only the blocks it actually touches.

Streaming is essential for very large images (20GB+) because it avoids a full upfront download.

## Legal / Responsible Use (Important)

You must only use disk images that you **own** or are otherwise **licensed to use**.

Do **not** use Aero’s streaming support to access or distribute pirated Windows installers or disk images. If you don’t have explicit rights to the content, don’t point Aero at it.

## Streaming images: server requirements

To stream a remote image, the server **must** support byte-range requests:

- `Accept-Ranges: bytes`
- `Content-Length` on `HEAD`/`GET`
- Correct `206 Partial Content` responses to `Range: bytes=start-end`
- Correct `Content-Range: bytes start-end/total`

Notes:

- Some servers disallow `HEAD`. Aero can fall back to a small `Range: bytes=0-0` probe,
  but that requires a valid `Content-Range` header (and appropriate CORS exposure).

### CORS headers (browser requirement)

Browsers will block cross-origin range reads unless the server is configured for CORS.

For a self-contained local setup (MinIO + optional reverse proxy) to validate Range + CORS behavior, see:
[`infra/local-object-store/README.md`](../infra/local-object-store/README.md).

At minimum, the response should include headers similar to:

```
Access-Control-Allow-Origin: https://your-aero-origin.example
Access-Control-Allow-Headers: Range
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length
```

Notes:

- `Access-Control-Allow-Origin: *` is acceptable for public, non-credentialed access.
- `Content-Range` is not a “simple” header, so it must be **exposed** if the UI needs to read it.

## Streaming images: caching behavior

The streaming backend downloads data in fixed-size **blocks** (for example, 1 MiB).

- On a cache miss, Aero fetches the required block with an HTTP Range request.
- Blocks are stored locally and reused on subsequent runs.
- A cache size limit can be configured; when exceeded, least-recently-used blocks are evicted.

## Security / UX expectations

Remote image support should be gated behind explicit user action:

- A dedicated “Use remote image” toggle (off by default).
- A URL input field.
- A clear warning that remote images can be untrusted and may leak request metadata to the host.
- A cache/progress indicator (downloaded blocks, cache size, etc.).

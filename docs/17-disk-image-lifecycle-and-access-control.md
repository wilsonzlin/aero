# 17 - Disk Image Lifecycle and Access Control (Hosted Service)

## Overview

The open-source Aero emulator can read disk bytes from a URL using HTTP `Range` requests (see the streaming design in the storage subsystem). A **hosted Aero service** has additional requirements implied by [Legal & Licensing Considerations](./13-legal-considerations.md):

- The service **must not provide Windows media**; users must upload/import their own.
- Uploaded Windows ISOs/disks must remain **private by default** and remain **usable over time**.
- The browser must be able to **stream** these private images securely.
- Depending on product tier, the system must support some form of **persistence/writeback** for changes made by the guest OS.

This document defines the disk image lifecycle, access control, and persistence strategies for a hosted Aero backend. It is written so a backend engineer can implement the flows without needing external references.

> Integration note: this document assumes the streaming/lease mechanism described in [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md). Leases/capabilities may be presented as signed URLs, cookies, and/or `Authorization` headers (see that doc for tradeoffs). This doc extends that model to cover uploads, ownership, sharing, and writeback.

Related documents (deployment/ops details):
- Streaming auth, CORS, COOP/COEP: [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md)
- CDN/object-store delivery: [Remote Disk Image Delivery (Object Store + CDN + HTTP Range)](./16-remote-disk-image-delivery.md)
- Range behavior + CDN limits (CloudFront): [HTTP Range + CDN Behavior](./17-range-cdn-behavior.md)
- CDN-friendly alternative to `Range`: [Chunked Disk Image Format](./18-chunked-disk-image-format.md)
- Canonical disk/backend trait mapping: [Storage trait consolidation](./20-storage-trait-consolidation.md)
- Concrete AWS setup (S3 + CloudFront signed URL/cookie + COOP/COEP): [deployment/cloudfront-disk-streaming.md](./deployment/cloudfront-disk-streaming.md)
- Ops runbook for the bytes endpoint: [backend/disk-image-streaming-service.md](./backend/disk-image-streaming-service.md)
- Reference backend service (multipart upload + CloudFront signed cookies/URLs): [`services/image-gateway`](../services/image-gateway/README.md) (see also its [`openapi.yaml`](../services/image-gateway/openapi.yaml))

---

## Terminology

- **Image**: A stored blob that can be streamed to the browser (ISO or block device content).
- **ISO image**: Optical media (installation media), always treated as read-only.
- **Disk image**: A hard-disk/SSD block device presented to the guest as a writable disk.
- **Base image**: Immutable blob used as the starting point (e.g., a clean Windows install, or an uploaded disk).
- **Delta / overlay**: Copy-on-write state layered over a base (local or remote).
- **Lease token**: Short-lived token granting a narrow capability (e.g., `disk:read` for a specific image).
- **User session**: Long-lived authentication (cookie/OAuth) used to call management APIs and request leases.
- **`imageId` / `diskId`**: stable identifier for an image. This doc uses `imageId` for management-plane APIs; the streaming auth spec uses `diskId` for the disk-bytes endpoint. They can be the same underlying ID.

---

## Typical hosted user flows (end-to-end)

### A) Install Windows from a user-uploaded ISO (recommended default experience)

1) User uploads a Windows ISO (`kind=iso`, `visibility=private`).
2) User creates a new VM and provisions an **empty disk** of some size (e.g., 40 GiB).
   - The empty disk does not need an uploaded source file; the service can represent it as a “virtual zero” base with a declared `sizeBytes`.
3) When the VM starts, the browser requests short-lived `disk:read` leases for:
   - the ISO (CD-ROM) and
   - the disk base (empty disk base or uploaded base).
4) The browser streams data via `Range` reads from the disk bytes endpoint (e.g., `GET /disks/{diskId}/bytes`, sometimes routed as `/disk/{id}`; see [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md)).
5) Persistence:
   - **Strategy 1 (early default):** all writes go to an OPFS delta local to the browser.
   - **Strategy 2/3:** writes are persisted remotely under `disk:write`.

### B) Import an existing VM disk (power user / migration)

1) User uploads/imports a disk image (`kind=disk`) in raw/qcow2/vhd.
2) Service converts to canonical raw (if needed) and marks it `ready`.
3) VM starts and streams the base disk read-only (plus delta, depending on writeback strategy).

---

## Image types: what users can upload vs what the service stores

### Supported user-provided formats

The hosted service can accept multiple formats for user convenience, but should store **canonical formats** that are cheap to stream and easy for the browser disk layer to consume.

| User upload type | Typical extension(s) | Allowed? | Used as | Service canonical storage | Notes |
|---|---:|:---:|---|---|---|
| ISO 9660/UDF | `.iso` | ✅ | CD-ROM/DVD (install media) | **ISO blob as-is** | Read-only. Supports `Range` reads directly. |
| Raw disk | `.img`, `.raw` | ✅ | HDD/SSD | **Raw disk blob** (sector-addressable) | Best for streaming; simplest client. |
| QCOW2 | `.qcow2` | ✅ (import) | HDD/SSD | **Converted to raw disk blob** + optional “source” retention | QCOW2 is sparse/COW; converting avoids implementing QCOW2 in-browser. |
| VHD (fixed/dynamic) | `.vhd` | ✅ (import) | HDD/SSD | **Converted to raw disk blob** + optional “source” retention | VHD parsing/conversion best done server-side. |
| VHDX | `.vhdx` | ⚠️ Optional | HDD/SSD | Converted to raw disk blob | More complex; support later if needed. |

**Policy recommendation (early):**
- Accept **ISO + raw disk** initially.
- Add QCOW2/VHD import later with server-side conversion (async job).

### What the service actually stores

For a hosted system, store images in object storage as **immutable objects** (even for “writable” disks; see writeback strategies). The one common exception is an **empty “zero disk” base**, which can be represented without pre-uploading gigabytes of zeros (e.g., by synthesizing zeros in the streaming endpoint, or by using a sparse/chunked representation).

Each image record should point to at least one underlying storage location:

- `canonicalObjectKey`: the canonical bytes object for the image (raw disk or ISO).
- Optional: `chunkedManifestObjectKey` (and `chunkedChunksPrefix`): precomputed chunked delivery artifacts (see [Chunked Disk Image Format](./18-chunked-disk-image-format.md)).
- Optional: `sourceObjectKey`: the original uploaded file, retained for debugging/audit or re-conversion.

Delivery requirements (random access in the browser):
- **Range delivery:** the disk-bytes endpoint MUST support `GET` + `Range: bytes=...` with stable `Content-Length` (see [Disk Image Streaming](./16-disk-image-streaming-auth.md)).
- **Chunked delivery (optional):** alternatively, the service MAY serve read-only base images via a manifest + fixed-size chunk objects (see [Chunked Disk Image Format](./18-chunked-disk-image-format.md)).
- In either case, avoid compression/transforms that break deterministic offsets (see the streaming/auth spec for required headers).

---

## Ownership, visibility, and sharing

### Core fields

Each image record is owned by exactly one user:

- `id`: stable identifier (UUID/ULID).
- `ownerUserId`: user who created/imported the image.
- `kind`: `iso` or `disk`.
- `canonicalFormat`: `iso` or `raw` (even if the upload was qcow2/vhd).
- `sizeBytes`
- `createdAt`, `updatedAt`
- `state`: `uploading` → `processing` → `ready` (or `failed`, `deleted`)
- `visibility`: `private` \| `shared` \| `public`

### Visibility meanings

- **`private` (default):** only the owner (and admins) can access.
- **`shared`:** access granted to specific principals (users and/or share links).
- **`public`:** readable by anyone via the Aero streaming endpoint (and optionally an unauthenticated “public read lease” flow); still no direct object-storage URLs are exposed.

### Sharing mechanisms

Two supported sharing mechanisms can co-exist:

1) **Direct user sharing (ACL grants)**  
   An owner explicitly grants another user read/write access.

2) **Share links**  
   The service creates a random, revocable secret (a “link”) that acts as a principal.  
   This is useful for one-off sharing without creating accounts.

Recommended share link properties:
- Token value is **unguessable** (≥ 128 bits of entropy).
- Stored in DB as a **hash** (like password storage), so DB leaks do not expose active links.
- Links are **revocable** and can be time-limited.
- Link access should be **read-only by default** (read-write links are high risk).

### Permissions matrix

Define actions in terms of image management and data plane access:

- `read`: stream bytes (`Range` reads) and view metadata.
- `write`: modify state (only applicable to delta/writeback strategies), update metadata like name/description, attach to VMs.
- `delete`: remove the image and all derived data (deltas, share links).
- `share`: create/revoke share grants and share links.
- `upload`: create/complete uploads (initial ingest only).

Roles/principals:
- **Owner**: `ownerUserId`
- **Shared user (read)**: explicit ACL entry
- **Shared user (write)**: explicit ACL entry
- **Share link holder**: anyone presenting a valid share-link token
- **Public/anonymous**: any caller (no auth)

| Principal | read | write | delete | share | upload |
|---|:---:|:---:|:---:|:---:|:---:|
| Owner | ✅ | ✅ | ✅ | ✅ | ✅ |
| Shared user (read) | ✅ | ❌ | ❌ | ❌ | ❌ |
| Shared user (write) | ✅ | ✅ | ❌ | ❌ | ❌ |
| Share link holder (default) | ✅ | ❌ | ❌ | ❌ | ❌ |
| Public/anonymous (`visibility=public`) | ✅ | ❌ | ❌ | ❌ | ❌ |

> Implementation tip: keep “management-plane” checks (list/update/delete/share/upload) on user session auth, and keep “data-plane” checks (stream bytes, write blocks) on short-lived leases.

---

## Suggested data model (relational)

This section is optional, but it is a good starting point for implementing the ownership/sharing semantics above.

### `images`

One row per uploaded/imported image.

- `id` (PK)
- `owner_user_id` (indexed)
- `kind` (`iso` | `disk`)
- `canonical_format` (`iso` | `raw`)
- `canonical_object_key` (object storage key; immutable once `ready`)
- `source_object_key` (nullable; original upload)
- `size_bytes`
- `state` (`uploading` | `processing` | `ready` | `failed` | `deleted`)
- `visibility` (`private` | `shared` | `public`)
- `created_at`, `updated_at`

### `image_acl`

Explicit grants for “shared” images.

- `id` (PK)
- `image_id` (FK → `images.id`, indexed)
- `principal_type` (`user` | `share_link`)
- `principal_id` (e.g., `userId` or `shareLinkId`)
- `permission` (`read` | `write`)
- `created_at`

> Policy suggestion: avoid “delete/share/upload” in ACL grants unless you have a strong use case; keep those owner-only.

### `share_links`

Share links are principals; callers prove possession of the link token during the management-plane “redeem” flow or directly during lease issuance.

- `id` (PK)
- `image_id` (FK → `images.id`, indexed)
- `token_hash` (store a salted hash; never store the raw token)
- `permission` (`read` | `write`), but default to `read`
- `expires_at` (nullable)
- `revoked_at` (nullable)
- `created_at`

### `uploads` (optional)

Track resumable uploads and multipart state.

- `id` (PK)
- `image_id` (FK → `images.id`, indexed)
- `provider` (`direct` | `s3` | `gcs` | …)
- `provider_upload_id` (multipart upload ID / resumable session ID)
- `state` (`active` | `completed` | `aborted`)
- `part_size_bytes`
- `created_at`, `expires_at`

### `deltas` (Strategies 2/3)

Track remotely persisted writable state.

- `id` (PK)
- `owner_user_id` (indexed)
- `base_image_id` (FK → `images.id`, indexed)
- `block_size_bytes`
- `created_at`, `updated_at`

---

## Lifecycle states and transitions

### Suggested state machine

```
create(metadata)
   │
   ▼
uploading  --(finalize upload)-->  processing  --(success)-->  ready
   │                                  │
   │                                  └--(failure)--> failed
   │
   └--(cancel/delete)--> deleted
```

Transitions:
- **uploading → processing:** the service has a complete object and starts validation/conversion.
- **processing → ready:** canonical object is available for streaming.
- **processing → failed:** keep diagnostics; allow retry by re-upload or re-convert.
- **ready → deleted:** delete canonical object and associated metadata (and deltas).

### Validation and conversion (processing step)

Minimum recommended processing:
- Enforce size limits and per-user quotas.
- Detect format (do not trust filename/MIME type).
- For ISO: optionally sanity-check ISO headers/volume descriptors.
- For disk imports (qcow2/vhd): convert to canonical raw and record the resulting size/geometry.

Optional processing (product-dependent):
- Virus/malware scanning (be careful: scanning Windows images is non-trivial and may be expensive).
- Content hashing (SHA-256) for integrity checks and deduplication **within a single user**.

---

## Upload/import flows (browser → service)

Both flows start with the management plane (authenticated user session) creating an image record and obtaining authorization to upload bytes.

### Common API shape (management plane)

1) **Create metadata**

`POST /v1/images`
```json
{
  "kind": "iso" | "disk",
  "displayName": "Windows 7 SP1 x64 ISO",
  "upload": {
    "filename": "Win7.iso",
    "sizeBytes": 3355443200
  }
}
```

Response:
```json
{
  "imageId": "img_...",
  "state": "uploading"
}
```

2) **Initiate upload** (choose A or B below)

3) **Finalize upload**

`POST /v1/images/{imageId}/upload:finalize`
```json
{
  "expectedSizeBytes": 3355443200,
  "optionalSha256": "..."
}
```

The server should then move the image to `processing` and later to `ready`.

> `disk:upload` scope is required for the upload initiation/finalization steps; uploading bytes themselves may be authorized by either a user session or a short-lived upload lease.

---

### Approach A: direct upload to the Aero API (reference / self-hosted)

This approach is easiest to implement and deploy in a self-hosted environment, but pushes large data through your API servers.

#### Flow

1) `POST /v1/images/{imageId}/upload:begin`  
   Server returns an **upload token** (short-lived) and an endpoint to `PUT` to.

2) Browser uploads bytes to the Aero API:

`PUT /v1/images/{imageId}/content`
- `Authorization: Bearer <upload-lease>`
- `Content-Type: application/octet-stream`
- Use either:
  - **Single PUT** (small files)
  - **Chunked/resumable PUTs** (large files)

Resumable option (recommended if implemented):
- Client sends chunks with `Content-Range: bytes <start>-<end>/<total>`
- Server persists parts to temp storage and assembles on finalize.

3) `POST /v1/images/{imageId}/upload:finalize`

#### Pros / cons

Pros:
- No object-storage-specific client logic.
- Works without S3/GCS credentials or CORS complexity.

Cons:
- Expensive bandwidth/egress on API layer.
- Requires large request body handling, timeouts, buffering.
- Harder to make robust resumable uploads at tens of GB.

---

### Approach B: direct-to-object-storage upload (signed URLs / multipart)

This approach keeps large uploads off your API servers while preserving strict access control.

#### Flow (multipart; S3/GCS-style)

1) `POST /v1/images/{imageId}/upload:begin`

Request:
```json
{
  "mode": "multipart",
  "partSizeBytes": 8388608
}
```

Response:
```json
{
  "uploadId": "upl_...",
  "partSizeBytes": 8388608,
  "parts": [
    { "partNumber": 1, "signedUrl": "https://storage..."},
    { "partNumber": 2, "signedUrl": "https://storage..."}
  ],
  "expiresAt": "2026-01-10T00:00:00Z"
}
```

2) Browser uploads each part with `PUT <signedUrl>` (or the provider’s multipart API).
   - Record each part’s ETag/checksum returned by the storage provider.

3) Browser finalizes the multipart upload with the Aero API:

`POST /v1/images/{imageId}/upload:complete`
```json
{
  "uploadId": "upl_...",
  "parts": [
    { "partNumber": 1, "etag": "\"...\"" },
    { "partNumber": 2, "etag": "\"...\"" }
  ]
}
```

4) Server completes the multipart upload server-side and transitions the image to `processing`.

#### Resumability notes

- Presigned URLs expire; resumability comes from keeping a stable `uploadId` and letting the client:
  - `GET /v1/images/{imageId}/upload` to list uploaded parts
  - `POST /v1/images/{imageId}/upload:refresh` to obtain new signed URLs for missing parts
- The browser should persist only **non-secret identifiers** (e.g., `imageId`, `uploadId`) if it needs to resume after reload.

#### Pros / cons

Pros:
- Scales to very large files; minimal load on API servers.
- Leverages storage provider durability and multipart semantics.

Cons:
- Requires CORS configuration on the bucket.
- Signed URLs are bearer secrets; must be handled carefully (see Security Controls).

---

## Secure streaming access (leases + Range reads)

### Data plane endpoint

The browser streams image bytes via a `Range`-capable **disk bytes endpoint**, as specified in [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md):

- `GET /disks/{diskId}/bytes` (and `HEAD /disks/{diskId}/bytes`), supports `Range: bytes=...` (often routed as `GET /disk/{id}` in simplified deployments)

Private images MUST require a valid lease/capability (or equivalent authorization) on every request; public images may be cacheable and unauthenticated depending on policy.

Implementation detail: the disk bytes endpoint can be implemented as either:
- A same-origin service endpoint that proxies to object storage using backend credentials, or
- A CDN/object-storage URL gated by a short-lived signed URL/cookie lease.

The service SHOULD NOT hand out long-lived, permanent object-storage URLs for private images.

> Note: For some deployments, read-only base images may be delivered via a **chunked manifest + chunk objects** format to avoid `Range` and reduce CDN/cross-origin friction. In that mode, the same ownership/lease principles apply, but the data plane is `GET manifest.json` + `GET chunks/*.bin` instead of `Range` reads. See: [Chunked Disk Image Format](./18-chunked-disk-image-format.md).

### End-to-end diagram (auth → lease → streaming)

```
┌──────────────┐        ┌─────────────────────┐        ┌───────────────────────┐
│   Browser    │        │   Aero API (mgmt)   │        │  Aero Disk Endpoint   │
│ (user agent) │        │  user session auth  │        │ (data plane, Range)   │
└──────┬───────┘        └──────────┬──────────┘        └───────────┬───────────┘
       │                           │                               │
       │ 1) User login / session   │                               │
       │──────────────────────────▶│                               │
       │                           │                               │
       │ 2) Request lease:         │                               │
       │    POST /v1/leases        │                               │
       │    { diskId, scopes }     │                               │
       │──────────────────────────▶│                               │
       │                           │ 3) Issue short-lived lease    │
       │                           │    (e.g., signed URL or JWT)  │
       │◀──────────────────────────│                               │
       │                           │                               │
       │ 4) Stream bytes:          │                               │
       │    GET /disks/{id}/bytes  │                               │
       │    (aka /disk/{id})       │                               │
       │    Range: bytes=...       │                               │
       │    (auth via lease)       │                               │
       │──────────────────────────────────────────────────────────▶│
       │                           │                               │
       │ 5) 206 Partial Content    │                               │
       │◀──────────────────────────────────────────────────────────│
       │                           │                               │
```

Lease scope enforcement:
- The disk endpoint verifies:
  - lease validity (`exp`, signature, `aud`)
  - resource ID binding (e.g., `diskId`)
  - required scope (`disk:read` for reads; `disk:write` for writes where applicable)
  - optional rate limits / byte limits

---

## Persistence/writeback strategies (and auth implications)

Different products will choose different persistence models. The hosted service should support at least Strategy 1 initially, and be designed so Strategy 2/3 can be added without breaking the streaming interface.

### Summary

| Strategy | What is remote? | What is local? | Cross-device resume | Server complexity | Required lease scopes |
|---|---|---|:---:|:---:|---|
| 1. Remote read-only + local OPFS delta (recommended early) | Base image (ISO/disk) | Delta/overlay (COW) | ❌ | Low | `disk:read` |
| 2. Remote base + remote delta | Base image + per-user delta | Optional cache | ✅ | Medium | `disk:read`, `disk:write` |
| 3. Fully remote read-write disk (block API) | Entire disk state | Optional cache | ✅ | High | `disk:read`, `disk:write` |

### Strategy 1 (recommended early): remote base read-only + local OPFS COW delta

**Model**
- The service streams a **read-only base** (uploaded disk or a blank “template disk” created at VM creation time).
- The browser stores all writes in a **local copy-on-write delta** in OPFS (Origin Private File System).
- On boot:
  - Reads: check local delta first; fall back to remote base via `Range`.
  - Writes: go to delta only.

**Pros**
- Minimal backend complexity: no remote writes.
- Strong privacy: changes (including potentially sensitive user data) stay in the user’s browser storage.
- Great fit for “try it out” experiences.

**Cons**
- No cross-device resume.
- Clearing site data loses state.
- Browser storage quotas may be restrictive.

**Auth**
- Base streaming requires only `disk:read` leases.
- No `disk:write` is needed because the service never receives disk writes.

### Strategy 2: remote base + remote delta (per-user, cross-device resume)

**Model**
- Keep the base immutable.
- Create a per-user (or per-VM) **delta image** stored remotely (object storage or DB-backed chunk store).
- Browser reads base + delta; writes are sent to the delta.

Recommended server-side delta representation:
- Fixed-size blocks (e.g., 1 MiB) addressed by `(deltaId, blockIndex)`.
- Store only written blocks (sparse).
- Optionally compact/merge blocks in the background.

**Pros**
- Cross-device resume.
- Easy backups and server-side retention policies.
- Allows sharing “machine state” without sharing base media.

**Cons**
- Requires a write API and careful quota enforcement.
- Concurrency/locking: decide whether multiple devices can write simultaneously (usually “single writer”).
- Potentially higher legal/compliance burden because the service now stores an installed Windows state (still user-provided, but it is persisted server-side).

**Auth**
- Reads: `disk:read` for base and delta.
- Writes: `disk:write` scoped to the delta (not necessarily the base).
- Recommended: issue **separate leases** for read vs write so a VM can be launched read-only.

### Strategy 3: fully remote read-write disk (block API)

**Model**
- The disk is conceptually a remote block device.
- The browser uses a block read/write API (or `Range` for reads + separate write endpoint) to access the disk.
- Backend is the source of truth; client caching is an optimization only.

**Pros**
- True cross-device and server-side durability.
- Enables server-side snapshots, cloning, and collaborative scenarios (if desired).

**Cons**
- Highest complexity and cost: many small reads/writes, low-latency requirements.
- Requires caching, write coalescing, flush semantics, and conflict resolution.

**Auth**
- Requires `disk:read` + `disk:write` leases for the active VM session.
- Consider extra restrictions in the lease (rate limits, max bytes written) to reduce blast radius.

---

## Token and lease scopes

Beyond the baseline `disk:read`, the hosted service should define these scopes:

- `disk:read` — authorize streaming reads (`Range` GETs).
- `disk:write` — authorize writeback (delta writes or block writes).
- `disk:delete` — authorize deletion of an image and its derivatives.
- `disk:share` — authorize creating/revoking share grants/links.
- `disk:upload` — authorize upload initiation/finalization.

### Least privilege model (recommended)

- **User session tokens** (cookies/OAuth):
  - Used for management-plane APIs: create/list/update/share/delete images; request leases.
  - Longer lifetime; higher privilege; never sent to the raw disk streaming endpoint from WASM workers if avoidable.

- **Leases** (short-lived bearer tokens):
  - Used for data-plane endpoints only: the disk bytes endpoint (`/disks/{diskId}/bytes`, sometimes routed as `/disk/{id}`) and any writeback endpoints.
  - Minted per image with explicit scopes and short TTL (minutes).
  - Presented as a signed URL, signed cookie, and/or `Authorization` header depending on deployment (see [Disk Image Streaming](./16-disk-image-streaming-auth.md)).
  - Renewed as needed (silent refresh) rather than being long-lived.

---

## Security controls (hosted service)

### Storage security

- **Encryption at rest:** enable object storage server-side encryption (SSE). Prefer KMS-backed keys (SSE-KMS) for auditability.
- **Access isolation:** bucket policies/IAM should only allow the Aero backend role to read/write objects. No public ACLs for private images.
- **Access logging:** enable object access logs (and application logs) including `userId`, `imageId`, action (`read-range`, `upload-part`, `delete`), and bytes transferred.

### Signing key rotation

- Lease tokens should include a `kid` header so signing keys can be rotated without downtime.
- Maintain multiple active verification keys; rotate on a schedule.
- For presigned URL generation credentials (S3 access keys / service accounts), rotate regularly and scope permissions to only the necessary bucket/prefix.

### Quotas and abuse controls

- Enforce per-user quotas at **create** and **finalize** time:
  - max total bytes stored
  - max number of images
  - max size per image
- Rate-limit lease issuance and streaming requests (per user + per IP) to reduce scraping risk.

### Avoid leaking signed URLs and tokens

Signed URLs (upload) and lease tokens (streaming) are bearer secrets.

Client guidance:
- Do not put long-lived user/session tokens in the **page URL** (no `?access_token=...`).
- If using signed URL leases for streaming (e.g., `?cap=...`), treat the full URL as a secret: do not persist it, do not log it, and keep expirations short (minutes).
- Do not persist signed URLs or leases in `localStorage` / `indexedDB`. Keep them in memory; re-issue when needed.
- Set `Referrer-Policy: no-referrer` (or at least `strict-origin`) on pages that may ever handle signed URLs to reduce accidental leakage via the `Referer` header. See also: [Security headers](./security-headers.md).

Server guidance:
- Return signed URLs only over HTTPS.
- Use tight expirations (minutes) and scope the URL to a single part/object.
- Set `Cache-Control: no-store` on responses that contain any secrets (leases, signed URLs).

---

## Appendix: Suggested API surface (v1)

Exact paths are implementation-defined; the goal is a clean separation between:

- **Management plane** (user session auth, slower, higher privilege): create images, upload initiation, share, delete, request leases.
- **Data plane** (lease/capability auth, high volume): `Range` reads and (optionally) writeback.

### Image management (management plane)

Common endpoints (cookie/OAuth session auth; permission checks per the matrix above):

- `GET /v1/images` — list images visible to the caller.
- `GET /v1/images/{imageId}` — metadata (kind, size, owner, state, visibility).
- `PATCH /v1/images/{imageId}` — update metadata (e.g., display name, visibility).
- `DELETE /v1/images/{imageId}` — delete image + derived data (deltas, shares, share links).

### Upload (management plane + upload lease)

- `POST /v1/images` — create an image record in `uploading`.
- `POST /v1/images/{imageId}/upload:begin` — return either:
  - an upload lease for direct API upload, or
  - multipart instructions + signed URLs for direct-to-object-storage upload.
- `PUT /v1/images/{imageId}/content` — (approach A) upload bytes to the API (optionally `Content-Range` resumable).
- `POST /v1/images/{imageId}/upload:complete` — (approach B) complete multipart upload by providing ETags.
- `POST /v1/images/{imageId}/upload:finalize` — transition to `processing` and start validation/conversion.

### Sharing (management plane)

- `POST /v1/images/{imageId}/shares` — grant another user read or write (creates an ACL entry).
- `DELETE /v1/images/{imageId}/shares/{shareId}` — revoke an ACL entry.
- `POST /v1/images/{imageId}/share-links` — create a share link principal (store hashed token).
- `DELETE /v1/images/{imageId}/share-links/{linkId}` — revoke a share link.

### Lease issuance (management plane)

The management plane mints short-lived leases/capabilities compatible with the disk-bytes endpoint described in [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md).

One possible shape:

`POST /v1/leases`
```json
{ "diskId": "img_...", "scopes": ["disk:read"], "ttlSeconds": 600 }
```

Response (signed URL example):
```json
{
  "diskId": "img_...",
  "url": "https://app.example.com/disks/img_.../bytes?cap=...",
  "expiresAt": "2026-01-10T12:34:56Z"
}
```

Response (bearer token example):
```json
{
  "diskId": "img_...",
  "authorization": "Bearer ...",
  "expiresAt": "2026-01-10T12:34:56Z"
}
```

### Writeback endpoints (Strategies 2 and 3)

If the service supports remote persistence, keep write endpoints separate from the read-only base:

- **Strategy 2 (remote delta):**
  - Create a delta resource owned by the user (or per-VM), e.g. `deltaId`.
  - Writes go to the delta only; reads combine base + delta.
- **Strategy 3 (fully remote read-write):**
  - Treat the disk itself as a mutable resource, but still prefer immutable generations/snapshots under the hood.

A simple, CDN-agnostic write API uses fixed-size blocks:

- `PUT /v1/deltas/{deltaId}/blocks/{blockIndex}` — write an entire block (e.g., 1 MiB).
  - Requires a `disk:write` lease scoped to `{deltaId}` (or the effective disk resource).
  - Recommend `If-Match: "<generation>"` (or similar) to enforce single-writer semantics.
- `GET /v1/deltas/{deltaId}/blocks/{blockIndex}` — read a block (optional; many designs can serve reads via the normal disk-bytes endpoint).
- `POST /v1/deltas/{deltaId}:flush` — optional explicit flush/commit point (often a no-op if each block write is durable).

---

## Testable invariants (minimum)

- The service MUST NOT mint `disk:read`/streaming leases for images that are not in `ready`.
- The data plane MUST reject expired or scope-mismatched leases (typically `401`/`403`).
- A valid `Range` request MUST return `206 Partial Content` with a correct `Content-Range` and `Content-Length`.

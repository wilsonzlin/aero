# Third-party notices (virtio Windows drivers)

This directory is intended to contain (or be populated from) the **virtio-win** driver project.

When Aero starts redistributing real driver artifacts under `drivers/virtio/prebuilt/` (at minimum `.inf`/`.sys`/`.cat`, plus any INF-referenced payloads such as `*.dll`), add:

- the pinned upstream version (tag/commit + download URL)
- the upstream license texts copied verbatim
- any required attribution/notice files from the upstream distribution

Packaging note:

- `drivers/scripts/make-driver-pack.ps1` copies this file into produced driver packs as
  a top-level `THIRD_PARTY_NOTICES.md`, and also attempts to copy common upstream
  virtio-win `LICENSE*`/`NOTICE*`/`README*` files into `licenses/virtio-win/` for
  redistribution.

Until then, `drivers/virtio/sample/` only contains non-functional placeholders for CI.

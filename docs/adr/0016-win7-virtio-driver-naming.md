# ADR 0016: Windows 7 virtio driver naming + layout

## Context

Aero has multiple in-tree, clean-room Windows 7 virtio drivers (virtio-blk/net/input/snd) plus supporting packaging/provisioning tooling.

Historically, different parts of the tree used different naming schemes (for example, a driver project producing `virtioinput.sys` while the packaging scaffold expected `aero_virtio_input.sys`). This caused:

- mismatches between built binaries and INF `ServiceBinary` / `SourceDisksFiles`
- inconsistent service names (making diagnostics and provisioning harder)
- duplicated/confusing directory layout for the same driver

## Decision

For Aero’s in-tree Windows 7 virtio drivers, use a single canonical base name:

`aero_virtio_<dev>`

Where `<dev>` is one of:

- `blk`
- `net`
- `input`
- `snd`

This base name is applied consistently to:

- **Driver binary**: `aero_virtio_<dev>.sys`
- **Service name**: `aero_virtio_<dev>`
- **INF file name**: `aero_virtio_<dev>.inf`
- **Catalog file name**: `aero_virtio_<dev>.cat` (referenced by `CatalogFile = ...` in the INF)

Display strings should use “Aero VirtIO …” consistently (exact wording is device-specific).

### Directory layout

Each driver directory (regardless of whether the code lives under `drivers/windows7/` or `drivers/win7/`) MUST include a standard staging directory:

`<driver-root>/inf/`

This directory is intended to contain the complete installable driver package (`.inf` + `.sys` + `.cat`) for “Have Disk…” installs and for `Inf2Cat`/signing flows.

Examples:

- `drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf`
- `drivers/windows7/virtio-net/inf/aero_virtio_net.inf`
- `drivers/windows7/virtio-input/inf/aero_virtio_input.inf`
- `drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf`

Older in-tree driver prototypes and pre-provisioned test images may still reference legacy service/binary names (for example `aerovblk` / `aerovnet` / `aeroviosnd`). New in-tree contract-v1 drivers must use the canonical `aero_virtio_*` scheme.

## Alternatives considered

1. Keep existing names (`aerovblk`, `aerovnet`, `virtioinput`, `virtiosnd`)
   - Rejected: inconsistent and already caused packaging/provisioning mismatches.
2. Use `aerovio<dev>` (short prefix) for SYS/service/INF
   - Rejected: hard to parse, inconsistent with newer driver packaging scaffolds that already used `aero_virtio_*`.
3. Use hyphens everywhere (`aero-virtio-<dev>`)
   - Rejected: service names are commonly represented with underscores in Windows driver stacks, and file names already used underscores in several places.

## Consequences

- Driver projects and INFs must stay in sync: renaming `TARGETNAME` or the INF’s `ServiceBinary` requires regenerating catalogs and re-signing.
- Provisioning tooling and documentation can assume a stable set of INF names (`aero_virtio_*.inf`) when selecting which drivers to install.
- Existing dev/test images that were provisioned with the old driver service names may need reprovisioning after these renames.

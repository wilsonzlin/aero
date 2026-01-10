# Guest Tools packaging specs

These JSON files are consumed by `tools/packaging/aero_packager/` (`--spec`) to validate
driver artifacts before producing `aero-guest-tools.iso` / `aero-guest-tools.zip`.

## `win7-virtio-win.json`

Intended for packaging Guest Tools using a driver payload extracted from **virtio-win**.

- Requires: `viostor` (virtio-blk) + `netkvm` (virtio-net)
- Also allows other driver directories to be present (e.g. `viosnd`, `vioinput`); they will
  still be included in the output ISO/zip if present in the input.

This spec is used by `drivers/scripts/make-guest-tools-from-virtio-win.ps1` by default.


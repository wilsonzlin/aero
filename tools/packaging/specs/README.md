# Guest Tools packaging specs

These JSON files are consumed by `tools/packaging/aero_packager/` (`--spec`) to validate
driver artifacts before producing `aero-guest-tools.iso` / `aero-guest-tools.zip`.

## `win7-virtio-win.json`

Intended for packaging Guest Tools using a driver payload extracted from **virtio-win**.

- Requires: `viostor` (virtio-blk) + `netkvm` (virtio-net)
- Includes only the drivers listed in the spec (other driver directories present in the input
  are ignored).

## `win7-virtio-full.json`

Same as `win7-virtio-win.json`, but also declares optional drivers:

- Optional: `vioinput` (virtio-input) + `viosnd` (virtio-snd)

This spec can be passed to `drivers/scripts/make-guest-tools-from-virtio-win.ps1` via `-SpecPath`
to include the optional drivers when they are present in the virtio-win source.

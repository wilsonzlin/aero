# virtio-win ISO extractor (cross-platform)

`tools/virtio-win/extract.py` extracts a minimal subset of an upstream `virtio-win.iso`
needed by Aero’s Windows 7 packaging pipeline.

This exists because the Windows-only workflow (`Mount-DiskImage`) can’t run on Linux/macOS.

## Usage

From the repo root:

```bash
python3 tools/virtio-win/extract.py \
  --virtio-win-iso /path/to/virtio-win.iso \
  --out-root /tmp/virtio-win-root
```

Then pass the extracted directory to the existing packaging scripts:

```bash
pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot /tmp/virtio-win-root
```

Note: `make-driver-pack.ps1 -VirtioWinIso ...` mounts ISOs via `Mount-DiskImage` and is
therefore Windows-only. On Linux/macOS, use the extracted root (`-VirtioWinRoot`) or the
one-shot shell wrapper that runs the extractor for you:

```bash
drivers/scripts/make-driver-pack.sh --virtio-win-iso virtio-win.iso
```

For convenience, Aero also provides one-shot wrappers that do the extraction + packaging in one command:

- `drivers/scripts/make-driver-pack.sh` (driver pack zip/staging dir)
- `drivers/scripts/make-virtio-driver-iso.sh` (mountable drivers ISO)
- `drivers/scripts/make-guest-tools-from-virtio-win.sh` (Guest Tools ISO + zip)

## What gets extracted

- Win7 driver subtrees used by Aero:
  - required: `viostor`, `NetKVM`
  - optional (best-effort): `viosnd`, `vioinput`
  - both arches: `x86` + `amd64`
- Common root-level upstream notice files (best-effort): `LICENSE*`, `NOTICE*`, `README*`, etc.
- Small metadata/version marker files (best-effort): `VERSION`, `virtio-win-version.txt`, etc.

The tool also writes a provenance file at:

- `<out-root>/virtio-win-provenance.json`

`drivers/scripts/make-driver-pack.ps1` will ingest this file (when present) so driver-pack
`manifest.json` can record the original ISO sha256/volume label even when building from a
directory (`-VirtioWinRoot`).

## Dependencies / backends

Extraction backend selection is controlled by `--backend`:

- `auto` (default): use `7z`/`7zz`/`7za` if present, otherwise use `pycdlib`
- `7z`: force the `7z` backend
- `pycdlib`: force the pure-Python backend

Installing dependencies:

- Linux (Ubuntu/Debian): `sudo apt-get install p7zip-full`
- macOS (Homebrew): `brew install p7zip`
- Pure Python fallback: `python3 -m pip install pycdlib`

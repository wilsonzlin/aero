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

Tip: pass a dedicated output directory. If you use `--clean`, the extractor refuses to
delete the filesystem root (e.g. `/`) or a git checkout root (directories containing `.git/`).

Then pass the extracted directory to the existing packaging scripts:

```bash
pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot /tmp/virtio-win-root
```

Alternatively, you can pass `-VirtioWinIso` directly under `pwsh`. On Windows, `make-driver-pack.ps1`
will mount the ISO via `Mount-DiskImage`. When `Mount-DiskImage` is not available (Linux/macOS, or
minimal Windows installs), it falls back to invoking this extractor automatically (requires Python):

```bash
pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinIso virtio-win.iso
```

For convenience, you can also use the one-shot shell wrapper that runs the extractor for you:

```bash
bash ./drivers/scripts/make-driver-pack.sh --virtio-win-iso virtio-win.iso
```

For convenience, Aero also provides one-shot wrappers that do the extraction + packaging in one command:

- `bash ./drivers/scripts/make-driver-pack.sh` (driver pack zip/staging dir)
- `bash ./drivers/scripts/make-virtio-driver-iso.sh` (mountable drivers ISO)
- `bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh` (Guest Tools ISO + zip)

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

The extractor is a plain Python script and aims to work on Python 3.8+.

Note: the `pycdlib` backend prefers **Joliet** paths (mixed-case). If the ISO was authored
without Joliet, it will fall back to **Rock Ridge** (and then ISO-9660) paths. When using
plain ISO-9660 paths, the extractor normalizes extracted filenames by stripping version
suffixes like `;1` (and any resulting trailing dot, e.g. `VERSION.;1 → VERSION`) so
downstream tooling sees normal `*.inf`/`*.sys` names.

Installing dependencies:

- Linux (Ubuntu/Debian): `sudo apt-get install p7zip-full`
- macOS (Homebrew): `brew install p7zip`
- Pure Python fallback: `python3 -m pip install pycdlib`

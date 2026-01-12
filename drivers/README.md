# Windows 7 guest driver stack (virtio + GPU path)

This directory contains the **Windows 7 guest driver** workflow for Aero: build tooling, packaging, and installation/injection steps for the **virtio performance drivers** required by the emulator.

## CI/release artifacts (what Aero ships)

Aero’s CI/release pipeline builds, catalogs, and signs the **in-tree** Windows 7 drivers in this repo (virtio + AeroGPU), then packages distributable artifacts from the signed output directories:

- `out/packages/**` (signed driver packages)
- `out/certs/aero-test.cer` (public cert used to sign the driver catalogs)

Canonical workflows:

- `.github/workflows/drivers-win7.yml` (PR/push; builds + signs + packages)
- `.github/workflows/release-drivers-win7.yml` (tagged releases; publishes assets)

Primary CI artifacts (names matter; these are consumed by downstream workflows/scripts):

- `win7-drivers`
  - Contents: `out/artifacts/` (installable driver bundle ZIPs + ISO, packaged Guest Tools outputs, and optionally a FAT driver disk image)
- `win7-drivers-signed-packages`
  - Contents: `out/packages/**` + `out/certs/aero-test.cer` (raw signed packages)
- `aero-guest-tools`
  - Built from: `out/packages/` + `out/certs/`
  - Files:
    - `aero-guest-tools.iso`
    - `aero-guest-tools.zip`
    - `manifest.json`
    - `aero-guest-tools.manifest.json` (copy of `manifest.json` used by CI/release asset publishing)

Driver set (Guest Tools media, by default):

| Aero device | Guest Tools driver dir | Packaging | Notes |
|---|---|---|---|
| `virtio-blk` | `virtio-blk` | required | Storage (boot-critical when switching from AHCI). |
| `virtio-net` | `virtio-net` | required | Network. |
| `virtio-input` | `virtio-input` | required | Optional device (PS/2 fallback), but expected to be shipped. |
| `virtio-snd` | `virtio-snd` | optional | Optional device (HDA/AC’97 fallback). |
| `aerogpu` | `aerogpu` | required | Optional device (VGA fallback), but expected to be shipped. |

Notes:

- “required/optional” above refers to the **packaging spec** (what the ISO/zip is expected to contain), not whether the emulator can boot without the device.
- This repo generally does **not** commit `.sys` binaries. Official artifacts are built by CI from source.

To build Guest Tools locally from CI outputs, use:

- `ci/package-guest-tools.ps1` (CI wrapper around `tools/packaging/aero_packager/`)
- `drivers/scripts/make-guest-tools-from-ci.ps1` (convenience wrapper)

CI/release packaging uses the spec:

- `tools/packaging/specs/win7-signed.json`

## Alternative/compatibility: virtio-win-derived driver packs

In addition to the in-tree CI/release driver artifacts, Aero also supports building driver packs and Guest Tools media from an **upstream `virtio-win.iso`**. This is useful for compatibility testing or when you want to bring your own WHQL/production-signed virtio driver set.

## Quickstart (virtio-win): build a driver pack ZIP

1. Download a **virtio-win ISO** (stable) on a build machine.
   - Example: `virtio-win.iso` from the virtio-win project’s “stable-virtio” direct downloads.

2. Build the driver pack:

### Windows host (mount ISO directly)

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-driver-pack.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso
```

Notes:

- By default, `make-driver-pack.ps1` **requires** `viostor` + `netkvm`, and attempts to include `viosnd` + `vioinput` best-effort (emits a warning if missing).
- To build a minimal pack explicitly:
  - `-Drivers viostor,netkvm`
- To fail if optional drivers are requested but missing:
  - `-StrictOptional` (typically used together with `-Drivers viostor,netkvm,viosnd,vioinput`)

### Linux/macOS host

`drivers/scripts/make-driver-pack.ps1` can run under PowerShell 7 (`pwsh`).

Option A (recommended): extract first, then use `-VirtioWinRoot`:

```bash
python3 tools/virtio-win/extract.py \
  --virtio-win-iso virtio-win.iso \
  --out-root /tmp/virtio-win-root

pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot /tmp/virtio-win-root
```

Option B: pass `-VirtioWinIso` directly under `pwsh` (auto-extract fallback on non-Windows if mounting is unavailable or fails):

```bash
pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinIso virtio-win.iso
```

Notes:

- The extractor prefers `7z`/`7zz` (no root required). If you don’t have it, install:
  - Ubuntu/Debian: `sudo apt-get install p7zip-full`
  - macOS (Homebrew): `brew install p7zip`
  - Or use the pure-Python backend: `python3 -m pip install pycdlib` and pass `--backend pycdlib`.
- `make-driver-pack.ps1` requires PowerShell 7 (`pwsh`) on non-Windows hosts.
- See `tools/virtio-win/README.md` for extractor details (outputs, provenance, backends).
- Convenience: `bash ./drivers/scripts/make-driver-pack.sh` wraps the extraction + `pwsh` invocation into one command.
- Convenience: `bash ./drivers/scripts/make-virtio-driver-iso.sh` builds `aero-virtio-win7-drivers.iso` from a virtio-win ISO on Linux/macOS.
- Convenience: `bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh` builds `aero-guest-tools.iso`/`.zip` from a virtio-win ISO on Linux/macOS.

Output:

- `drivers\out\aero-win7-driver-pack\` (staging dir)
- `drivers\out\aero-win7-driver-pack.zip` (zip copy of the staging dir; convenient for copying into a VM or for downstream tooling)

Both the staging directory and the zip include:

- `manifest.json` (provenance info; records the source virtio-win ISO path/hash when applicable)
- `THIRD_PARTY_NOTICES.md` (third-party attribution/redistribution notices for virtio-win-derived artifacts)
- `licenses/virtio-win/` (best-effort copy of upstream license/notice files from the virtio-win distribution root)

### Optional: build a mountable drivers ISO (for Windows Setup “Load driver”)

If you want a CD-ROM ISO containing the same `win7/x86/...` and `win7/amd64/...` directories:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-virtio-driver-iso.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutIso .\dist\aero-virtio-win7-drivers.iso
```

On non-Windows hosts you have three options:

- Use the `.sh` wrapper: `bash ./drivers/scripts/make-virtio-driver-iso.sh`
- Extract first with `tools/virtio-win/extract.py` and pass `-VirtioWinRoot`
- Run under `pwsh` and pass `-VirtioWinIso` directly (auto-extract fallback when `Mount-DiskImage` is unavailable or fails)

See also: `docs/virtio-windows-drivers.md`.

Note: the resulting drivers ISO includes `THIRD_PARTY_NOTICES.md` at the ISO root
so redistributed media carries virtio-win attribution requirements.

### Optional: build `aero-guest-tools.iso` from virtio-win (post-install enablement)

If you want the Guest Tools ISO (scripts + drivers; certificates are optional depending on signing policy), use:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools
```

By default, this wrapper builds media with `signing_policy=none` (for WHQL/production-signed virtio-win drivers), so it does **not** require or inject any custom certificate files and `setup.cmd` will not prompt to enable Test Mode by default.

On non-Windows hosts you have three options:

- Use the `.sh` wrapper: `bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh`
- Extract first with `tools/virtio-win/extract.py` and pass `-VirtioWinRoot`
- Run under `pwsh` and pass `-VirtioWinIso` directly (auto-extract fallback when `Mount-DiskImage` is unavailable or fails)

Profiles:

- `-Profile full` (default): includes optional virtio drivers when available (`vioinput`, `viosnd`)
- `-Profile minimal`: storage + network only (`viostor`, `netkvm`)

Signing policy:

- Default is `-SigningPolicy none` (for WHQL/production-signed virtio-win drivers; no cert injection).
- Override with `-SigningPolicy test` for test-signed/custom-signed driver bundles.
  - Legacy alias accepted: `testsigning` (maps to `test`).

This emits the following under `dist/guest-tools/`:

- `aero-guest-tools.iso`
- `aero-guest-tools.zip`
- `manifest.json`

The Guest Tools ISO/zip root also includes `THIRD_PARTY_NOTICES.md` (sourced from
`guest-tools/THIRD_PARTY_NOTICES.md` in this repo).

When building Guest Tools from a virtio-win ISO/root using the wrapper script,
upstream virtio-win license/notice files (if present) are also included under:

- `licenses/virtio-win/`
  - Includes `driver-pack-manifest.json` (copied from the extracted driver pack) to preserve virtio-win ISO provenance.

### Optional: build `aero-guest-tools.iso` from in-tree aero virtio drivers (aero_virtio_blk + aero_virtio_net)

If you built Aero's in-tree Win7 virtio drivers (`aero_virtio_blk`, `aero_virtio_net`) and have a packager-style driver output directory:

```
<DriverOutDir>/
  x86/aero_virtio_blk/*.{inf,sys,cat}
  x86/aero_virtio_net/*.{inf,sys,cat}
  amd64/aero_virtio_blk/*.{inf,sys,cat}   # (or x64/ instead of amd64/)
  amd64/aero_virtio_net/*.{inf,sys,cat}
```

You can build Guest Tools media (ISO + zip) using:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-aero-virtio.ps1 `
  -DriverOutDir C:\path\to\driver-out `
  -OutDir .\dist\guest-tools
```

This validates the packaged drivers using:

- `tools/packaging/specs/win7-aero-virtio.json` (modern-only virtio IDs; rejects virtio-pci transitional IDs)

## In-guest install workflow (recommended): Guest Tools

For end users, the intended flow is to mount **Aero Guest Tools** (`aero-guest-tools.iso`) in the Windows 7 guest and run `setup.cmd` as Administrator.

See: `docs/windows7-guest-tools.md`.

### Manual install (virtio-win driver pack ZIP)

If you built a virtio-win-derived pack (`drivers/out/aero-win7-driver-pack.zip`), you can install those drivers manually:

1. Copy `aero-win7-driver-pack.zip` into the Win7 guest.
2. Extract it.
3. Run as Administrator:

```bat
install.cmd
```

This uses `pnputil` to add the correct-architecture Win7 driver INFs. For boot-disk switching (AHCI → virtio-blk), prefer Guest Tools (it performs boot-critical pre-seeding).

## Offline injection workflow (slipstream into install media)

If you want Windows Setup to see virtio storage/network during install, inject drivers into the WIMs.

There are two common cases:

- **Test-signed Aero driver bundles** (CI/release, `signing_policy=test`): inject both drivers **and** the public signing certificate into the offline images.
- **WHQL/production-signed drivers** (for example, unmodified virtio-win packages): inject drivers with DISM and skip certificate injection.

### Recommended (Windows host): patch extracted install media (`tools/windows/patch-win7-media.ps1`)

The script `tools/windows/patch-win7-media.ps1` is the most complete end-to-end flow: it patches BCD
`testsigning`, injects the signing certificate into offline stores, and can inject drivers from any
directory containing `.inf` files (including `out/packages/**` from CI builds).

Example (CI-built signed packages + cert → patched Win7 ISO tree):

```powershell
.\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\iso\win7sp1 `
  -CertPath .\out\certs\aero-test.cer `
  -DriversPath .\out\packages
```

### Alternative (driver-pack layout): inject a single WIM (`drivers/scripts/inject-win7-wim.ps1`)

If you already have a `win7/<arch>/...` driver tree (for example from `drivers/out/aero-win7-driver-pack/`
produced by `make-driver-pack.ps1`), `drivers/scripts/inject-win7-wim.ps1` can inject drivers into a
single WIM index and inject a certificate into the offline `ROOT` + `TrustedPublisher` stores.

For WHQL/production-signed drivers, prefer DISM-only injection without adding a custom certificate.

Rebuild the ISO after patching (outside the scope of this repo; use your preferred `oscdimg`/ISO tool).

## Driver signing / test mode

For custom Aero drivers (e.g. the optional GPU path), Windows 7 will require either:

- properly signed drivers, or
- **test signing** enabled in the guest.

See: `drivers/docs/signing-win7.md`.

## WDK build environment (for source builds)

See: `drivers/docs/wdk-build.md`.

## Basic validation plan (in-guest)

- **Device Manager**
  - Verify devices bind to the expected storage/network drivers.
    - CI/in-tree Guest Tools typically uses `aero_virtio_blk` (virtio-blk) and `aero_virtio_net` (virtio-net).
    - virtio-win-derived packs typically use `viostor` and `NetKVM` (`netkvm`).
  - If present in your media, also verify audio/input drivers (`virtio-snd`/`virtio-input` or `viosnd`/`vioinput` depending on source).
- **Storage throughput**
  - `winsat disk -seq -read` and `winsat disk -seq -write`
  - Large file copy in/out of the guest.
- **Network**
  - `ipconfig /all` shows link + DHCP lease
  - `ping` gateway and `iperf3` to a host endpoint (if available)
- **Audio**
  - Play a WAV in Windows Media Player, verify output device exists.
- **Input**
  - Verify keyboard/mouse are responsive without PS/2 fallback.
- **GPU (AeroGPU)**
  - If using the optional AeroGPU WDDM stack, run the guest-side validation suite:
    - `drivers\\aerogpu\\tests\\win7\\run_all.cmd --require-vid=0xA3A0 --require-did=0x0001`
    - If using the deprecated legacy AeroGPU device model, pass its matching VID/DID (see `docs/abi/aerogpu-pci-identity.md`).
    - (Use `run_all.cmd --help` for flags like `--dump` / `--allow-remote`.)

## Host-side protocol tests (shared structs)

`drivers/protocol/virtio/` contains `#[repr(C)]` virtio protocol structs intended to be shared with the emulator implementation.

Run:

```bash
cargo test --locked --manifest-path drivers/protocol/virtio/Cargo.toml
```

## Portable virtio-pci capability parser tests (hardware-free)

`drivers/win7/virtio/virtio-core/portable/` contains a small C99 module that walks a PCI capability list and extracts the **Virtio 1.0+ "modern"** vendor capabilities (common/notify/isr/device). This is intended to prevent regressions in Windows driver capability discovery logic without requiring any real hardware.

Run:

```bash
bash ./drivers/win7/virtio/tests/build_and_run.sh
```

## Optional: custom GPU path (WDDM)

Design notes live in `drivers/docs/gpu-path.md`. The in-tree implementation lives under `drivers/aerogpu/`:

- Driver sources + build tooling: `drivers/aerogpu/` (start at `drivers/aerogpu/README.md`)
- Build instructions: `drivers/aerogpu/build/README.md`

This is the long-term “fast path” for DirectX command interception/translation, but it is not required for the initial virtio bring-up.

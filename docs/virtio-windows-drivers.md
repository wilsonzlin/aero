# Virtio Windows 7 Drivers (virtio-blk / virtio-net / virtio-input / virtio-snd)

## Goal

Enable Aero’s **virtio acceleration path** by making it straightforward to install Windows 7 drivers for:

- **virtio-blk** (storage) *(minimum deliverable)*
- **virtio-net** (network) *(minimum deliverable)*
- **virtio-input** (keyboard/mouse/tablet) *(best-effort; PS/2/USB HID remains fallback)*
- **virtio-snd** (audio) *(optional; HDA remains fallback; AC’97 is legacy-only)*

See also:

- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive device/feature/transport contract.
- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation reference for Win7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
- [`16-windows7-driver-build-and-signing.md`](./16-windows7-driver-build-and-signing.md) — CI + local pipeline for building/cataloging/signing the in-tree Win7 driver packages (`drivers-win7.yml`).
- virtio-input end-to-end test plan (device model + Win7 driver + web runtime): [`test-plans/virtio-input.md`](./test-plans/virtio-input.md)

This document defines:

- The chosen driver approach (and licensing rationale)
- The required on-disk packaging layout (`.inf` + `.sys` + `.cat`, plus any INF-referenced payload files such as `WdfCoInstaller*.dll`)
- A reproducible “drivers ISO” build flow for the emulator UX
- Windows 7 installation steps (Device Manager + `pnputil`)
- Win7 x64 test-signing mode flow + test certificate tooling
- Optional offline driver injection into Windows install images (DISM)

See also:

- `drivers/README.md` (how Aero builds/ships driver packs in practice)
- `docs/windows7-guest-tools.md` (end-user flow: Guest Tools, then switch to virtio)
- `docs/16-win7-image-servicing.md` (detailed Win7 x64 offline servicing: cert injection + BCD test signing)
- `docs/windows7-virtio-driver-contract.md` (Aero-specific virtio device IDs + contract)

---

## Packaging options (Guest Tools)

Aero can build Guest Tools media (`aero-guest-tools.iso` / `.zip`) from a few different driver sources:

1) **CI/release in-tree drivers (official artifacts)**
   - CI builds signed packages under `out/packages/**` and a public signing cert under `out/certs/`.
   - In GitHub Actions these are published as:
     - `win7-drivers-signed-packages` (`out/packages/**` + `out/certs/aero-test.cer`)
     - `aero-guest-tools` (`aero-guest-tools.iso/.zip` + manifest)
   - Scripts:
     - `ci/package-guest-tools.ps1`
     - `drivers/scripts/make-guest-tools-from-ci.ps1` (convenience wrapper)
   - CI/release spec: `tools/packaging/specs/win7-signed.json`
   - Outputs:
     - `aero-guest-tools.iso`
     - `aero-guest-tools.zip`
     - `manifest.json`
     - `aero-guest-tools.manifest.json` (alias used by CI/release asset publishing)

2) **Upstream virtio-win** (`viostor`, `netkvm`, etc.) *(optional / compatibility)*
   - Script: `drivers/scripts/make-guest-tools-from-virtio-win.ps1`
   - Spec (via `-Profile`):
      - Default (`-Profile full`): `tools/packaging/specs/win7-virtio-full.json` (expects modern IDs for core devices; `AERO-W7-VIRTIO` v1 is modern-only; includes best-effort `vioinput`/`viosnd` when present)
      - Optional (`-Profile minimal`): `tools/packaging/specs/win7-virtio-win.json` (storage+network only)
   - Device contract (for generated `config/devices.cmd`): `docs/windows-device-contract-virtio-win.json`

3) **In-tree Aero virtio** (`aero_virtio_blk`, `aero_virtio_net`, etc.) *(local/dev)*
   - Script: `drivers/scripts/make-guest-tools-from-aero-virtio.ps1`
   - Spec: `tools/packaging/specs/win7-aero-virtio.json` (**modern-only** IDs; rejects virtio-pci transitional IDs at packaging time)
   - Device contract (for generated `config/devices.cmd` when using the CI packaging wrapper): `docs/windows-device-contract.json`

See also:

- `docs/16-guest-tools-packaging.md` (specs, inputs/outputs, signing policy)
- `drivers/README.md` (CI artifacts + virtio-win alternative tooling)

## Chosen approach (what Aero ships): CI-built in-tree drivers

Aero’s official Windows 7 driver artifacts (driver bundles + Guest Tools media) are produced by CI from the in-repo driver sources and published via the `drivers-win7.yml` / `release-drivers-win7.yml` workflows.

The virtio-win packaging flow remains supported as an **alternative/compatibility** path (for example: to compare behavior/performance against upstream drivers or to bring your own WHQL/production-signed virtio stack).

### Why

Shipping CI-built in-tree drivers keeps Aero releases reproducible and ensures the packaged HWIDs/service-name contract stays aligned with the emulator + Guest Tools scripts.

The **virtio-win** packaging flow remains available as an optional compatibility/testing path.

### virtio-win compatibility mapping

When using the optional virtio-win flow, we target the **virtio-win** driver distribution (commonly shipped as `virtio-win.iso`) and specifically the packages:

| Aero device | Windows PCI HWID (Aero / `AERO-W7-VIRTIO` v1) | virtio-win package name (typical) |
|------------|----------------------------------------|-----------------------------------|
| virtio-net | `PCI\VEN_1AF4&DEV_1041&REV_01` | `NetKVM` (`netkvm.inf` / `netkvm.sys`) |
| virtio-blk | `PCI\VEN_1AF4&DEV_1042&REV_01` | `viostor` (`viostor.inf` / `viostor.sys`) |
| virtio-input | `PCI\VEN_1AF4&DEV_1052&REV_01` | `vioinput` (best-effort; Win7 package not present in all virtio-win releases) |
| virtio-snd | `PCI\VEN_1AF4&DEV_1059&REV_01` | `viosnd` (optional; Win7 package not present in all virtio-win releases) |

Notes:

- `VEN_1AF4` is the conventional VirtIO PCI vendor ID used by the upstream ecosystem.
- Aero’s virtio device contract is `AERO-W7-VIRTIO` (see `docs/windows7-virtio-driver-contract.md`) and is **modern-only**:
  - virtio-pci vendor capabilities + BAR0 MMIO (no legacy I/O BAR)
  - PCI Revision ID `0x01`
  - device IDs in the virtio 1.0+ modern space (`0x1040 + <virtio device id>`)
- Many upstream virtio-win drivers also match the virtio-pci **transitional** ID range (the older `0x1000..` device IDs),
  but Aero contract v1 does not require and may not expose those IDs.
- The Aero contract major version is encoded in the PCI **Revision ID** (contract v1 = `REV_01`). QEMU virtio devices commonly enumerate as `REV_00` by default.
  - Aero’s in-tree Win7 virtio driver packages are revision-gated (`&REV_01`), and some drivers also validate the revision at runtime.
  - For QEMU-based testing with strict contract-v1 drivers, pass `x-pci-revision=0x01` on each `-device virtio-*-pci,...` arg (the Win7 host harness under `drivers/windows7/tests/host-harness/` does this automatically).

### Contract ↔ in-tree drivers ↔ Guest Tools config (virtio)

For Aero’s in-tree drivers and Guest Tools installer logic, the identifiers below must match **exactly**:

| Device | Contract PCI ID | In-tree driver INF | Windows service name | Guest Tools config |
|---|---|---|---|---|
| virtio-net | `1AF4:1041` (REV `0x01`) | `drivers/windows7/virtio-net/inf/aero_virtio_net.inf` | `aero_virtio_net` | `guest-tools/config/devices.cmd`: `AERO_VIRTIO_NET_SERVICE`, `AERO_VIRTIO_NET_HWIDS` |
| virtio-blk | `1AF4:1042` (REV `0x01`) | `drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf` | `aero_virtio_blk` | `guest-tools/config/devices.cmd`: `AERO_VIRTIO_BLK_SERVICE`, `AERO_VIRTIO_BLK_HWIDS` |
| virtio-input | `1AF4:1052` (REV `0x01`) | `drivers/windows7/virtio-input/inf/aero_virtio_input.inf` | `aero_virtio_input` | `guest-tools/config/devices.cmd`: `AERO_VIRTIO_INPUT_SERVICE`, `AERO_VIRTIO_INPUT_HWIDS` |
| virtio-snd | `1AF4:1059` (REV `0x01`) | `drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf` | `aero_virtio_snd` | `guest-tools/config/devices.cmd`: `AERO_VIRTIO_SND_SERVICE`, `AERO_VIRTIO_SND_HWIDS` |

Guest Tools uses:

- `AERO_VIRTIO_BLK_SERVICE` to configure the storage service as `BOOT_START` and to pre-seed `CriticalDeviceDatabase`.
- `AERO_VIRTIO_*_HWIDS` to enumerate the hardware IDs the installer should expect. For `AERO-W7-VIRTIO` v1, include `&REV_01` patterns for contract-major safety (the in-tree `aero_virtio_{blk,net,input,snd}.inf` files match `...&REV_01`, and some drivers also validate the revision at runtime).

Note: `guest-tools/config/devices.cmd` is generated from a Windows device contract JSON
(see `scripts/generate-guest-tools-devices-cmd.py`) and is regenerated during Guest Tools packaging
from the contract passed to the packager (`ci/package-guest-tools.ps1 -WindowsDeviceContractPath`):

- `docs/windows-device-contract.json` (canonical; Aero in-tree driver service names like `aero_virtio_blk` / `aero_virtio_net`)
- `docs/windows-device-contract-virtio-win.json` (virtio-win; upstream service names like `viostor` / `netkvm`)

CI-style drift check (no rewrite):

```bash
python3 scripts/ci/gen-guest-tools-devices-cmd.py --check
```

Virtio-win Guest Tools builds must use the virtio-win contract so `guest-tools/setup.cmd` can
validate the boot-critical storage INF `AddService` name and pre-seed `CriticalDeviceDatabase`
without requiring `/skipstorage` while keeping Aero’s modern-only PCI HWID patterns (`REV_01`).

### Licensing policy (virtio-win-derived artifacts)

Aero aims for permissive licensing (MIT/Apache-2.0). Official CI/release artifacts ship the in-tree drivers from this repo under the project licenses. If you redistribute **virtio-win-derived** media (driver packs / Guest Tools built from `virtio-win.iso`), you must comply with virtio-win’s licensing/redistribution terms and ship the corresponding license texts/notices.

**Upstream reference points (to pin during implementation):**

- Driver sources: `kvm-guest-drivers-windows` (virtio-win project)
- Binary distribution: `virtio-win.iso` (virtio-win project)

In practice, virtio-win’s driver sources are typically distributed under a **BSD-style permissive license** (commonly BSD-3-Clause), which is compatible with Aero’s licensing goals, but Aero should still pin a version and ship the exact license texts for the specific artifacts it redistributes.

**Repository policy:**

- If Aero **vendors** virtio-win artifacts (or a source subtree), we must include:
  - the upstream license texts
  - a pinned upstream version/commit reference
  - a `THIRD_PARTY_NOTICES.md` attribution file (or equivalent redistribution notice document)
- If Aero chooses not to vendor binaries and instead requires users to supply them, we still document the flow, but Aero is no longer “shipping” the drivers.

This repo provides a **packaging + ISO build story** that works either way. The directory layout and tooling are designed so that later work can:

- vendor a pinned virtio-win subset directly into `drivers/virtio/prebuilt/`, or
- populate `drivers/virtio/prebuilt/` from an externally obtained virtio-win ISO.

Practical note: this repo generally avoids committing `.sys` driver binaries directly. Instead, it provides tooling to build/pin driver packs and emits installable artifacts via CI/release workflows.

---

## Packaging layout (virtio-win driver packs)

The virtio-win extraction tooling (`drivers/scripts/make-driver-pack.ps1` and related wrappers) uses the following layout under `drivers/virtio/`:

```
drivers/virtio/
  manifest.json
  THIRD_PARTY_NOTICES.md
  prebuilt/                  # where real driver files go
    win7/
      x86/
        viostor/
          viostor.inf
          viostor.sys
          viostor.cat
        netkvm/
          netkvm.inf
          netkvm.sys
          netkvm.cat
        vioinput/            # best-effort
        viosnd/              # optional
      amd64/                 # recommended (required for Win7 x64 Setup + x64 guests)
        viostor/
        netkvm/
        ...
  sample/                    # repo-owned placeholders used by CI/tests
    ...
```

Notes:

- For the virtio-win-derived pack, Aero only **requires** `viostor` (storage) + `netkvm` (network).
- `vioinput` and `viosnd` are optional and may be missing from some virtio-win versions; the packaging scripts handle this by default.
- CI/release Guest Tools uses **in-tree** driver packages and a different spec/driver naming (`virtio-blk`, `virtio-net`, ...). See `drivers/README.md`.

### Why we require `.inf` + `.sys` + `.cat`

- Windows installs PnP drivers via an **INF**.
- On x64, Windows requires kernel drivers to be **signed**; the signature is normally validated via the **catalog (`.cat`)** for the driver package.
- On Win7 x86, signature enforcement is generally off by default, but keeping `.cat` in the package makes the flow consistent.

---

## Building the “drivers ISO” (emulator UX integration)

Aero should expose a UX action like:

> “Mount driver ISO…” → select `aero-virtio-win7-drivers.iso`

The ISO is a simple ISO-9660/Joliet filesystem containing the `win7/` driver directories.

### Which ISO?

There are two related but distinct “driver ISO” concepts in Aero:

1) **Minimal virtio drivers ISO** (`aero-virtio-win7-drivers.iso`)
   - Contains only the extracted virtio driver packages under `win7/x86/...` and `win7/amd64/...`.
   - Intended for Windows Setup’s **Load Driver** flow (so Setup can see a virtio-blk boot disk).

2) **Aero Guest Tools ISO** (`aero-guest-tools.iso`)
   - Contains install scripts (`setup.cmd`), the driver payload under `drivers/x86/...` and `drivers/amd64/...`, and (optionally) certificate files under `certs/` depending on `manifest.json` `signing_policy`.
   - Intended for the recommended post-install flow:
     - install Win7 using baseline emulated devices (AHCI HDD + IDE/ATAPI CD-ROM, e1000)
       - see [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md) for the canonical Win7 storage topology
     - mount Guest Tools ISO
     - run `setup.cmd`
     - switch VM devices to virtio and reboot

This document covers both; see the sections below.

### Tooling provided

- `tools/driver-iso/build.py` builds an ISO from a driver root directory and `drivers/virtio/manifest.json`.
- `tools/driver-iso/verify_iso.py` verifies the ISO contains the required INF files.

The builder requires an ISO authoring tool (Linux: `xorriso` preferred; also supports `genisoimage`/`mkisofs`).

### Multi-arch driver ISOs are recommended (x86 + amd64)

Windows will only load kernel drivers that match the OS architecture:

- **Windows 7 x64 Setup** (“Load Driver”) requires **amd64** drivers.
- **Windows 7 x86 Setup** requires **x86** drivers.

To prevent accidentally producing an ISO that can’t be used for Win7 x64 installs, `tools/driver-iso/build.py`
and `tools/driver-iso/verify_iso.py` default to:

```text
--require-arch both
```

This requires the minimum driver set (at least `viostor` + `netkvm`) to be present for **both** architectures.

Example (from repo root):

```bash
python3 tools/driver-iso/build.py \
  --drivers-root drivers/virtio/prebuilt \
  --output dist/aero-virtio-win7-drivers.iso
```

### Building a single-arch ISO (x86-only or amd64-only)

If you intentionally want a single-arch ISO, pass `--require-arch`:

```bash
# Win7 x86-only drivers ISO (will NOT work for Win7 x64 installs)
python3 tools/driver-iso/build.py \
  --require-arch x86 \
  --drivers-root drivers/virtio/prebuilt \
  --output dist/aero-virtio-win7-drivers-x86.iso

# Win7 amd64-only drivers ISO
python3 tools/driver-iso/build.py \
  --require-arch amd64 \
  --drivers-root drivers/virtio/prebuilt \
  --output dist/aero-virtio-win7-drivers-amd64.iso
```

The verifier supports the same flag:

```bash
python3 tools/driver-iso/verify_iso.py \
  --require-arch x86 \
  --iso dist/aero-virtio-win7-drivers-x86.iso
```

To build a demo ISO from placeholders:

```bash
python3 tools/driver-iso/build.py \
  --drivers-root drivers/virtio/sample \
  --output dist/aero-virtio-win7-drivers-sample.iso
```

### Build an ISO from an upstream virtio-win ISO (optional / compatibility)

On Windows you can mount `virtio-win.iso` directly via `Mount-DiskImage`.

On Linux/macOS, you can:

- extract first with `tools/virtio-win/extract.py` and then pass `-VirtioWinRoot`, or
- run under `pwsh` and pass `-VirtioWinIso` (auto-extract fallback when `Mount-DiskImage` is unavailable or fails), or
- use the one-shot `.sh` wrappers under `drivers/scripts/`.

Quick one-liner wrapper (does both steps):

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-virtio-driver-iso.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutIso .\dist\aero-virtio-win7-drivers.iso
```

1) Extract a Win7 driver pack from the ISO:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-driver-pack.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -NoZip
```

By default this requires `viostor` + `netkvm` and attempts to include `vioinput` + `viosnd` best-effort (emits a warning if missing). To control this explicitly:

- Minimal pack: `-Drivers viostor,netkvm`
- Strict optional (fail if audio/input are missing): `-StrictOptional`

On Linux/macOS:

Option A (recommended): extract first, then use `-VirtioWinRoot`:

```bash
python3 tools/virtio-win/extract.py \
  --virtio-win-iso virtio-win.iso \
  --out-root /tmp/virtio-win-root

pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot /tmp/virtio-win-root -NoZip
```

Option B: pass `-VirtioWinIso` directly under `pwsh` (auto-extract fallback on non-Windows when mounting is unavailable or fails):

```bash
pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinIso virtio-win.iso -NoZip
```

Notes:

- `tools/virtio-win/extract.py` prefers `7z` (install `p7zip-full` on Ubuntu/Debian or `p7zip` via Homebrew on macOS).
  If you don’t have `7z`, install `pycdlib` (`python3 -m pip install pycdlib`) and pass `--backend pycdlib`.
- `drivers/scripts/make-driver-pack.ps1` requires PowerShell 7 (`pwsh`) on non-Windows hosts.
- See `tools/virtio-win/README.md` for details (what is extracted, backends, provenance fields).
- Convenience: `bash ./drivers/scripts/make-driver-pack.sh` wraps the extraction + `pwsh` invocation into one command on Linux/macOS.
- Convenience: `bash ./drivers/scripts/make-virtio-driver-iso.sh` and `bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh` provide one-shot wrappers for building the drivers ISO and Guest Tools media on Linux/macOS.

`tools/virtio-win/extract.py` also writes a machine-readable provenance file to:

- `/tmp/virtio-win-root/virtio-win-provenance.json`

`drivers/scripts/make-driver-pack.ps1` will ingest this file when present so the produced
driver pack `manifest.json` can record the original ISO hash/volume label even when using
`-VirtioWinRoot`.

The extractor also copies common root-level license/notice files (e.g. `LICENSE*`, `NOTICE*`,
`README*`) and small metadata files like `VERSION` into the extracted root so subsequent
packaging can propagate them (and derive a best-effort virtio-win version string) even on
non-Windows hosts.

This produces a staging directory (by default) at:

`drivers\out\aero-win7-driver-pack\`

The staging directory includes:

- `manifest.json` (provenance, including virtio-win ISO hash/volume label/version hints when available; also records which upstream license/notice files were copied when present)
- `THIRD_PARTY_NOTICES.md` (redistribution notices)
- `licenses/virtio-win/` (best-effort copy of upstream virtio-win license/notice files)

2) Build a mountable drivers ISO from that staging directory:

```powershell
python .\tools\driver-iso\build.py `
  --drivers-root .\drivers\out\aero-win7-driver-pack `
  --output .\dist\aero-virtio-win7-drivers.iso
```

Notes:

- `tools/driver-iso/build.py` needs an ISO authoring tool available on PATH:
  - Linux/WSL: `xorriso` is easiest
  - Windows: `oscdimg.exe` (Windows ADK) is commonly used

### Build `aero-guest-tools.iso` from a virtio-win ISO (optional / compatibility)

This produces a Guest Tools ISO that includes virtio drivers plus install scripts.

By default, the wrapper builds media with `signing_policy=none` (for WHQL/production-signed virtio-win drivers), so it does **not** require or inject any custom certificate files and `setup.cmd` will not prompt to enable Test Mode by default.

On a machine with Rust (`cargo`) installed:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

By default, the wrapper uses `-Profile full` (includes optional Win7 audio/input drivers when present; best-effort).

To build storage+network-only Guest Tools media, use:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -Profile minimal `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

If you are packaging **test-signed/custom-signed** drivers (not typical for virtio-win), you can override:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -Profile full `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local `
  -SigningPolicy test
```

On Linux/macOS, you can either extract first and pass `-VirtioWinRoot`, pass `-VirtioWinIso` directly under `pwsh`
(auto-extract fallback when mounting is unavailable or fails), or use the `.sh` wrapper (`bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh`):

```bash
python3 tools/virtio-win/extract.py \
  --virtio-win-iso virtio-win.iso \
  --out-root /tmp/virtio-win-root

pwsh drivers/scripts/make-guest-tools-from-virtio-win.ps1 \
  -VirtioWinRoot /tmp/virtio-win-root \
  -OutDir ./dist/guest-tools \
  -Version 0.0.0 \
  -BuildId local
```

This wrapper:

1. Extracts a Win7 driver pack from `virtio-win.iso` (using `drivers/scripts/make-driver-pack.ps1`).
2. Converts it into the input layout expected by the Rust Guest Tools packager.
3. Runs `tools/packaging/aero_packager/` with the selected packaging profile (default: `full`):
   - `-Profile full` (default): `tools/packaging/specs/win7-virtio-full.json`
     - required: `viostor` + `netkvm`
     - optional (included if present): `vioinput` + `viosnd`
   - `-Profile minimal`: `tools/packaging/specs/win7-virtio-win.json` (required: `viostor` + `netkvm`)

Advanced overrides:

- `-SpecPath` overrides the profile’s spec selection.
- `-Drivers` overrides the profile’s driver extraction list.

Outputs:

- `dist/guest-tools/aero-guest-tools.iso`
- `dist/guest-tools/aero-guest-tools.zip`
- `dist/guest-tools/manifest.json`

The Guest Tools ISO/zip root also includes `THIRD_PARTY_NOTICES.md`, and will include
upstream virtio-win license/notice files (if present) under `licenses/virtio-win/`
(including `driver-pack-manifest.json` for virtio-win ISO provenance).

---

## Installing on Windows 7

For most users, the recommended installation path is **Aero Guest Tools** (`aero-guest-tools.iso`) and `setup.cmd`
(`docs/windows7-guest-tools.md`). The steps below are primarily useful for:

- installing storage drivers during **Windows Setup** (“Load driver”), or
- manual troubleshooting / non-standard packaging flows.

### virtio-blk (storage) during Windows 7 setup (“Load driver”)

If the Windows installer can’t see the disk:

1. Boot the Windows 7 installer ISO.
2. When you reach “Where do you want to install Windows?”, choose **Load Driver**.
3. Attach one of the following driver media sources:
   - **CI/release driver bundles** (`win7-drivers`): mount `AeroVirtIO-Win7-<version>.iso` or attach `*-fat.vhd` as a secondary disk (see [`docs/16-driver-install-media.md`](./16-driver-install-media.md) and `INSTALL.txt` at the media root for the exact layout).
   - **virtio-win-derived drivers ISO** (optional/compatibility): mount `aero-virtio-win7-drivers.iso` built by `drivers/scripts/make-virtio-driver-iso.ps1`.
4. Select the storage driver INF for your media:
   - **Aero in-tree virtio-blk**: `aero_virtio_blk.inf` (find it under the attached media; the directory names differ between Guest Tools vs driver bundle artifacts).
   - **virtio-win virtio-blk**: `viostor.inf` (typically under `\win7\amd64\viostor\` or `\win7\x86\viostor\`).
5. The virtio disk should appear; continue installation.

### Post-install via Device Manager (net / input / snd)

1. Boot Windows.
2. Open **Device Manager**.
3. For each unknown device (virtio-net, virtio-input, virtio-snd):
   - Right click → **Update Driver Software…**
   - “Browse my computer for driver software”
   - Point it at the mounted driver media (Guest Tools ISO/zip, CI driver bundle ISO, or virtio-win-derived drivers ISO)
   - Enable “Include subfolders”

### Post-install via pnputil

Windows 7 includes `pnputil.exe` (limited compared to newer Windows):

```bat
REM Example: virtio-win-derived drivers ISO
pnputil -i -a D:\win7\amd64\viostor\viostor.inf
pnputil -i -a D:\win7\amd64\netkvm\netkvm.inf

REM Example: Aero Guest Tools media (in-tree drivers)
pnputil -i -a X:\drivers\amd64\aero_virtio_blk\aero_virtio_blk.inf
pnputil -i -a X:\drivers\amd64\aero_virtio_net\aero_virtio_net.inf
```

Replace `D:` / `X:` with your mounted media drive letter and use the correct architecture directory
(`x86` vs `amd64` / `x64`) for your guest.

If you’re using the FAT driver disk image instead, the layout is typically:

- `E:\x86\<driver>\*.inf`
- `E:\x64\<driver>\*.inf`

---

## Win7 x64: test-signing mode + test certificate tooling

Windows 7 x64 enforces driver signature checks. There are three practical scenarios:

1. **Using Aero CI/release driver artifacts (in-tree drivers, typically test-signed)**: requires trusting the signing certificate and enabling Test Signing (or using `nointegritychecks`, not recommended). Guest Tools media (`signing_policy=test`) guides this flow.
2. **Using upstream WHQL/production-signed virtio-win packages**: should install without enabling test mode.
3. **Using modified drivers / self-built drivers**: requires test signing mode (or a production code-signing certificate + cross-signing, which is out of scope).

### SHA-256 signatures (Win7 update requirement)

If the driver catalogs (`.cat`) are signed with **SHA-256**, Windows 7 needs **KB3033929** to validate those signatures. Without it, drivers may fail to load with **Code 52** (“Windows cannot verify the digital signature…”).

This is a common failure mode when using modern tooling to sign Win7 drivers.

### Enable test-signing mode (Win7 x64)

Run an elevated Command Prompt:

```bat
bcdedit /set testsigning on
shutdown /r /t 0
```

To disable:

```bat
bcdedit /set testsigning off
shutdown /r /t 0
```

### Generate a test certificate + sign drivers

See `tools/driver-signing/README.md` and scripts in `tools/driver-signing/`.

High-level process:

1. Create a code-signing certificate (self-signed is fine for test mode).
2. Import it into:
   - Trusted Root Certification Authorities
   - Trusted Publishers
3. Use the WDK `signtool.exe` to sign the `.cat` (and optionally `.sys`) files.

---

## Optional: offline injection into Windows install media (DISM)

If you want Windows Setup to “just work” with virtio storage without clicking “Load Driver”, inject drivers into `boot.wim` and `install.wim` using DISM on a Windows host:

1. Mount the image:
   - `dism /Mount-Wim /WimFile:X:\sources\boot.wim /Index:2 /MountDir:C:\mount\boot`
2. Add drivers:
   - `dism /Image:C:\mount\boot /Add-Driver /Driver:C:\drivers\win7\amd64\viostor /Recurse`
3. Commit/unmount:
   - `dism /Unmount-Wim /MountDir:C:\mount\boot /Commit`

Repeat for `install.wim` (pick the correct edition index).

### Important: test-signed drivers also require offline certificate trust

If the driver package you’re injecting is **test-signed** (common for development), you must also inject the public signing certificate into the offline certificate stores for **both**:

- `boot.wim` (WinPE / Setup index, typically 2)
- `install.wim` (the edition index you plan to install)

At minimum, inject into:

- `ROOT`
- `TrustedPublisher`

Recommended tooling (Windows host):

- End-to-end media servicing: `tools/windows/patch-win7-media.ps1`
- Offline hive injector: `tools/win-offline-cert-injector` (`win-offline-cert-injector --windows-dir <mount> --store ROOT --store TrustedPublisher <cert-file>...`)

For a much more complete, Win7 x64-focused servicing procedure (certificate injection + BCD template patching), see `docs/16-win7-image-servicing.md` and the helper script `drivers/scripts/inject-win7-wim.ps1`.

---

## Manual verification checklist (Aero)

**Storage (virtio-blk):**

- [ ] Windows 7 detects the virtio disk during setup (with driver loaded).
- [ ] Installation completes and boots from the virtio disk.
- [ ] Disk is usable (create/copy large files; no I/O errors).

**Networking (virtio-net):**

- [ ] Windows 7 detects virtio NIC and installs the driver.
- [ ] Windows obtains a DHCP lease using Aero’s network stack.
- [ ] Basic connectivity works (ICMP/HTTP via Aero proxy).

**Input (virtio-input, best-effort):**

- [ ] Device appears and installs.
- [ ] Mouse movement is smooth (no PS/2-rate limitations).

**Sound (virtio-snd, optional):**

- [ ] Device appears and installs.
- [ ] Audio output works (system sounds).
- [ ] A recording endpoint exists (Control Panel → Sound → Recording) and capture works (may be silent if no host input source is available).

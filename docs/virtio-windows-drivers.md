# Virtio Windows 7 Drivers (virtio-blk / virtio-net / virtio-input / virtio-snd)

## Goal

Enable Aero’s **virtio acceleration path** by making it straightforward to install Windows 7 drivers for:

- **virtio-blk** (storage) *(minimum deliverable)*
- **virtio-net** (network) *(minimum deliverable)*
- **virtio-input** (keyboard/mouse/tablet) *(best-effort; PS/2/USB HID remains fallback)*
- **virtio-snd** (audio) *(optional; HDA/AC’97 remains fallback)*

See also:

- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive device/feature/transport contract.
- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation reference for Win7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).

This document defines:

- The chosen driver approach (and licensing rationale)
- The required on-disk packaging layout (`.inf` + `.sys` + `.cat`)
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

## Chosen approach (short term): package upstream virtio-win drivers

**Longer term:** Aero also has ongoing work toward clean-room, permissively licensed Windows 7 virtio drivers under `drivers/windows7/`. Until the storage (`virtio-blk`) and network (`virtio-net`) drivers are production-ready, the practical path for enabling virtio acceleration is to package the upstream virtio-win driver set.

### Why

Implementing Windows 7 kernel drivers for Storport (block) and NDIS (network) from scratch is a major standalone project. For Aero’s near-term goals, the pragmatic path is to **reuse the existing, widely deployed virtio driver stack** used by QEMU/KVM.

### What we reuse

We target the **virtio-win** driver distribution (commonly shipped as `virtio-win.iso`) and specifically the packages:

| Aero device | virtio PCI ID (transitional / modern) | virtio-win package name (typical) |
|------------|----------------------------------------|-----------------------------------|
| virtio-net | `VEN_1AF4&DEV_1000` / `VEN_1AF4&DEV_1041` | `NetKVM` (`netkvm.inf` / `netkvm.sys`) |
| virtio-blk | `VEN_1AF4&DEV_1001` / `VEN_1AF4&DEV_1042` | `viostor` (`viostor.inf` / `viostor.sys`) |
| virtio-input | `VEN_1AF4&DEV_1011` / `VEN_1AF4&DEV_1052` | `vioinput` (best-effort; Win7 package not present in all virtio-win releases) |
| virtio-snd | `VEN_1AF4&DEV_1018` / `VEN_1AF4&DEV_1059` | `viosnd` (optional; Win7 package not present in all virtio-win releases) |

Notes:

- `VEN_1AF4` is the conventional VirtIO PCI vendor ID used by the upstream ecosystem.
- Aero’s current virtio device contract (see `docs/windows7-virtio-driver-contract.md`) is trending toward **modern-only** device IDs (`0x1040 + device_id`). Transitional IDs may still be exposed for compatibility, but drivers should not rely on them.

### Licensing policy (project requirement)

Aero aims for permissive licensing (MIT/Apache-2.0). This doc’s approach is only acceptable if the driver code we ship is under a license compatible with redistribution alongside an MIT/Apache project.

**Upstream reference points (to pin during implementation):**

- Driver sources: `kvm-guest-drivers-windows` (virtio-win project)
- Binary distribution: `virtio-win.iso` (virtio-win project)

In practice, virtio-win’s driver sources are typically distributed under a **BSD-style permissive license** (commonly BSD-3-Clause), which is compatible with Aero’s licensing goals, but Aero should still pin a version and ship the exact license texts for the specific artifacts it redistributes.

**Repository policy:**

- If Aero **vendors** virtio-win artifacts (or a source subtree), we must include:
  - the upstream license texts
  - a pinned upstream version/commit reference
  - a `THIRD_PARTY_NOTICES` entry
- If Aero chooses not to vendor binaries and instead requires users to supply them, we still document the flow, but Aero is no longer “shipping” the drivers.

This repo provides a **packaging + ISO build story** that works either way. The directory layout and tooling are designed so that later work can:

- vendor a pinned virtio-win subset directly into `drivers/virtio/prebuilt/`, or
- populate `drivers/virtio/prebuilt/` from an externally obtained virtio-win ISO.

Practical note: this repo generally avoids committing `.sys` driver binaries directly. Instead, it provides tooling to build/pin driver packs and emits installable artifacts via CI/release workflows.

---

## Packaging layout (what Aero expects on disk)

Drivers live under `drivers/virtio/`:

```
drivers/virtio/
  manifest.json
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

- For Windows 7, Aero only **requires** `viostor` (storage) + `netkvm` (network).
- `vioinput` and `viosnd` are optional and may be missing from some virtio-win versions; the packaging scripts handle this by default.

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
   - Contains install scripts (`setup.cmd`), certificates, and the driver payload under `drivers/x86/...` and `drivers/amd64/...`.
   - Intended for the recommended post-install flow:
     - install Win7 using AHCI/e1000
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

### Build an ISO from an upstream virtio-win ISO (recommended flow)

On Windows you can mount `virtio-win.iso` directly. On Linux/macOS, extract it first with
`tools/virtio-win/extract.py` and then pass `-VirtioWinRoot` to the PowerShell scripts.

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

```bash
python3 tools/virtio-win/extract.py \
  --virtio-win-iso virtio-win.iso \
  --out-root /tmp/virtio-win-root

pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot /tmp/virtio-win-root -NoZip
```

Notes:

- `tools/virtio-win/extract.py` prefers `7z` (install `p7zip-full` on Ubuntu/Debian or `p7zip` via Homebrew on macOS).
  If you don’t have `7z`, install `pycdlib` (`python3 -m pip install pycdlib`) and pass `--backend pycdlib`.
- `drivers/scripts/make-driver-pack.ps1` requires PowerShell 7 (`pwsh`) on non-Windows hosts.
- See `tools/virtio-win/README.md` for details (what is extracted, backends, provenance fields).

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

- `manifest.json` (provenance, including virtio-win ISO hash/volume label/version hints when available)
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

### Build `aero-guest-tools.iso` from a virtio-win ISO (recommended for end users)

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

If you are packaging **test-signed/custom-signed** drivers (not typical for virtio-win), you can override:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local `
  -SigningPolicy testsigning
```

On Linux/macOS, extract first and pass `-VirtioWinRoot`:

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
3. Runs `tools/packaging/aero_packager/` with the default "minimal" spec:
   - `tools/packaging/specs/win7-virtio-win.json` (required: `viostor` + `netkvm`)

To also include optional virtio drivers (if present in the input), pass:

- `-SpecPath tools/packaging/specs/win7-virtio-full.json` (optional: `vioinput` + `viosnd`)

Outputs:

- `dist/guest-tools/aero-guest-tools.iso`
- `dist/guest-tools/aero-guest-tools.zip`
- `dist/guest-tools/manifest.json`

The Guest Tools ISO/zip root also includes `THIRD_PARTY_NOTICES.md`, and will include
upstream virtio-win license/notice files (if present) under `licenses/virtio-win/`
(including `driver-pack-manifest.json` for virtio-win ISO provenance).

---

## Installing on Windows 7

### virtio-blk (storage) during Windows 7 setup (recommended)

If the Windows installer can’t see the disk:

1. Boot the Windows 7 installer ISO.
2. When you reach “Where do you want to install Windows?”, choose **Load Driver**.
   - If you have Aero’s packaged driver artifacts, you can also attach the optional FAT driver disk image (`*-fat.vhd`) as a secondary disk and browse `x86\` or `x64\` instead of mounting an ISO. See: [`docs/16-driver-install-media.md`](./16-driver-install-media.md).
3. Mount the Aero drivers ISO as a second CD-ROM and browse to:
   - `\win7\x86\viostor\` for Win7 32-bit
   - `\win7\amd64\viostor\` for Win7 64-bit
4. Select `viostor.inf`.
5. The virtio disk should appear; continue installation.

### Post-install via Device Manager (net / input / snd)

1. Boot Windows.
2. Open **Device Manager**.
3. For each unknown device (virtio-net, virtio-input, virtio-snd):
   - Right click → **Update Driver Software…**
   - “Browse my computer for driver software”
   - Point it at the mounted drivers ISO
   - Enable “Include subfolders”

### Post-install via pnputil

Windows 7 includes `pnputil.exe` (limited compared to newer Windows):

```bat
pnputil -i -a D:\win7\x86\viostor\viostor.inf
pnputil -i -a D:\win7\x86\netkvm\netkvm.inf
```

Replace `D:` with your mounted drivers ISO drive letter and `x86` with `amd64` as appropriate.

If you’re using the FAT driver disk image instead, the layout is typically:

- `E:\x86\<driver>\*.inf`
- `E:\x64\<driver>\*.inf`

---

## Win7 x64: test-signing mode + test certificate tooling

Windows 7 x64 enforces driver signature checks. There are three practical scenarios:

1. **Using upstream signed virtio-win packages**: should install without enabling test mode.
2. **Using modified drivers / self-built drivers**: requires test signing mode (or a production code-signing certificate + cross-signing, which is out of scope).
3. **CI/dev-only experimentation**: test mode is acceptable.

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

<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Windows 7 virtio-snd driver package (build + signing)

This directory contains the Aero **virtio-snd** Windows 7 SP1 driver sources, plus scripts to produce an **installable, test-signed** driver package.

The intended developer workflow is:

1. Build `virtiosnd.sys`
2. Copy it into `drivers/windows7/virtio-snd/inf/`
3. Generate a test certificate, generate a catalog (`.cat`), and sign `SYS + CAT`
4. Install on Windows 7 with test-signing enabled (Device Manager → “Have Disk…”)

## Directory layout

| Path | Purpose |
| --- | --- |
| `SOURCES.md` | Clean-room/source tracking record (see `drivers/windows7/LEGAL.md` §2.6). |
| `src/`, `include/`, `makefile` | Driver sources (WDK 7.1 build). |
| `inf/` | Driver package staging directory (INF/CAT/SYS live together for “Have Disk…” installs). |
| `scripts/` | Utilities for generating a test cert, generating the catalog, signing, and optional release packaging. |
| `cert/` | **Local-only** output directory for `.cer/.pfx` (ignored by git). |
| `release/` | Release packaging docs and output directory (ignored by git). |
| `docs/` | Driver implementation notes / references. |

## Prerequisites (host build/sign machine)

Any Windows machine that can run the Windows Driver Kit tooling.

You need the following tools in `PATH` (typically by opening a WDK Developer Command Prompt):

- `Inf2Cat.exe`
- `signtool.exe`
- `certutil.exe` (built into Windows)

## Build

See `docs/README.md` for WDK 7.1 build environment notes. The build must produce:

- `virtiosnd.sys`

Copy the built driver into the package staging folder:

```text
drivers/windows7/virtio-snd/inf/virtiosnd.sys
```

## Windows 7 test-signing enablement (test VM / machine)

On the Windows 7 test machine, enable test-signing mode from an elevated cmd prompt:

```cmd
bcdedit /set testsigning on
shutdown /r /t 0
```

## Test certificate workflow (generate + install)

### 1) Generate a test certificate (on the signing machine)

From `drivers/windows7/virtio-snd/`:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1
```

`make-cert.ps1` defaults to generating a **SHA-1-signed** test certificate for maximum compatibility with stock Windows 7 SP1.
If your environment cannot create SHA-1 certificates, you can opt into SHA-2 by rerunning with:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1 -AllowSha2CertFallback
```

Expected outputs:

```text
cert\aero-virtio-snd-test.cer
cert\aero-virtio-snd-test.pfx
```

> Do **not** commit `.pfx` files. Treat them like private keys.

### 2) Install the test certificate (on the Windows 7 test machine)

Copy `cert\aero-virtio-snd-test.cer` to the test machine, then run from an **elevated** PowerShell prompt:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-test-cert.ps1 -CertPath .\cert\aero-virtio-snd-test.cer
```

This installs the cert into:

- LocalMachine **Trusted Root Certification Authorities**
- LocalMachine **Trusted Publishers**

## Catalog generation (CAT)

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\make-cat.cmd
```

This runs `Inf2Cat` for both architectures:

- `7_X86`
- `7_X64`

Expected output (once `virtiosnd.sys` exists in `inf/`):

```text
inf\aero-virtio-snd.cat
```

## Signing (SYS + CAT)

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\sign-driver.cmd
```

`sign-driver.cmd` will prompt for the PFX password. You can also pass it as the first argument or set `PFX_PASSWORD` in the environment.

This signs:

- `inf\virtiosnd.sys`
- `inf\aero-virtio-snd.cat`

## Installation (Device Manager → “Have Disk…”)

1. Device Manager → right-click the virtio-snd PCI device → **Update Driver Software**
2. **Browse my computer**
3. **Let me pick** → **Have Disk…**
4. Browse to `drivers/windows7/virtio-snd/inf/`
5. Select `aero-virtio-snd.inf`

## Offline / slipstream installation (optional)

If you want virtio-snd to bind automatically on first boot (for example when building unattended Win7 images), see:

- `tests/offline-install/README.md`

## Release packaging (optional)

Once the package has been built/signed, you can stage a Guest Tools–ready folder under `release\<arch>\virtio-snd\` using:

- `scripts/package-release.ps1` (see `release/README.md`)

The same script can also produce a deterministic ZIP bundle from `inf/` by passing `-Zip`.

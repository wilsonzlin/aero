# 16 - Driver Install Media (FAT Image)

Some Windows 7 installer flows (including WinPE and some emulators/VMs) make it easier to load drivers from a small FAT-formatted disk than from an ISO. To support this, CI can optionally produce a **mountable FAT32 disk image** containing the signed driver packages.

## Artifact

When enabled, driver packaging produces:

- `out/artifacts/AeroVirtIO-Win7-<version>-fat.vhd` (via `ci/package-drivers.ps1 -MakeFatImage`)

This is a FAT32-formatted VHD containing:

- `aero-test.cer` *(SigningPolicy=test only)*
- `INSTALL.txt`
- `x86/` (signed 32-bit drivers, grouped by driver name)
- `x64/` (signed 64-bit drivers, grouped by driver name)

## Creating the FAT image locally

The image is created with built-in Windows tooling (DiskPart). It requires:

- Windows
- Administrator privileges (VHD attach/mount + formatting)

Run:

```powershell
pwsh ci/package-drivers.ps1 -MakeFatImage
```

Notes:

- By default, `ci/package-drivers.ps1` uses `-SigningPolicy test`, so the produced FAT image contains
  `aero-test.cer` and `INSTALL.txt` includes test-signing + cert import steps.
- For production/WHQL-signed drivers, pass `-SigningPolicy production` (or `none`) to omit the certificate
  and to generate `INSTALL.txt` without test-signing instructions.

In CI, you can enable FAT image creation without changing invocation by setting:

```text
AERO_MAKE_FAT_IMAGE=1
```

To make FAT image creation a hard requirement, pass:

- `ci/package-drivers.ps1 -FatImageStrict` (packaging-level strictness), or
- `ci/make-fat-image.ps1 -Strict` (image creation strictness)

To build a FAT image from an already-prepared directory (containing `INSTALL.txt`, `x86/`, `x64/`, and optionally `aero-test.cer` when using `-SigningPolicy test`), run:

```powershell
pwsh ci/make-fat-image.ps1 -SourceDir <prepared-dir> -OutFile out/artifacts/aero-drivers-fat.vhd -SigningPolicy test
```

For production/WHQL-signed driver media (no bundled cert), pass:

```powershell
pwsh ci/make-fat-image.ps1 -SourceDir <prepared-dir> -OutFile out/artifacts/aero-drivers-fat.vhd -SigningPolicy production
```

If your environment cannot create or mount VHDs, the script **skips FAT image creation** with a warning by default. To make this a hard failure, pass `-Strict`.

## Using it during Windows 7 Setup ("Load Driver")

Attach the VHD as a **secondary disk** in your VM/emulator (or mount it on the host and copy its contents to a FAT32 USB stick).

In Windows Setup:

1. Click **Load Driver**
2. Browse the attached disk
3. Select the correct architecture folder (`x86` or `x64`)
4. Pick the `.inf` for the driver you need (e.g., `x64\<driver>\*.inf`)

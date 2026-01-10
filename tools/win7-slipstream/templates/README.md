# Win7 slipstream templates (golden references)

This directory contains **clean-room** templates intended for a future `tools/win7-slipstream` helper that prepares Windows 7 SP1 install media for Aero development.

These files contain **no Microsoft binaries** and are **not tied to any specific Windows image**. They are “golden reference” inputs that a future tool may copy and then substitute placeholders.

See also: `docs/16-windows7-install-media-prep.md`.

For ready-to-edit, end-to-end unattended templates and Win7-compatible post-install scripts (test signing + driver install), also see:

- `windows/win7-sp1/unattend/`

---

## Files

- `autounattend.drivers-only.xml`
  - Adds WinPE + offline-servicing driver paths, but leaves the install mostly interactive.
- `autounattend.full.xml`
  - Example full unattended install skeleton (disk layout, edition selection, user placeholders).
- `setupcomplete.cmd`
  - Template for `%WINDIR%\\Setup\\Scripts\\SetupComplete.cmd` (runs as SYSTEM near the end of Setup).
- `firstlogon.ps1`
  - Optional PowerShell companion script (usable from FirstLogonCommands or called from SetupComplete).

---

## Where these go on the install media

### `autounattend.xml` location

Windows Setup scans the root of removable/install media for `autounattend.xml`.

Typical usage:

1. Copy one template to the ISO root as `autounattend.xml`.
2. Substitute placeholders (paths, edition index, user details, signing mode).

### `SetupComplete.cmd` location

To have Setup copy a `SetupComplete.cmd` into the installed OS, it must be placed in the ISO tree at:

```
sources/$OEM$/$$/Setup/Scripts/SetupComplete.cmd
```

Windows Setup will copy `$OEM$\\$$` into `%WINDIR%` of the installed OS. The resulting path becomes:

```
%WINDIR%\Setup\Scripts\SetupComplete.cmd
```

### Staging drivers/certs for scripts

If scripts need to access drivers and certs after installation, place them under:

```
sources/$OEM$/$1/Aero/...
```

Windows Setup copies `$OEM$\\$1` into `%SystemDrive%` (usually `C:\`). Example resulting path:

```
C:\Aero\certs\aero-test-root.cer
C:\Aero\drivers\...
```

---

## Placeholder substitution

The templates use `{{PLACEHOLDER}}` markers. A future tool can replace these with real values.

Common placeholders used across templates:

- `{{ARCH}}`
  - `x86` or `amd64` (used in unattend `processorArchitecture`).
- `{{AERO_CERT_FILENAME}}`
  - e.g., `aero-test-root.cer`
- `{{AERO_SIGNING_MODE}}`
  - `testsigning` (preferred) or `nointegritychecks` (fallback / emulator-only)
- `{{AERO_WINPE_DRIVER_PATH}}`
  - e.g., `%configsetroot%\Drivers\WinPE\{{ARCH}}` (matches `windows/win7-sp1/unattend/` layout)
- `{{AERO_SYSTEM_DRIVER_PATH}}`
  - e.g., `%configsetroot%\Drivers\Offline\{{ARCH}}` (matches `windows/win7-sp1/unattend/` layout)
- `{{INSTALL_WIM_INDEX}}`
  - Install edition index inside `sources/install.wim` (only used in `autounattend.full.xml`).

---

## Clean-room note

These templates were authored from scratch using publicly documented unattended setup schemas. They intentionally avoid copying any vendor sample files verbatim.

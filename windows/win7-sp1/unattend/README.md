# Windows 7 SP1 Unattended Templates (Driver Injection + Scripting)

This directory contains **reference `autounattend.xml` templates** for Windows 7 SP1 that focus on:

- Loading **setup-critical** drivers in WinPE (storage/NIC)
- Staging drivers into the **offline OS** driver store
- Providing a clean hook for **post-install scripting** via `SetupComplete.cmd`

Files:

- `autounattend_amd64.xml` (Windows 7 x64)
- `autounattend_x86.xml` (Windows 7 x86)
- `scripts/` (Win7 SP1 `cmd.exe` post-install automation: test signing + unattended driver install)

These templates are intentionally minimal; users are expected to edit image selection, locale, and account settings.

See: [`docs/16-win7-unattended-install.md`](../../../docs/16-win7-unattended-install.md)

---

## How to use

1. Pick the template matching your ISO architecture.
2. Rename it to `autounattend.xml`.
3. Put `autounattend.xml` at the **root** of a removable/config media image (USB or ISO).
4. Boot Windows Setup with that media attached (in addition to the Windows 7 ISO).

Windows Setup scans attached removable media for `autounattend.xml` at startup.

---

## Expected config media layout

The templates reference driver and script paths relative to `%configsetroot%`:

```
<config-media-root>/
  autounattend.xml
  Drivers/
    WinPE/
      amd64/
      x86/
    Offline/
      amd64/
      x86/
  Scripts/
    SetupComplete.cmd        (copy from `windows/win7-sp1/unattend/scripts/SetupComplete.cmd`)
    InstallDriversOnce.cmd   (copy from `windows/win7-sp1/unattend/scripts/InstallDriversOnce.cmd`)
    FirstLogon.cmd        (optional)
  Cert/
    aero_test.cer         (optional; preferred name for the unattended scripts)
  Certs/
    AeroTestRoot.cer      (optional; accepted for compatibility)
```

Notes:

- **WinPE drivers** (`Drivers/WinPE/...`) are for storage/NIC drivers needed by Setup itself.
- **Offline drivers** (`Drivers/Offline/...`) are staged into the installed OS driver store during `offlineServicing`.
- `Scripts/SetupComplete.cmd` is copied into `%WINDIR%\\Setup\\Scripts\\SetupComplete.cmd` during the `specialize` pass.
- `Scripts/InstallDriversOnce.cmd` is invoked via a scheduled task created by `SetupComplete.cmd` at the next boot.
- If `Scripts/FirstLogon.cmd` exists, the templates also copy it into `%WINDIR%\\Setup\\Scripts\\FirstLogon.cmd` and run it via `FirstLogonCommands`.

> Verify on real Win7 setup: the availability of `%configsetroot%` after the first reboot depends on how Setup handles configuration sets in your scenario. For robustness, keep the config ISO attached until the desktop appears and/or copy needed files to the system drive during `specialize`.

---

## What you can safely edit

Common edits:

- **Which Windows edition to install**: `Microsoft-Windows-Setup` → `ImageInstall` → `/IMAGE/INDEX` (or swap to `/IMAGE/NAME`).
- **Locale/time zone**: `Microsoft-Windows-International-Core(-WinPE)` and `Microsoft-Windows-Shell-Setup`.
- **User/account settings**: `Microsoft-Windows-Shell-Setup` → `UserAccounts`.
- **Product key**: `Microsoft-Windows-Setup` → `UserData` (optional; many installs omit it and activate later).

---

## What you generally should not touch (unless you know why)

- `UseConfigurationSet=true` (`Microsoft-Windows-Setup`): this is what allows `%configsetroot%`-based layouts to work reliably.
- The `PnpCustomizations*` driver-injection sections:
  - `Microsoft-Windows-PnpCustomizationsWinPE` (`windowsPE`)
  - `Microsoft-Windows-PnpCustomizationsNonWinPE` (`offlineServicing`)
- The `specialize` `RunSynchronous` command that copies `SetupComplete.cmd` into place.

If you change folder layout (for example, move `Drivers/` or `Scripts/`), update all referenced paths accordingly.

---

## Disk layout rationale

The templates create **one active primary NTFS partition** on Disk 0 (MBR/BIOS-style). This keeps the install simple for VM/disk-image use cases.

If you need the 100MB “System Reserved” partition, GPT/UEFI, BitLocker, or a multi-partition layout, you can adjust `DiskConfiguration`, but ensure:

- `ImageInstall` → `InstallTo` matches the partition you intend to install Windows onto.
- The target partition is bootable for your firmware model (BIOS vs. UEFI).

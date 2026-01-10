# 16 - Windows 7 SP1 Unattended Install (Driver Injection + Post-Install Scripting)

This project needs a repeatable, zero-touch Windows 7 SP1 install path where:

- Users supply their own Windows 7 SP1 ISO (x86 or x64).
- Aero supplies only **open-source drivers** (virtio storage/NIC, virtual GPU, etc.) and **scripts**.

This document focuses on the *plumbing* required for:

1. Loading drivers early enough for Windows Setup to see the disk/network.
2. Staging additional drivers into the offline OS so they’re available on first boot.
3. Running scripts during setup to enable test-signing and install drivers.

See also the reference templates in [`windows/win7-sp1/unattend/`](../windows/win7-sp1/unattend/).

For the broader install-media preparation workflow (ISO layout, what must be patched in WIM/BCD/registry hives, ISO rebuild commands), see:

- [`docs/16-windows7-install-media-prep.md`](./16-windows7-install-media-prep.md)

For a step-by-step **validation + troubleshooting playbook** on real Win7 SP1 installs (logs, `%configsetroot%` verification, `$OEM$` copy behavior, `SetupComplete.cmd` checks), see:
 
* [`docs/17-win7-unattend-validation.md`](./17-win7-unattend-validation.md)

---

## Windows 7 setup pass order (what runs when)

Windows 7 unattended setup is driven by passes in `autounattend.xml`. The high-level flow for a clean install looks like:

1. **`windowsPE`** (WinPE / Windows Setup environment)
   - Disk partitioning / formatting
   - Choosing which image to apply from `install.wim`
   - Accepting EULA
   - **Loading setup-critical drivers** (storage, NIC)
2. **`offlineServicing`** (offline OS image on disk, not booted yet)
   - **Staging drivers into the offline OS driver store**
   - (Also used for offline package/feature injection, if needed)
3. **Reboot** into the newly applied OS
4. **`specialize`** (first boot configuration; runs as **SYSTEM**)
   - Computer- and machine-level configuration
   - **`RunSynchronous`** hooks (SYSTEM) for early scripting
5. **`oobeSystem`** (OOBE / first-run; some parts run as **SYSTEM**, some as the first user)
   - User creation / OOBE suppression
   - **`FirstLogonCommands`** hooks (user context)
6. **`%WINDIR%\\Setup\\Scripts\\SetupComplete.cmd`** and first logon
   - `SetupComplete.cmd` runs **once** as **SYSTEM** near the end of setup (before the first logon)
   - First logon triggers `FirstLogonCommands` (if configured)

> Note: The exact boundaries between “end of setup”, `SetupComplete.cmd`, and “first logon” can be confusing. The practical takeaway is: if you need **SYSTEM context without depending on a user session**, prefer `specialize` / `SetupComplete.cmd`.

---

## Driver injection (WinPE vs. offline OS)

Windows 7 has two different unattend components for drivers:

- **WinPE/setup environment (setup-critical):** `Microsoft-Windows-PnpCustomizationsWinPE` (`windowsPE`)
- **Offline OS staging (post-apply):** `Microsoft-Windows-PnpCustomizationsNonWinPE` (`offlineServicing`)

These solve different problems and are often used together.

### 1) Loading setup-critical drivers in WinPE (`windowsPE`)

**Component:** `Microsoft-Windows-PnpCustomizationsWinPE`  
**Pass:** `windowsPE`  
**Purpose:** Make drivers available *while Windows Setup is running in WinPE*.

This is the right place for drivers needed for:

- Storage controller access (so Setup can see the target disk)
- Network access (if you plan to pull content from the network during setup)

**Key setting:**

`DriverPaths/PathAndCredentials/Path` — a list of directories containing `.inf` driver packages.

Typical pattern (see templates):

- Put storage/NIC drivers under `Drivers\WinPE\<arch>\...`
- Point `DriverPaths` at that directory

> Verify on real Win7 setup: whether `DriverPaths` is scanned recursively for `.inf` files vs. only the top directory can vary across tooling and versions. To avoid surprises, keep drivers in a shallow directory structure or add multiple `PathAndCredentials` entries.

### 2) Staging drivers into the offline OS (`offlineServicing`)

**Component:** `Microsoft-Windows-PnpCustomizationsNonWinPE`  
**Pass:** `offlineServicing`  
**Purpose:** Add driver packages to the *offline* Windows installation on disk.

This is analogous to doing `DISM /Add-Driver` against the target `Windows` directory: the drivers are placed into the offline driver store so that on the first boot Windows can install them when it enumerates hardware.

Typical uses:

- GPU drivers
- Virtio drivers that are not strictly required for WinPE setup, but should be present in the installed OS
- Any device driver you want Windows to “just find” after first boot (INF-based packages)

> Important: This only stages **INF-based** drivers. If a vendor driver is only distributed as an interactive installer (`.exe` / `.msi`), it won’t be installed by `offlineServicing`—use `SetupComplete.cmd` or another scripting hook instead.

---

## Script execution hooks (where to run what)

Windows 7 gives multiple places to run commands; the main difference is **timing** and **security context**.

### Hook A: `specialize` → `Microsoft-Windows-Deployment` → `RunSynchronous` (SYSTEM)

- Runs on first boot after the image is applied.
- Runs as **LocalSystem**.
- Good for machine-level changes and for copying files into `%WINDIR%` locations.

Common uses in this project:

- Copy `SetupComplete.cmd` from installation/config media onto the installed OS.
- Copy drivers/certs to a stable path on the system drive.
- Enable test-signing early (though it still requires a reboot to take effect).

### Hook B: `oobeSystem` → `Microsoft-Windows-Shell-Setup` → `FirstLogonCommands` (user)

- Runs when a user logs in for the first time.
- Runs in the **user context** (which may or may not be an administrator).

Use this for tasks that require a user profile or per-user configuration. For fully unattended usage, pair it with `AutoLogon` or ensure OOBE creates/logs into the intended account automatically.

### Hook C: `%WINDIR%\\Setup\\Scripts\\SetupComplete.cmd` (SYSTEM, runs once)

- If present, Windows Setup runs it near the end of setup.
- Runs as **LocalSystem**.
- Runs **once** (per install).

This is often the easiest place to:

- Import certificates into LocalMachine stores
- Install INF drivers via `pnputil` (Win7 has `pnputil.exe`; exact flags differ by OS version—verify on Win7 SP1)
- Trigger a reboot (for example after enabling test signing)

### Recommended: use Aero's unattended scripts

This repo includes Win7 SP1-compatible post-install automation scripts that:

- Import an optional test certificate
- Enable test signing (`bcdedit /set testsigning on`)
- Install all `*.inf` packages under a `Drivers\` folder via `pnputil`
- Avoid reboot loops via marker files and self-deleting scheduled tasks

See:

- [`windows/win7-sp1/unattend/scripts/`](../windows/win7-sp1/unattend/scripts/)
- [`windows/win7-sp1/unattend/scripts/README.md`](../windows/win7-sp1/unattend/scripts/README.md)
- [`windows/win7-sp1/unattend/scripts/SetupComplete.cmd`](../windows/win7-sp1/unattend/scripts/SetupComplete.cmd)
- [`windows/win7-sp1/unattend/scripts/InstallDriversOnce.cmd`](../windows/win7-sp1/unattend/scripts/InstallDriversOnce.cmd)

### Example: `SetupComplete.cmd` skeleton

If you ship a config ISO that includes `Scripts\\SetupComplete.cmd`, you can use it to enable test signing, trust your signing certificate, and stage/install drivers.

Minimal example (treat this as a starting point and **verify on real Win7 SP1**):

```bat
@echo off
setlocal EnableExtensions

set LOG=%WINDIR%\Temp\Aero-SetupComplete.log
echo [%DATE% %TIME%] SetupComplete starting>>"%LOG%"

REM If you used UseConfigurationSet=true, Setup may provide %configsetroot%.
REM For robustness, copy payloads to C:\Aero\ during specialize (or use the
REM production scripts in windows/win7-sp1/unattend/scripts/, which do this by default).
set SRC=%configsetroot%
if not defined configsetroot (
  echo [%DATE% %TIME%] WARNING: configsetroot is not set>>"%LOG%"
)

REM Enable test signing (requires reboot to take effect)
bcdedit /set testsigning on>>"%LOG%" 2>&1

REM Trust the driver signing certificate (optional; adjust file name/store as needed)
set CERT=%SRC%\Cert\aero_test.cer
if not exist "%CERT%" set CERT=%SRC%\Cert\aero-test.cer
if not exist "%CERT%" set CERT=%SRC%\Cert\aero-test-root.cer
if not exist "%CERT%" set CERT=%SRC%\Certs\AeroTestRoot.cer
if exist "%CERT%" (
  certutil -addstore -f Root "%CERT%">>"%LOG%" 2>&1
  certutil -addstore -f TrustedPublisher "%CERT%">>"%LOG%" 2>&1
)

REM Stage/install INF drivers (verify pnputil flags on Win7 SP1)
REM - Staging only: pnputil -a <inf>
REM - Stage + install: pnputil -i -a <inf>
if exist "%SRC%\Drivers\Offline\amd64" (
  for /r "%SRC%\Drivers\Offline\amd64" %%I in (*.inf) do (
    pnputil -i -a "%%I">>"%LOG%" 2>&1
  )
)

REM Reboot if you enabled test signing (otherwise you can remove this)
shutdown /r /t 0
```

> Note: For x86 installs, change the driver folder to `...\Offline\x86`. For robustness, copy needed drivers/certs from `%configsetroot%` to a stable location (for example `C:\Aero\`) during the `specialize` pass. The production `SetupComplete.cmd` in `windows/win7-sp1/unattend/scripts/` performs this copy by default.

---

## Test-signing for Windows 7 x64 (test-signed drivers)

Windows 7 x64 enforces kernel-mode driver signing. If Aero’s drivers are test-signed (common for internal/open-source builds), you typically need both:

1. Windows booted with **test signing enabled**, and
2. The **signing certificate trusted** by the local machine.

### SHA-2 update note (KB3033929 / KB4474419)

If your driver packages (or the signing certificate itself) use **SHA-256** signatures, stock Windows 7 SP1 may be unable to validate them until SHA-2 support updates are installed (commonly **KB3033929** and **KB4474419**).

If you see signature/trust failures (for example Device Manager **Code 52**), see:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support)

### Enable test signing (requires reboot)

Run as Administrator (SYSTEM is fine):

```bat
bcdedit /set testsigning on
```

This setting does **not** take effect until you reboot.

### Import the signing certificate (LocalMachine)

Import into both `Root` and `TrustedPublisher`:

```bat
certutil -addstore -f Root "%configsetroot%\\Cert\\aero_test.cer"
certutil -addstore -f TrustedPublisher "%configsetroot%\\Cert\\aero_test.cer"
```

The unattended scripts in this repo accept several common certificate file names:

- `Cert\\aero_test.cer` (preferred)
- `Cert\\aero-test.cer`
- `Cert\\aero-test-root.cer`
- `Certs\\AeroTestRoot.cer` (legacy)

> Verify on real Win7 setup: the best store(s) depend on how the drivers are signed (cross-signed vs. test-signed). The above is a common baseline for test-signed driver packages.

### Suggested sequencing

If you’re enabling test signing during setup:

1. Run `bcdedit /set testsigning on`
2. Copy/import the cert
3. Reboot
4. Install drivers (or let staged drivers install automatically)

In practice, steps 1–3 fit well in `SetupComplete.cmd`, followed by `shutdown /r /t 0`.

---

## Recommended packaging strategy (no Windows file redistribution): “config ISO”

The recommended approach is to ship a **separate, tiny “config ISO”** (or USB image) that contains only:

- `autounattend.xml`
- driver `.inf` packages
- scripts (`SetupComplete.cmd`, etc.)
- certificates (optional)

This avoids redistributing any Microsoft files and makes the process reproducible.

> Note: If you only need drivers for an interactive install (Windows Setup → **Load Driver**),
> CI can also produce a small FAT32 “driver disk” (`*-fat.vhd`) that you attach as a secondary disk.
> See: [`docs/16-driver-install-media.md`](./16-driver-install-media.md).

### Expected config media layout

The reference templates assume a layout like:

```
<config-media-root>/
  autounattend.xml
  Drivers/
    WinPE/
      amd64/   (storage/NIC drivers needed by WinPE/Setup)
      x86/
    Offline/
      amd64/   (drivers to stage into installed OS)
      x86/
  Scripts/
    SetupComplete.cmd
    InstallDriversOnce.cmd
    FirstLogon.cmd        (optional)
  Cert/
    aero_test.cer         (optional; preferred)
    aero-test.cer         (optional; accepted)
    aero-test-root.cer    (optional; accepted)
  Certs/
    AeroTestRoot.cer      (optional; accepted for compatibility)
```

### Keeping access to config content after the first reboot

**Setting:** `Microsoft-Windows-Setup` → `UseConfigurationSet=true`

When enabled, Setup treats the media containing `autounattend.xml` as a *configuration set* and copies its contents onto the target disk so later passes can still access them (commonly via `%configsetroot%`).

> Verify on real Win7 setup: the exact copy location and the lifetime/availability of `%configsetroot%` varies across Windows versions and deployment scenarios. For robustness, keep the config ISO attached through first boot.
>
> The reference templates (and Aero's Win7 unattended scripts) can stage the payload into `C:\Aero\` during `specialize` so later phases don't depend on removable/config media remaining attached.

---

## Advanced strategy: slipstream drivers into `boot.wim` / `install.wim`

For advanced users (or for edge cases where you can’t rely on external config media), you can inject drivers directly into the Windows images:

- `boot.wim` (the WinPE/Setup environment)
- `install.wim` (the actual OS image)

High-level workflow (Windows host with DISM):

1. Mount `boot.wim` (typically index 2: “Microsoft Windows Setup”)
2. `dism /image:<mount> /add-driver /driver:<path> [/recurse]`
3. Unmount/commit
4. Repeat for `install.wim` (the index you plan to install)
5. Rebuild the ISO

Linux-friendly tooling: `wimlib-imagex` can mount/edit WIMs, but the exact commands depend on your environment.

Tradeoffs:

- More complex and error-prone
- Must be repeated for each ISO/edition
- Modifies Microsoft media (still fine for personal use, but changes your reproducibility story)

For Aero’s use case, the config ISO + unattend driver paths is the preferred baseline.

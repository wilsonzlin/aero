# Windows 7 SP1 unattended Aero driver installation (test signing)

This folder contains Windows 7 SP1 compatible `cmd.exe` scripts intended for use with `unattend.xml` to:

1. Import an Aero test certificate (optional)
2. Enable **test signing**
3. Install all driver packages (`*.inf`) under an Aero `Drivers\` payload folder without user interaction

No PowerShell is used and only built-in Win7 tools are required (`certutil`, `bcdedit`, `schtasks`, `pnputil`, `shutdown`).

## Expected payload layout

Primary expected location is `C:\Aero\`:

```
C:\Aero\
  Drivers\
    ...\*.inf
  Cert\
    aero_test.cer          (optional)
  Scripts\
    SetupComplete.cmd
    InstallDriversOnce.cmd
```

The scripts locate the payload using the following strategy:

1. `C:\Aero\`
2. `%configsetroot%` (if defined), and `%configsetroot%\Aero`
3. Scan drive letters `C:` → `Z:` for an `AERO.TAG` marker file:
   - `X:\Aero\AERO.TAG` (payload root is `X:\Aero\`)
   - `X:\AERO.TAG` (payload root is `X:\`)
4. As an additional convenience fallback, scan drive letters for a standard `X:\Aero\` folder that contains both `Drivers\` and `Scripts\` (no marker file required).

The marker file can be an empty file; it is only used to find the payload.

Notes:

- `InstallDriversOnce.cmd` also attempts to infer the payload root from its own location (parent of the `Scripts\` directory) before trying `C:\Aero` / `AERO.TAG` scanning. This makes it robust when the scheduled task runs the script directly from the payload.

## SetupComplete.cmd

`SetupComplete.cmd` is intended to be copied to:

```
%WINDIR%\Setup\Scripts\SetupComplete.cmd
```

It creates a marker file:

```
%WINDIR%\Temp\aero-setupcomplete.done
```

If this marker exists, the script exits without rebooting or making changes.

Logs are appended to:

```
%WINDIR%\Temp\aero-setup.log
```

### What it does

1. Locates the Aero payload root (`Drivers\`, `Cert\`, `Scripts\`).
2. If `Cert\aero_test.cer` exists, imports it into:
   - `Root`
   - `TrustedPublisher`

   For compatibility with existing config media layouts, it will also accept:
   - `Certs\AeroTestRoot.cer`
3. Enables test signing via `bcdedit /set testsigning on`
4. Creates a scheduled task (`Aero-InstallDriversOnce`) that runs `InstallDriversOnce.cmd` at next boot as `SYSTEM`.
5. Reboots immediately to apply boot configuration changes.

### Optional: nointegritychecks

At the top of `SetupComplete.cmd`:

```
set "AERO_ENABLE_NOINTEGRITYCHECKS=0"
```

Set to `1` only if you specifically need it.

### Optional: copy payload to C:\Aero

By default, `SetupComplete.cmd` copies the located payload (Drivers/Scripts/Cert) to `C:\Aero\` before enabling test signing and scheduling the driver install task.

This makes the next-boot driver installation independent of removable/config media drive letters.

If the payload's `Drivers\` folder uses the Win7 unattend layout (`Drivers\WinPE\<arch>` and `Drivers\Offline\<arch>`), only the matching architecture subfolders are copied.

To disable this behaviour, set:

```
set "AERO_COPY_PAYLOAD_TO_C=0"
```

### Disabling test signing / integrity changes later

These scripts enable test signing via `bcdedit`. To revert to normal mode later:

```cmd
bcdedit /set testsigning off
bcdedit /set nointegritychecks off
shutdown /r /t 0
```

## InstallDriversOnce.cmd

This script is intended to run once at boot via the scheduled task created by `SetupComplete.cmd`.

It creates a marker file:

```
%WINDIR%\Temp\aero-install-drivers.done
```

Logs are appended to:

```
%WINDIR%\Temp\aero-driver-install.log
```

### What it does

1. Locates the Aero payload root and `Drivers\` directory.
2. Runs `pnputil -i -a` for every `*.inf` under `Drivers\` (recursive).
   - Continues on failures
   - Records failing INF paths in the log and sets a non-zero exit code
3. Deletes the scheduled task (`Aero-InstallDriversOnce`) so it does not run again at future boots.

### Optional: reboot after installing drivers

At the top of `InstallDriversOnce.cmd`:

```
set "AERO_REBOOT_AFTER_DRIVER_INSTALL=0"
```

Set to `1` to reboot after at least one `pnputil` install command succeeds.

## Example unattend.xml copy step (specialize)

This is a sketch of how an unattend `specialize` phase command can place `SetupComplete.cmd` where Windows expects it.

Make sure `C:\Windows\Setup\Scripts` exists, then copy:

```cmd
cmd.exe /c mkdir "%WINDIR%\Setup\Scripts" ^&^& copy /y "C:\Aero\Scripts\SetupComplete.cmd" "%WINDIR%\Setup\Scripts\SetupComplete.cmd"
```

If your payload is on removable media, you can reference `%configsetroot%` or use the `AERO.TAG` marker mechanism described above.

Example using `%configsetroot%` (configuration set media):

```cmd
cmd.exe /c mkdir "%WINDIR%\Setup\Scripts" ^&^& copy /y "%configsetroot%\Scripts\SetupComplete.cmd" "%WINDIR%\Setup\Scripts\SetupComplete.cmd"
```

## Re-running

To re-run (for debugging), delete the marker files:

- `%WINDIR%\Temp\aero-setupcomplete.done`
- `%WINDIR%\Temp\aero-install-drivers.done`

Then re-run the scripts manually or re-create the scheduled task.

## Skipping

Both scripts are gated by marker files in `%WINDIR%\Temp`.

To skip `SetupComplete.cmd` (including enabling test mode), pre-create:

```
%WINDIR%\Temp\aero-setupcomplete.done
```

To skip driver installation, pre-create:

```
%WINDIR%\Temp\aero-install-drivers.done
```

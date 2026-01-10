# 17 - Windows 7 Unattended Install Validation & Troubleshooting

This document is a practical validation + debugging playbook for doing a **real Windows 7 SP1 installation** in a VM using:

* **CD0**: the official Windows 7 SP1 installer ISO
* **CD1**: an Aero “config ISO” containing `autounattend.xml` plus any drivers/scripts/payload

It is written to answer (by *verification*, not assumptions) the open questions around:

* whether `%configsetroot%` is available when booting with a separate config ISO,
* how Windows Setup treats a secondary CD/DVD (“config ISO”) during unattended install, and
* whether `$OEM$` is processed/copied when `$OEM$` is on that separate media.

Where behavior varies by hypervisor or media type, this guide uses **“expected”** language and provides concrete steps to prove what happened from logs and on-screen state.

See also (related docs in this repo):

* [`docs/16-win7-unattended-install.md`](./16-win7-unattended-install.md) (how to structure `autounattend.xml`, driver injection passes, and setup scripting hooks)
* [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md) (post-install driver signing/trust failures, Device Manager codes, `setupapi.dev.log` triage)

---

## Prerequisites

* A Windows 7 SP1 ISO (x86 or x64).
* An Aero config ISO (CD1) that contains at minimum:
  * `\autounattend.xml` (root of the ISO)
  * A unique marker file at the root, e.g. `\AERO_CONFIG.MEDIA` (recommended for drive discovery)
  * Any payload you expect to be copied/used (drivers, scripts, certificates, etc)
* A VM with:
  * 2+ GB RAM (4 GB preferred for x64)
  * 2 vCPUs
  * 25+ GB virtual disk
  * BIOS/Legacy boot (Win7 supports UEFI in some configs, but keep this simple)

---

## 1) End-to-end validation checklist (Win7 SP1 in a VM)

### A. VM + media setup (pre-boot)

1. Create a new VM and attach storage:
   - **Hard disk**: blank, 25+ GB
   - **CD0**: Windows 7 SP1 ISO
   - **CD1**: Aero config ISO (contains `autounattend.xml`)

2. Ensure the VM boot order prefers **CD/DVD** first (so Windows Setup boots).

3. (Recommended) Make CD1 easy to identify:
   - ISO volume label: `AERO-CONFIG` (or similar)
   - Root marker file: `AERO_CONFIG.MEDIA`

Why: Windows Setup/WinPE drive letters are not stable across hypervisors, and CD0/CD1 can swap.

---

### B. Boot + prove Setup is fully unattended

Boot the VM. A fully unattended flow should show **no prompts** for the items below.

#### Unattended proof points

Check off each item as it happens:

- [ ] **No “Press any key to boot from CD/DVD…” stalls** (if your hypervisor requires a keypress, this is fine; it’s outside Windows Setup).
- [ ] **No language/keyboard prompt** (or it is auto-accepted without interaction).
- [ ] **No EULA prompt** (EULA is accepted automatically).
- [ ] **No “Which version of Windows do you want to install?” prompt** (image selection is automatic).
- [ ] **No disk selection/partitioning UI** (disk partitioning is automatic).
- [ ] Setup proceeds directly through “Expanding Windows files”, “Installing features”, “Completing installation”, then reboots.
- [ ] **No OOBE user creation prompts** (username/computer name/timezone/etc are auto-configured).

If any prompt appears, skip to [Failure modes + fixes](#4-failure-modes--fixes) and to [Where to look for logs](#2-where-to-look-for-logs).

---

### C. Prove scripts ran (SetupComplete, testsigning, scheduled task lifecycle, drivers)

After the first boot to the desktop (or to the login screen, depending on your unattend), validate in this order.

#### 1) SetupComplete executed

`SetupComplete.cmd` is executed by Windows Setup at the end of installation, from:

* `%WINDIR%\Setup\Scripts\SetupComplete.cmd`

Validation steps:

1. Open an elevated command prompt (or run as Administrator).
2. Confirm the file exists:

```bat
dir "%WINDIR%\Setup\Scripts\SetupComplete.cmd"
```

3. Confirm the Aero script logs/markers exist (expected, if your scripts create them):

```bat
dir C:\Windows\Temp\aero-setup.log
dir C:\Windows\Temp\aero-driver-install.log
```

4. (Optional) Confirm Setup attempted to execute `SetupComplete.cmd` by searching Setup logs:

```bat
findstr /i /c:"setupcomplete" C:\Windows\Panther\setupact.log
```

What “good” looks like:

* `C:\Windows\Temp\aero-setup.log` exists and includes a timestamp and a line indicating it ran under `SYSTEM`.
* If driver install is separated into a second phase, `C:\Windows\Temp\aero-driver-install.log` exists as well.

If `SetupComplete.cmd` is missing or logs are missing, see [SetupComplete not running](#setupcomplete-not-running).

---

#### 2) testsigning enabled

If your workflow relies on test-signed drivers, the system should have testsigning enabled.

Run:

```bat
bcdedit /enum {current}
```

Expected snippet:

```text
Windows Boot Loader
-------------------
identifier              {current}
...
testsigning             Yes
```

Notes:

* If `testsigning` is missing or `No`, either testsigning wasn’t set, it was set on the wrong BCD entry, or a reboot is still required.
* In Win7, enabling testsigning typically requires a reboot before unsigned/test-signed drivers will load reliably.

---

#### 3) Scheduled task created and then removed

Some unattended flows create a startup scheduled task to finish post-install steps (e.g. driver install after the first boot) and remove it once complete.

Validation steps:

1. Immediately after reaching the desktop the first time, list tasks and look for “Aero”:

```bat
schtasks /query /fo LIST /v | findstr /i aero
```

2. Reboot once and run the same command again.

Expected behavior (example):

* First boot: an Aero-related task exists (name varies by implementation).
* After the post-install step completes: the task is deleted and no longer appears in `schtasks /query`.

If you don’t know the exact task name, use this as a broad check and rely on your custom logs (e.g. `aero-setup.log`) to print the created task name explicitly.

If tasks persist forever or you get reboot loops, see [Reboot loops / task never deleted](#reboot-loops--task-never-deleted).

---

#### 4) Drivers installed

How you validate driver installation depends on whether the VM hardware matches your drivers (virtio devices in QEMU, synthetic devices in Hyper-V, etc). The goal here is to prove *the installation mechanism worked* and to capture why it didn’t when it fails.

**A) Validate via your own logs (recommended)**

Check for logged `pnputil` output in:

* `C:\Windows\Temp\aero-driver-install.log`

You want to see successful add/install messages, e.g.:

```text
Processing inf :  netkvm.inf
Driver package added successfully.
Driver package installed successfully.
```

**B) Validate via `pnputil` driver store enumeration**

Run:

```bat
pnputil -e
```

Look for your driver provider/name in the output (for custom Aero drivers, consider using a unique provider string to make this grep-able).

**C) Validate in Device Manager**

* Run `devmgmt.msc`
* Confirm:
  * No “Unknown device” entries remain for the devices your drivers target.
  * The expected driver is bound to the expected device (Properties → Driver tab).

If installation fails, jump to [Unsigned/test-signed driver install blocked](#unsignedtest-signed-driver-install-blocked) and consult `setupapi.dev.log` in [Log locations](#2-where-to-look-for-logs).

---

## 2) Where to look for logs

These are the first places to look when unattended install does not behave as expected.

### Windows Setup logs (unattend parsing, config-set detection, pass execution)

During setup (WinPE), logs are written to the WinPE RAM disk:

* `X:\Windows\Panther\setupact.log`
* `X:\Windows\Panther\setuperr.log`
* `X:\Windows\Panther\UnattendGC\setupact.log`
* `X:\Windows\Panther\UnattendGC\setuperr.log`

After Windows is installed, logs are copied to:

* `C:\Windows\Panther\setupact.log`
* `C:\Windows\Panther\setuperr.log`
* `C:\Windows\Panther\UnattendGC\setupact.log`
* `C:\Windows\Panther\UnattendGC\setuperr.log`
* (If setup failed/rolled back) `C:\Windows\Panther\Rollback\setupact.log`
* (If setup failed/rolled back) `C:\Windows\Panther\Rollback\setuperr.log`

What each proves:

* `setupact.log`: the canonical timeline of Windows Setup actions (disk config, image apply, reboots, pass transitions).
* `setuperr.log`: errors only (start here when something failed).
* `UnattendGC\setupact.log`: unattend processing and “generated catalog” / “unattend gather” behavior. This is often where you’ll see:
  * which unattend file was selected,
  * which passes were applied,
  * and whether a configuration set was detected.

Useful searches:

```bat
findstr /i /c:"unattend" C:\Windows\Panther\setupact.log
findstr /i /c:"autounattend" C:\Windows\Panther\setupact.log
findstr /i /c:"configset" C:\Windows\Panther\setupact.log
findstr /i /c:"configsetroot" C:\Windows\Panther\setupact.log
findstr /i /c:"unattend" C:\Windows\Panther\UnattendGC\setupact.log
findstr /i /c:"setupcomplete" C:\Windows\Panther\setupact.log
```

---

### Driver install logs (PnP binding, signature enforcement, INF parsing)

* `C:\Windows\inf\setupapi.dev.log`

What it proves:

* Every device driver bind attempt, including:
  * which INF matched,
  * why a match was rejected (signature, rank, missing files),
  * whether the driver installed successfully.

Useful searches:

```bat
findstr /i /c:"!!!" C:\Windows\inf\setupapi.dev.log
findstr /i /c:"failed" C:\Windows\inf\setupapi.dev.log
findstr /i /c:".inf" C:\Windows\inf\setupapi.dev.log
```

`!!!` lines are the high-signal failure markers in `setupapi.dev.log`.

---

### Custom Aero logs (scripts and post-install automation)

These paths are recommended because they are writable during setup and easy to collect:

* `C:\Windows\Temp\aero-setup.log` (SetupComplete + general automation)
* `C:\Windows\Temp\aero-driver-install.log` (driver-specific install output)

What they should prove (minimum):

* Which phase ran (SetupComplete vs first-logon vs scheduled task).
* Which media path was used (e.g. `%configsetroot%` value, or discovered drive letter).
* Output of key commands (`bcdedit`, `pnputil`, `schtasks`), including exit codes.

---

## 3) Debug actions during setup (Shift+F10)

At almost any Windows Setup screen, press:

* `Shift+F10` → opens `cmd.exe` (WinPE command prompt)

This is the fastest way to prove whether CD1 is visible and whether `%configsetroot%` exists.

### A. Enumerate disks/volumes and identify CD0 vs CD1

List volumes:

```bat
wmic logicaldisk get name,description,filesystem
```

Optional (include volume label for easier CD0/CD1 identification):

```bat
wmic logicaldisk get name,description,filesystem,volumename
```

Then inspect likely drive letters:

```bat
dir C:\
dir D:\
dir E:\
dir F:\
```

If you want to brute-force scan drive letters (interactive `cmd.exe`):

```bat
for %D in (C D E F G H I J K L M N O P Q R S T U V W X Y Z) do @echo ==== %D ==== & @dir %D:\ 2>nul
```

Notes:

* In WinPE, the Windows installer environment is usually `X:\`.
* The target OS partition may not be `C:` yet, depending on when you check.
* CD drives are commonly `D:`/`E:` but can vary.

### B. Confirm the Aero config ISO is present

Once you believe you found CD1, check for your marker file and `autounattend.xml`:

```bat
dir <CD1>:\AERO_CONFIG.MEDIA
dir <CD1>:\autounattend.xml
```

If you did not include a marker file, add one; it makes every debugging step easier.

### C. Print and interpret environment variables (especially `%configsetroot%`)

Dump environment:

```bat
set
```

Then specifically:

```bat
echo configsetroot=[%configsetroot%]
set configset
```

Interpretation:

* **Expected (if Windows Setup treats CD1 as a “configuration set” root):**
  * `%configsetroot%` is a non-empty path to the *configuration set root*.
  * Depending on how Setup handled the media, it may point to:
    * the original removable/optical media (for example `D:\` / `E:\`), **or**
    * a copied/staged location on a local disk.
  * Prove what it points to by running:

```bat
echo configsetroot=[%configsetroot%]
dir "%configsetroot%"
```

  * If you use a marker file (recommended), also test:

```bat
dir "%configsetroot%\AERO_CONFIG.MEDIA"
```
* **If empty:**
  * Windows Setup may still be using `autounattend.xml`, but it may **not** consider the media a configuration set.
  * In that case, `$OEM$` copy behavior may differ (see [Open questions](#5-explicit-notes-on-the-open-questions-config-set-config-iso-oem)).

### D. Check live setup logs before reboot

Open logs in Notepad (works in WinPE):

```bat
notepad X:\Windows\Panther\setupact.log
notepad X:\Windows\Panther\setuperr.log
```

Search within the log for:

* `unattend`
* `autounattend`
* `ConfigSet`
* `ConfigSetRoot`

This is the fastest way to prove which unattend file was selected and whether a config set was detected.

---

## 4) Failure modes + fixes

### `autounattend.xml` not picked up

Symptoms:

* Setup shows prompts (EULA, edition selection, disk selection) that should be automated.

What to check:

1. Confirm the file is exactly at the root of the intended media:
   * `\autounattend.xml` (root)
2. Confirm Windows Setup can read the media (WinPE `dir` works).
3. Confirm Setup logs mention the file:
   * Search `setupact.log`/`UnattendGC\setupact.log` for `autounattend`.

Common causes / fixes:

* Wrong filename (`unattend.xml` instead of `autounattend.xml` for removable-media discovery).
* Wrong placement (nested folder instead of root).
* Wrong media type / discovery behavior:
  * **Expected (but not guaranteed):** Windows Setup scans *some* removable/optical media for `autounattend.xml`, but the exact search order varies by Windows version and environment.
  * If you place `autounattend.xml` on **CD1** and Setup behaves as if it never saw it, verify in `X:\Windows\Panther\setupact.log` / `UnattendGC\setupact.log` whether Setup enumerated that drive for answer files.
  * If CD1 is not scanned in your environment, the practical fixes are:
    - Put `autounattend.xml` on a **USB** device image instead (often treated as removable media), or
    - Rebuild the Windows install ISO so `autounattend.xml` is on **CD0** (customized install media), or
    - Ensure your workflow does not depend on “CD1 answer file discovery” and instead uses a slipstream/patcher approach.
* The hypervisor attaches CD1 too late (attach before boot).
* Multiple unattend files present on multiple devices; Setup may pick an unexpected one.

---

### Driver paths not found / quoting issues

Symptoms:

* Your driver-install script logs “file not found” or `pnputil` fails to open the INF.
* `setupapi.dev.log` shows missing source files.

What to check:

* In WinPE and in the installed OS, confirm the actual drive letter and paths.
* Avoid spaces and quoting ambiguity in paths on the ISO.

Fix patterns:

* Prefer a layout like `\Drivers\<vendor>\<arch>\*.inf` with no spaces.
* Log the resolved path before invoking `pnputil`.

---

### Unsigned/test-signed driver install blocked

Symptoms:

* `pnputil` reports failure.
* Device Manager shows the device but refuses the driver.
* `setupapi.dev.log` contains signature enforcement failures.

What to check:

1. Confirm testsigning state:

```bat
bcdedit /enum {current}
```

2. Check `C:\Windows\inf\setupapi.dev.log` for `!!!` lines around your INF name.

Typical fixes:

* Ensure testsigning is enabled **and** the machine rebooted after setting it.
* If using a test certificate, import it into the correct stores (TrustedPublisher and/or Root) before installing drivers:

```bat
certutil -addstore TrustedPublisher C:\Aero\certs\aero-test.cer
certutil -addstore Root C:\Aero\certs\aero-root.cer
```

Notes:

* Exact certificate requirements depend on how the driver was signed.
* Don’t guess: the error reason will be in `setupapi.dev.log`.

---

### SetupComplete not running

Symptoms:

* No `C:\Windows\Temp\aero-setup.log`
* No scheduled task created
* No post-install behavior happened (testsigning unchanged, no drivers installed)

What to check:

1. Confirm the file exists where Windows expects it:

```bat
dir "%WINDIR%\Setup\Scripts\SetupComplete.cmd"
```

2. Confirm `$OEM$` content was copied (if you rely on `$OEM$`):
   * `$OEM$\$$\Setup\Scripts\SetupComplete.cmd` should become:
     * `C:\Windows\Setup\Scripts\SetupComplete.cmd`

3. Check Setup logs:
   * `C:\Windows\Panther\setupact.log`
   * `C:\Windows\Panther\setuperr.log`

Common root causes / fixes:

* `$OEM$` wasn’t processed from CD1 (see [open questions](#5-explicit-notes-on-the-open-questions-config-set-config-iso-oem)).
* The file path inside `$OEM$` is wrong (must be exactly `$$\Setup\Scripts\SetupComplete.cmd`).
* Script ran but failed immediately; make the script write a first-line log entry before doing anything else.

---

### Reboot loops / task never deleted

Symptoms:

* VM keeps rebooting or repeatedly runs the same post-install step.
* Scheduled task persists across reboots.

What to check:

* Your script must create a durable “done” marker (file or registry) and check it before doing work.
* Ensure the scheduled task deletes itself after success:

```bat
schtasks /delete /tn "<TaskName>" /f
```

Fix patterns:

* Use a marker file such as `C:\Aero\postinstall.done` or `C:\Windows\Temp\aero-postinstall.done`.
* Log every branch decision (“marker present: skipping”, “marker missing: running”).
* Log `schtasks` output and `%ERRORLEVEL%`.

---

## 5) Explicit notes on the open questions (config-set, config ISO, $OEM$)

This section is intentionally a **test plan**: it tells you exactly how to determine what Windows Setup actually did in your environment.

### Question A: Is `%configsetroot%` available when using a separate config ISO (CD1)?

**Expected (but not guaranteed):** if Windows Setup recognizes CD1 as a *configuration set*, it sets `%configsetroot%` during setup.

How to verify:

1. During setup, press `Shift+F10`.
2. Run:

```bat
echo configsetroot=[%configsetroot%]
```

3. Record the result and correlate with logs:

* `X:\Windows\Panther\setupact.log`
* `X:\Windows\Panther\setuperr.log`

Search for:

* `ConfigSet`
* `ConfigSetRoot`

Interpretation:

* If `%configsetroot%` is set, **confirm it actually contains your config payload**:

```bat
dir "%configsetroot%"
dir "%configsetroot%\AERO_CONFIG.MEDIA"
```

* If the marker exists there (or you can see your expected folders), you can use `%configsetroot%` as your primary reference to find drivers/payload during setup.
* If `%configsetroot%` is set but the marker is missing, it may be pointing at a copied/staged config-set location or to a different device than you expect. Don’t guess:
  * Use the drive-letter scan in [Debug actions](#3-debug-actions-during-setup-shiftf10) to locate the real CD1.
  * Optionally, after install (once you know which drive is the OS partition), search the system drive for your marker to discover where Setup copied the config set:

```bat
where /r C:\ AERO_CONFIG.MEDIA
rem If `where` is not available (some WinPE environments), use:
dir /s /b C:\AERO_CONFIG.MEDIA 2>nul
```

* If `%configsetroot%` is empty, do **not** rely on it; treat CD1 as “just another CD drive” and use drive-letter discovery (see [Fallback](#fallback-approach-if-configsetroot--oem--are-unreliable)).

---

### Question B: Does Windows Setup process/copy `$OEM$` when `$OEM$` lives on CD1?

**Expected (but not guaranteed):** if CD1 is a recognized configuration set, `$OEM$` should be processed similarly to `$OEM$` on the main install media.

How to verify:

1. Put an unmistakable file under `$OEM$` in your config ISO, for example:
   * `$OEM$\$1\Aero\oem-proof.txt` (should copy to `C:\Aero\oem-proof.txt`)
   * `$OEM$\$$\Setup\Scripts\SetupComplete.cmd` (should copy to `%WINDIR%\Setup\Scripts\SetupComplete.cmd`)
2. After install completes, verify:

```bat
dir C:\Aero\oem-proof.txt
dir "%WINDIR%\Setup\Scripts\SetupComplete.cmd"
```

Interpretation:

* If these files exist, `$OEM$` copy happened.
* If not, `$OEM$` was not processed from CD1 in your environment. In that case, your `SetupComplete.cmd` will not exist unless you deliver it some other way.

---

### Question C: Does Windows Setup use `autounattend.xml` from CD1 while *not* treating it as a config set?

This is a common “partial success” scenario: Setup finds `autounattend.xml` and applies many settings, but does not set `%configsetroot%` and does not copy `$OEM$`.

How to verify:

* Prompts are automated (so unattend parsing worked), but:
  * `%configsetroot%` is empty during setup, and/or
  * `$OEM$` outputs are missing after install.

Corroborate in logs:

* `C:\Windows\Panther\UnattendGC\setupact.log` should still show which unattend file was used.
* You may see evidence of unattend parsing without config set detection.

---

### Question D: Does Windows Setup scan a secondary CD/DVD (CD1) for `autounattend.xml`?

**Expected (but not guaranteed):** Windows Setup will pick up `\autounattend.xml` from some removable/optical media, but the exact search order (and whether it scans a “second CD”) can vary by environment.

How to verify (minimal experiment):

1. Ensure **CD0** is the stock Windows 7 ISO (no answer file).
2. Put `\autounattend.xml` only on **CD1** and attach it before boot.
3. Boot and observe:
   * If Setup is fully unattended, CD1 answer file discovery worked in that environment.
   * If Setup shows prompts (edition selection, disk selection, EULA), it likely did not scan CD1 for the answer file.
4. Corroborate in logs:
   * In WinPE (Shift+F10): `notepad X:\Windows\Panther\setupact.log`
   * Search for `autounattend.xml` and/or the device path it was loaded from.

Practical fixes if CD1 is not scanned:

* Put `autounattend.xml` on a **USB** device image (often treated as removable media and more consistently scanned), or
* Patch/rebuild the install ISO so `autounattend.xml` is on **CD0**, or
* Use a slipstream/media patcher flow so Setup does not depend on “secondary CD answer file discovery”.

### Fallback approach (if `%configsetroot%` / `$OEM$` are unreliable)

If you find that CD1 is not treated as a configuration set on your hypervisor/media, the robust approach is:

1. **Make CD1 self-identifying** via a marker file (root): `AERO_CONFIG.MEDIA`.
2. **Scan drive letters at runtime** to locate CD1.
3. **Copy payload to a stable local path** early, e.g. `C:\Aero\` (so later phases don’t depend on CD drive letters).

Example drive-discovery snippet (batch, suitable for `SetupComplete.cmd`):

```bat
setlocal enabledelayedexpansion

set "AERO_MEDIA="
for %%D in (C D E F G H I J K L M N O P Q R S T U V W X Y Z) do (
  if exist "%%D:\AERO_CONFIG.MEDIA" set "AERO_MEDIA=%%D:"
)

if not defined AERO_MEDIA (
  echo ERROR: Aero config media not found. >> C:\Windows\Temp\aero-setup.log
  exit /b 1
)

echo Found Aero media at %AERO_MEDIA% >> C:\Windows\Temp\aero-setup.log

rem Example: copy payload to C:\Aero
md C:\Aero 2>nul
xcopy "%AERO_MEDIA%\Payload" C:\Aero\ /E /I /H /Y >> C:\Windows\Temp\aero-setup.log 2>&1
```

Key idea:

* **Do not** build a flow that only works if `%configsetroot%` is set or only works if `$OEM$` is copied from CD1.
* Instead, design the automation to succeed with either:
  * config-set semantics (when available), or
  * simple “find the CD by marker and copy from it” semantics (always available when the CD is attached).

# AeroGPU (Win7) build guide (WDK 7.1)
This directory contains the build tooling for the **AeroGPU Windows 7 driver stack**:
* **KMD** (kernel-mode driver): produces `*.sys`
* **UMD** (user-mode driver): produces `*.dll`

The build scripts use the **WDK 7.1 “BUILD” system** (the classic `setenv` + `build` flow).

---

## Supported toolchain

### Host OS (where you build)
* **Recommended:** Windows 7 SP1 x64 (matches the target and avoids legacy installer issues)
* **Also commonly workable:** Windows 10 x64 (WDK 7.1 is old; you may need compatibility mode / admin install)

### WDK
* **Windows Driver Kit 7.1** (typically installs to `C:\WinDDK\7600.16385.1`)

### Visual Studio
* **Visual Studio 2010 SP1** (optional, but the most compatible IDE for WDK 7.1-era projects)
  * The scripts below do **not** require VS; they use the WDK command-line toolchain.

---

## Repo layout expected by the build scripts

These scripts assume the driver sources live at:
* `drivers/aerogpu/kmd/` (contains a WDK `sources` file, or a `dirs` file)
* `drivers/aerogpu/umd/` (contains a WDK `sources` file, or a `dirs` file)

If your KMD/UMD tasks use a different layout, update `build_all.cmd` accordingly.

---

## One-time setup

### 1) Install WDK 7.1
1. Install **WDK 7.1** from Microsoft’s installer/ISO.
2. Confirm the following exists:
   * `C:\WinDDK\7600.16385.1\bin\setenv.cmd` (or `setenv.bat`)

### 2) Set `WINDDK` (recommended)
The scripts will look for a `WINDDK` environment variable. Example (temporary, for the current shell):

```cmd
set WINDDK=C:\WinDDK\7600.16385.1
```

To persist it for future shells:

```cmd
setx WINDDK C:\WinDDK\7600.16385.1
```

---

## Building

### Build everything (x86 + x64, fre + chk)
From a regular `cmd.exe` prompt at the repo root:

```cmd
drivers\aerogpu\build\build_all.cmd
```

### Build only one variant / arch
`build_all.cmd` accepts optional arguments:

```cmd
:: Only free builds (fre) for both arches
drivers\aerogpu\build\build_all.cmd fre

:: Only x64 builds (both fre + chk)
drivers\aerogpu\build\build_all.cmd all x64

:: Only checked x86
drivers\aerogpu\build\build_all.cmd chk x86
```

### Output layout
Artifacts are copied into a deterministic tree under:

`drivers/aerogpu/build/out/`

Example:

```
drivers/aerogpu/build/out/
  win7/
    x86/
      fre/
        kmd/  (*.sys, matching *.pdb)
        umd/  (*.dll, matching *.pdb)
      chk/
        ...
    x64/
      fre/
        ...
      chk/
        ...
```

If the build succeeds, you should see:
* at least one `*.sys` under `.../kmd/`
* at least one `*.dll` under `.../umd/`

---

## Notes on the WDK BUILD system (for contributors)

### Minimal `dirs` example
If you want a single top-level build entrypoint under `drivers/aerogpu/`:

`drivers/aerogpu/dirs`
```make
DIRS= \
    kmd \
    umd
```

### Minimal `sources` example (KMD)
`drivers/aerogpu/kmd/sources`
```make
TARGETNAME=aerogpu_kmd
TARGETTYPE=DRIVER
DRIVERTYPE=WDM

SOURCES= \
    driver.c
```

### Minimal `sources` example (UMD DLL)
`drivers/aerogpu/umd/sources`
```make
TARGETNAME=aerogpu_umd
TARGETTYPE=DYNLINK
UMTYPE=windows

SOURCES= \
    umd.c
```

---

## Signing + installation (dev/test)

This repo’s packaging task is expected to provide scripts under `drivers/aerogpu/packaging/`
to:
1. create/use a test certificate
2. sign the built binaries + INF/CAT
3. install the driver package on a Win7 VM / test machine

Typical flow after a successful build:

```cmd
:: Example only — see drivers\aerogpu\packaging\README.md (once added)
drivers\aerogpu\packaging\sign.cmd drivers\aerogpu\build\out\win7\x64\fre
drivers\aerogpu\packaging\install.cmd drivers\aerogpu\build\out\win7\x64\fre
```

If you don’t have the packaging scripts yet, the manual equivalents are usually:
* `signtool.exe` (from Windows SDK) for signing
* `pnputil.exe -i -a <path-to-inf>` (on Windows 7) for installing an INF-based driver package


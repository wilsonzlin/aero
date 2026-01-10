# AeroGPU (Win7) build guide (WDK 7.1)
This directory contains the build tooling for the **AeroGPU Windows 7 driver stack**:
* **KMD** (kernel-mode driver): produces `*.sys`
* **UMD** (user-mode driver): produces `*.dll`

The build scripts use:
* **WDK 7.1 “BUILD” system** for the **KMD** (`setenv` + `build`)
* **MSBuild** for the **UMD** (Visual Studio solution)

---

## Supported toolchain

### Host OS (where you build)
* **Recommended:** Windows 10/11 x64 (easiest way to get a modern MSBuild toolchain)
* **Also workable:** Windows 7 SP1 x64 (WDK 7.1 installs cleanly, but modern VS/MSBuild may be harder)

### WDK
* **Windows Driver Kit 7.1** (typically installs to `C:\WinDDK\7600.16385.1`)

### Visual Studio
* **Visual Studio 2022** (or “Build Tools for Visual Studio 2022”) for the UMD MSBuild project
  * Required components: **MSBuild** + **Desktop development with C++**
  * The KMD build does **not** require VS; it uses the WDK command-line toolchain.

---

## Repo layout expected by the build scripts

These scripts assume the driver sources live at:
* `drivers/aerogpu/kmd/` (WDK BUILD project; contains `sources`)
* `drivers/aerogpu/umd/d3d10_11/` (MSBuild/VS solution: `aerogpu_d3d10_11.sln`)

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

`build_all.cmd` builds:
* KMD via WDK `build.exe`
* UMD via `msbuild.exe` (it will try `where msbuild`, then VS `vswhere` fallback)

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
TARGETNAME=aerogpu
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

Use the Win7 packaging scripts under:
* `drivers/aerogpu/packaging/win7/`

They handle:
1. enabling test-signing mode (optional)
2. creating/installing a test certificate
3. signing binaries + generating `.cat` files (if `inf2cat.exe` is available)
4. install/uninstall via `pnputil`

Typical flow after a successful build:

```cmd
:: 1) Copy build outputs into the packaging folder (same dir as the .inf files)
copy /y drivers\aerogpu\build\out\win7\x64\fre\kmd\*.sys drivers\aerogpu\packaging\win7\
copy /y drivers\aerogpu\build\out\win7\x86\fre\umd\*.dll drivers\aerogpu\packaging\win7\
copy /y drivers\aerogpu\build\out\win7\x64\fre\umd\*.dll drivers\aerogpu\packaging\win7\

:: 2) In a Win7 VM, run as Administrator:
cd drivers\aerogpu\packaging\win7
sign_test.cmd
install.cmd
```

See `drivers/aerogpu/packaging/win7/README.md` for details (including Hardware ID edits).

> Note: the Win7 INFs currently assume the D3D9 UMD (`aerogpu_d3d9*.dll`) exists.
> If you don’t have those binaries yet, `sign_test.cmd` may fail during `inf2cat`
> generation and `pnputil` install may fail if the `.cat` is missing. See the
> packaging README for the full expected file list.

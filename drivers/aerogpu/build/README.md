# AeroGPU (Win7) build guide (WDK 7.1)
This directory contains the build tooling for the **AeroGPU Windows 7 driver stack**:
* **KMD** (kernel-mode driver): produces `*.sys`
* **UMD** (user-mode driver): produces `*.dll`

The build scripts use:
* **WDK 7.1 “BUILD” system** for the **KMD** (`setenv` + `build`)
* **MSBuild** for the **UMDs** (Visual Studio projects)

---

## Supported toolchain

### Host OS (where you build)
* **Supported:** Windows 10/11 x64
  * WDK 7.1 is old, but this is the most practical host for the required MSBuild toolchain.
* **KMD-only note:** you *can* build just the kernel driver on Windows 7 SP1 x64 with WDK 7.1, but building the UMDs requires a newer MSBuild/Visual Studio toolchain.

### WDK
* **Windows Driver Kit 7.1** (typically installs to `C:\WinDDK\7600.16385.1`)

### Visual Studio
* **Visual Studio 2022** (or “Build Tools for Visual Studio 2022”) for the UMD MSBuild project
  * Required components: **MSBuild** + **Desktop development with C++**
  * The KMD build does **not** require VS; it uses the WDK command-line toolchain.
  * The UMD projects are configured to use the **static** MSVC runtime (`/MT`), so you typically do **not** need to install a VC++ Redistributable inside the Win7 VM to load the DLLs.

---

## Repo layout expected by the build scripts

These scripts assume the driver sources live at:
* `drivers/aerogpu/kmd/` (WDK BUILD project; contains `sources`)
* `drivers/aerogpu/umd/d3d9/` (MSBuild/VS project: `aerogpu_d3d9_umd.vcxproj`)
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
* UMDs via `msbuild.exe` (it will try `where msbuild`, then VS `vswhere` fallback)
  * D3D9 UMD is required
  * D3D10/11 UMD is optional (only built if its `.sln` exists)

> Note: on **Win7 x64**, the display driver package installs both 64-bit and 32-bit (SysWOW64) UMDs.
> That means you still need the **x86 UMD build** even if you only care about an x64 VM.

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

## Notes on the WDK BUILD system (KMD)

### Optional `dirs` file
The classic WDK BUILD system can chain subprojects via a `dirs` file. AeroGPU does **not** use this as the primary entrypoint (because the UMDs are MSBuild projects), but if you want a WDK-only build root you can add:

`drivers/aerogpu/dirs`
```make
DIRS= \
    kmd
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

## Notes on the MSBuild UMD projects

Current UMDs in-tree:
* D3D9: `drivers/aerogpu/umd/d3d9/aerogpu_d3d9_umd.vcxproj`
  * Outputs: `aerogpu_d3d9.dll` (x86) + `aerogpu_d3d9_x64.dll` (x64)
* D3D10/11 (optional): `drivers/aerogpu/umd/d3d10_11/aerogpu_d3d10_11.sln`
  * Outputs: `aerogpu_d3d10.dll` (x86) + `aerogpu_d3d10_x64.dll` (x64)

`build_all.cmd` maps:
* `fre` → `Release`
* `chk` → `Debug`

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
:: 1) Stage the packaging folder (copies the right-arch KMD + required UMDs)
drivers\aerogpu\build\stage_packaging_win7.cmd fre x64

:: 2) In a Win7 VM, run as Administrator:
cd drivers\aerogpu\packaging\win7
sign_test.cmd
install.cmd
```

See `drivers/aerogpu/packaging/win7/README.md` for details (including Hardware ID edits).

> Note: `aerogpu_dx11.inf` is optional; if you don’t build the D3D10/11 UMD, install with `aerogpu.inf`.

For a **Win7 x86** VM, stage with:

```cmd
drivers\aerogpu\build\stage_packaging_win7.cmd fre x86
```

## Validation (recommended)

After installing the driver in a Win7 VM, run the guest-side validation suite:

* `drivers/aerogpu/tests/win7/README.md`

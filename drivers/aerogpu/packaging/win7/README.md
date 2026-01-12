# AeroGPU Windows 7 Driver Package (WDDM 1.1)

This directory contains a **Windows 7 SP1** driver package skeleton for the AeroGPU WDDM stack:

- Kernel-mode miniport (`aerogpu.sys`)
- User-mode display drivers (UMDs)
  - **Required:** Direct3D 9 UMD
  - **Optional:** Direct3D 10/11 UMD

It also includes scripts for **test-signing** and **install/uninstall** in a Win7 VM.

## 0) CI packages vs manual packaging (DX11-capable vs DX9-only)

### CI-produced artifacts (out/packages / release ZIP)

CI stages the INF(s) and built binaries at the **package root** (e.g. `out/packages/aerogpu/x64/`), but includes this `packaging/win7/` folder as extra documentation + helper scripts.

The helper scripts (`install.cmd`, `sign_test.cmd`, etc.) auto-detect the package root when run from within this folder, so you do **not** need to copy/move the INF/binaries into `packaging/win7/` for CI-produced packages.

Packaging is controlled by `drivers/aerogpu/ci-package.json`. By default, CI packages are **DX11-capable**
so they can run the D3D10/D3D11 guest validation suite:

- Included:
  - `aerogpu_dx11.inf` (canonical HWID binding: `A3A0:0001`; includes D3D10/11 UMD registration)
  - `legacy/aerogpu.inf` (legacy HWID binding: `1AED:0001`; shipped under `legacy/` to avoid INF name collisions)
- Not included:
  - `aerogpu.inf` (D3D9-only variant; useful for bring-up/regression)
  - `legacy/aerogpu_dx11.inf` (optional legacy D3D10/11 UMD variant)

When invoked with no arguments, `packaging\\win7\\install.cmd` prefers `aerogpu_dx11.inf` when present at the
package root and falls back to `aerogpu.inf`.

### Choosing which INF to install (DX11 vs DX9-only)

- DX11-capable (recommended; includes D3D10/11 UMDs):

  ```bat
  :: CI-staged packages: installs aerogpu_dx11.inf when it is present at the package root.
  :: Manual packaging: also selects aerogpu_dx11.inf when the D3D10/11 UMDs are staged alongside it.
  install.cmd
  ```

  ```bat
  install.cmd aerogpu_dx11.inf
  ```

- D3D9-only (does not require the D3D10/11 UMDs, and clears any stale DX11 UMD registration):

  ```bat
  :: Only available if aerogpu.inf is present next to this script (manual packaging),
  :: or if it was explicitly staged into a CI package.
  install.cmd aerogpu.inf
  ```

## 1) Expected build outputs

Copy the built driver binaries into this directory (same folder as the `.inf` files):

> Tip: if you built via the repo-local wrapper `drivers\aerogpu\build\build_all.cmd` (MSBuild/WDK10; stages outputs under `drivers\aerogpu\build\out\win7\...`), you can stage this folder automatically:
>
> ```bat
> :: For a Win7 x64 VM (copies x64 aerogpu.sys + x86/x64 UMDs)
> drivers\aerogpu\build\stage_packaging_win7.cmd fre x64
>
> :: For a Win7 x86 VM (copies x86 aerogpu.sys + x86 UMDs)
> drivers\aerogpu\build\stage_packaging_win7.cmd fre x86
> ```
>
> If you built via the CI scripts, skip staging and instead copy/install the ready-to-install
> package under `out/packages/aerogpu/<arch>/` (see section 3). Note: CI outputs are **DX11-capable** by
> default; `install.cmd` (no args) will pick `aerogpu_dx11.inf` when available.

### Required (D3D9)

| File | Arch | Destination after install |
|------|------|---------------------------|
| `aerogpu.sys` | x86/x64 | `C:\Windows\System32\drivers\` |
| `aerogpu_d3d9.dll` | x86 | `C:\Windows\System32\` (x86 OS) / `C:\Windows\SysWOW64\` (x64 OS) |
| `aerogpu_d3d9_x64.dll` | x64 | `C:\Windows\System32\` (x64 OS) |

### Optional (D3D10/11)

Only needed if you install using `aerogpu_dx11.inf` or `legacy/aerogpu_dx11.inf`:

| File | Arch | Destination after install |
|------|------|---------------------------|
| `aerogpu_d3d10.dll` | x86 | `C:\Windows\System32\` (x86 OS) / `C:\Windows\SysWOW64\` (x64 OS) |
| `aerogpu_d3d10_x64.dll` | x64 | `C:\Windows\System32\` (x64 OS) |

### UMD registration contract (Win7 loader registry values)

On Windows 7 / WDDM 1.1 the Direct3D runtimes locate the display driver’s user-mode DLLs (UMDs) via registry values written by the display driver INF under the adapter’s device key (`HKR`).

> Naming convention:
>
> - D3D9 keys use **base names** (no `.dll` extension).
> - D3D10/11 keys use **file names** (include `.dll`).

| API | Registry value (under `HKR`) | Type | Meaning |
|-----|-------------------------------|------|---------|
| D3D9 / D3D9Ex | `InstalledDisplayDrivers` | `REG_MULTI_SZ` | Base-name(s) of the native-bitness D3D9 UMD(s). |
| D3D9 / D3D9Ex (x64 only) | `InstalledDisplayDriversWow` | `REG_MULTI_SZ` | Base-name(s) of the 32-bit D3D9 UMD(s) used by WOW64 apps. |
| D3D10/11 | `UserModeDriverName` | `REG_SZ` | Filename of the native-bitness D3D10/11 UMD. |
| D3D10/11 (x64 only) | `UserModeDriverNameWow` | `REG_SZ` | Filename of the 32-bit D3D10/11 UMD used by WOW64 apps. |
| (driver ranking) | `FeatureScore` | `REG_DWORD` | Driver rank (lower is better). |

**Concrete values written by the AeroGPU INFs:**

- `aerogpu.inf` (D3D9 only):
  - x86: `InstalledDisplayDrivers = ["aerogpu_d3d9"]`
  - x64: `InstalledDisplayDrivers = ["aerogpu_d3d9_x64"]`, `InstalledDisplayDriversWow = ["aerogpu_d3d9"]`
  - `FeatureScore = 0xF8`
- `aerogpu_dx11.inf` (D3D9 + D3D10/11):
  - x86: `UserModeDriverName = "aerogpu_d3d10.dll"`
  - x64: `UserModeDriverName = "aerogpu_d3d10_x64.dll"`, `UserModeDriverNameWow = "aerogpu_d3d10.dll"`
  - `FeatureScore = 0xF7` (**preferred** by Windows PnP auto-selection if both INFs match the same HWID)

To inspect what was actually written after install, you can query the adapter’s device key from an elevated Command Prompt:

```bat
:: Find the AeroGPU adapter instance key under the Display class (note the \0000 / \0001 path):
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}" /s /f "AeroGPU Display Adapter" /d

:: Then query values (replace <CLASSKEY> with the path printed above):
reg query "<CLASSKEY>" /v VideoID
reg query "<CLASSKEY>" /v InstalledDisplayDrivers
reg query "<CLASSKEY>" /v InstalledDisplayDriversWow
reg query "<CLASSKEY>" /v UserModeDriverName
reg query "<CLASSKEY>" /v UserModeDriverNameWow
```

On some systems, the UMD registration values are surfaced under the `Control\Video` key for the adapter. If `InstalledDisplayDrivers` / `UserModeDriverName` aren’t present under `<CLASSKEY>`, use the `VideoID` value to locate:

```bat
:: Replace <VIDEOID> with the GUID printed by the VideoID query above (including braces):
set VIDEOID=<VIDEOID>
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Video\%VIDEOID%\0000" /v InstalledDisplayDrivers
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Video\%VIDEOID%\0000" /v InstalledDisplayDriversWow
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Video\%VIDEOID%\0000" /v UserModeDriverName
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Video\%VIDEOID%\0000" /v UserModeDriverNameWow
```

`packaging/win7/verify_umd_registration.cmd` performs this lookup automatically and prints the resolved registry key it is using.

## 2) Confirm the PCI Hardware ID(s) (required)

This Win7 driver package supports the following AeroGPU PCI Hardware IDs:

```
PCI\VEN_A3A0&DEV_0001  (canonical / current, versioned ABI / "AGPU")
PCI\VEN_1AED&DEV_0001  (legacy bring-up ABI)
```

`A3A0:0001` is the canonical ABI identity; `1AED:0001` is the deprecated legacy bring-up ABI and may still appear
depending on the emulator/device model (the emulator legacy device model is behind feature `emulator/aerogpu-legacy`).

The Win7 KMD still has a compatibility path for the legacy bring-up device model, but the primary INFs in this
directory (`aerogpu.inf` and `aerogpu_dx11.inf`) intentionally match only the canonical `A3A0:0001` device model (to
discourage accidental installs against the legacy device model). If you need the legacy `1AED:0001` device model for
bring-up/compatibility, install using the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/` and build the
emulator with the legacy device model enabled (feature `emulator/aerogpu-legacy`).

Note: the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/` are kept in the repo source tree for
compatibility/regression. CI-staged packages include a copy at `legacy/aerogpu.inf` so Guest Tools can support
emulator builds that still expose the deprecated legacy device model.

See `docs/abi/aerogpu-pci-identity.md` for the full context and the matching emulator device models. The Win7 KMD
supports multiple ABIs and auto-detects which one is active based on MMIO magic; see `drivers/aerogpu/kmd/README.md`.

Before installing, confirm your VM's device model reports one of the above Hardware IDs:

1. In the Win7 VM: Device Manager → Display adapters (or unknown device) → Properties → Details → *Hardware Ids*
2. Copy the `PCI\VEN_....&DEV_....` value.
3. If it differs, update the INF(s) in the `[AeroGPU_Models.*]` sections.

## 3) Prerequisites (test signing tools)

### Recommended: sign on the build host (no WDK tools needed in the Win7 VM)

The simplest workflow is to generate and sign the driver package on a **Windows 10/11 build host** using the repo CI scripts, then copy the signed package into the Windows 7 VM:

```powershell
pwsh ci/install-wdk.ps1
pwsh ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers aerogpu
pwsh ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
```

This produces:

- Signed packages under:
  - `out/packages/aerogpu/x86/` (Windows 7 x86)
  - `out/packages/aerogpu/x64/` (Windows 7 x64)
- The signing certificate at:
  - `out/certs/aero-test.cer`

By default, these CI-staged packages are **DX11-capable** and include:

- `aerogpu_dx11.inf` at the package root (canonical `PCI\VEN_A3A0&DEV_0001`)
- `legacy/aerogpu.inf` for the deprecated legacy bring-up device model (`PCI\VEN_1AED&DEV_0001`)

The optional D3D9-only INF (`aerogpu.inf`) and the legacy DX11 INF (`legacy/aerogpu_dx11.inf`) are not included unless you customize
`drivers/aerogpu/ci-package.json` (see section 0).

See **4.1 Host-signed package** for the Win7 VM install steps.

### Optional: sign inside the Win7 VM with `sign_test.cmd`

To run `sign_test.cmd` inside the Windows 7 VM, you need tooling from a Windows SDK/WDK available on `PATH`:

- `makecert.exe`
- `signtool.exe`
- `inf2cat.exe` (recommended; required if the INF declares `CatalogFile=...`)

The **Windows 7 WDK (7600)** includes all of these tools and is often the most straightforward option for signing inside a Win7 VM.
Newer Windows SDK/WDK releases (Windows Kits 10+) generally include `signtool.exe`/`inf2cat.exe` (CI uses a pinned Windows Kits 10 toolchain), but may not include the deprecated `makecert.exe`; if it’s missing, prefer the host-signed CI workflow above.

## 4) Install (Windows 7 SP1 VM)

### 4.1 Host-signed package (recommended)

1. Copy the signed package directory into the Win7 VM (for example):

- Windows 7 x86: `out/packages/aerogpu/x86/`
- Windows 7 x64: `out/packages/aerogpu/x64/`

2. Copy the signing certificate into the VM **next to the INF** (package root):

- `out/certs/aero-test.cer` → `C:\path\to\out\packages\aerogpu\x64\aero-test.cer`

3. In the Win7 VM, open an **elevated** Command Prompt (Run as Administrator) and trust the certificate (and enable test signing if needed):

```bat
cd C:\path\to\out\packages\aerogpu\x64
packaging\win7\trust_test_cert.cmd
shutdown /r /t 0
```

> Tip: If you omit the cert argument, `trust_test_cert.cmd` searches for `aero-test.cer` in parent directories (useful for bundle/ISO layouts).

4. After reboot, install the **signed** package by pointing at the INF in the copied package directory:

```bat
cd C:\path\to\out\packages\aerogpu\x64
:: DX11-capable (recommended; required for D3D10/D3D11 validation):
pnputil -i -a aerogpu_dx11.inf
:: D3D9-only variant (if staged):
pnputil -i -a aerogpu.inf
:: legacy bring-up device model:
pnputil -i -a legacy\aerogpu.inf
:: legacy bring-up device model (DX11-capable variant; if staged):
pnputil -i -a legacy\aerogpu_dx11.inf
:: or (use the helper script shipped in the package):
packaging\win7\install.cmd
:: legacy bring-up variants:
packaging\win7\install.cmd legacy\aerogpu.inf
packaging\win7\install.cmd legacy\aerogpu_dx11.inf
:: install.cmd also runs packaging\win7\verify_umd_registration.cmd to sanity-check UMD placement + registry values.
```

Note: CI packages include `aerogpu_dx11.inf` (DX11-capable) by default. `packaging\\win7\\install.cmd` (no args)
prefers `aerogpu_dx11.inf` when present at the package root (and falls back to `aerogpu.inf` when it is staged).
See section 0.

### 4.2 Sign inside the Win7 VM (optional)

1. Copy this directory into the VM and ensure it contains the built binaries next to the INFs (see **1) Expected build outputs** above).

> Tip: after running `drivers\aerogpu\build\build_all.cmd`, you can stage this directory on the build host via:
>
> ```bat
> drivers\aerogpu\build\stage_packaging_win7.cmd fre x64
> ```

2. In the Win7 VM, run (as Administrator):

```bat
sign_test.cmd
shutdown /r /t 0
```

3. After reboot, install (as Administrator):

```bat
install.cmd
```

Notes:

- `install.cmd` uses `pnputil` by default.
- `install.cmd` runs `verify_umd_registration.cmd` after install and returns non-zero if required UMD files/registry values are missing.
- If you have `devcon.exe` available, you can place it in this directory (next to `install.cmd`) and the script will use it as a fallback for device update if `pnputil` fails.

To install the DX11-capable variant explicitly:

```bat
install.cmd aerogpu_dx11.inf
```

Reboot if the display driver update does not fully apply immediately.

## 5) Uninstall

Run as Administrator:

```bat
uninstall.cmd
```

If `pnputil -d` reports the driver is in use, reboot into **Safe Mode**, or switch the display adapter back to a different driver first, then rerun `uninstall.cmd`.

## 6) Verify it worked

On a clean Win7 SP1 VM:

1. Device Manager → Display adapters shows **AeroGPU Display Adapter** (no yellow bang).
2. No **Code 52** (signature), **Code 39**, or **Code 43**.
3. Run the quick verification script (prints file placement + registry values and returns non-zero on failure):

   ```bat
   cd C:\path\to\out\packages\aerogpu\x64
   packaging\win7\verify_umd_registration.cmd
   ```

   The script also validates D3D10/11 UMD registration if it detects `UserModeDriverName` is present.
   If you installed the DX11-capable package (`aerogpu_dx11.inf` or `legacy/aerogpu_dx11.inf`), you can additionally force the D3D10/11 checks:

   ```bat
   packaging\win7\verify_umd_registration.cmd dx11
   ```

   > Note: on Win7 x64, `verify_umd_registration.cmd` automatically uses the `Sysnative` path when run from a 32-bit Command Prompt, so it can correctly check the real `System32\` directory (not the WOW64-redirected view).

4. Confirm UMD DLL placement:
    - x64 VM:
      - `C:\Windows\System32\aerogpu_d3d9_x64.dll` exists
      - `C:\Windows\SysWOW64\aerogpu_d3d9.dll` exists
      - (if you installed a DX11-capable INF) `C:\Windows\System32\aerogpu_d3d10_x64.dll` exists and `C:\Windows\SysWOW64\aerogpu_d3d10.dll` exists
    - x86 VM:
      - `C:\Windows\System32\aerogpu_d3d9.dll` exists
      - (if you installed a DX11-capable INF) `C:\Windows\System32\aerogpu_d3d10.dll` exists

## 6.1) Confirm which UMD DLL actually loaded (32-bit vs 64-bit)

On Win7 x64 it’s possible for a test to “work” while the wrong UMD is being used (e.g. fallback to Microsoft / WARP).

For a DX11-capable INF (`aerogpu_dx11.inf` or `legacy/aerogpu_dx11.inf`), the expected install on x64 is:

- Native x64 processes load: `C:\Windows\System32\aerogpu_d3d10_x64.dll` (`UserModeDriverName`)
- WOW64 (32-bit) processes load: `C:\Windows\SysWOW64\aerogpu_d3d10.dll` (`UserModeDriverNameWow`)

### 6.1.1) Quick registry check (no extra tools)

You can sanity-check the **UMD registry values** written by the INF using `reg query`:

```bat
:: D3D9 (x64 key + WOW64 key)
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}" /s /v InstalledDisplayDrivers
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}" /s /v InstalledDisplayDriversWow

:: D3D10/11 (x64 key + WOW64 key; requires a DX11-capable INF)
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}" /s /v UserModeDriverName
reg query "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}" /s /v UserModeDriverNameWow
```

Expected values (from `aerogpu_dx11.inf` / `legacy/aerogpu_dx11.inf`):

- `UserModeDriverName = aerogpu_d3d10_x64.dll` (x64 only)
- `UserModeDriverNameWow = aerogpu_d3d10.dll` (x64 only)

### 6.1.2) Confirm the real DLL loaded in-process

To confirm the **real** AeroGPU UMD loaded for a given process bitness:

1. Run the relevant test executable:
   - D3D9: `drivers\aerogpu\tests\win7\bin\d3d9ex_triangle.exe` (build/run both x86 and x64)
   - D3D11: `drivers\aerogpu\tests\win7\bin\d3d11_triangle.exe` (build/run both x86 and x64; requires a DX11-capable INF)
2. Confirm the loaded module using one of:
   - **Process Explorer**: select the process → View → *Lower Pane View* → *DLLs*, then look for:
     - D3D9: `aerogpu_d3d9.dll` (x86) or `aerogpu_d3d9_x64.dll` (x64)
     - D3D10/11: `aerogpu_d3d10.dll` (x86) or `aerogpu_d3d10_x64.dll` (x64)
     - **DebugView**: capture `OutputDebugString` output from the UMDs.
       - D3D9 logs `aerogpu-d3d9: module_path=...` and `aerogpu-d3d9: OpenAdapter...`
       - D3D10/11 logs `aerogpu-d3d10_11: module_path=...` and `aerogpu-d3d10_11: OpenAdapter.. ...`
        - Optional: set `AEROGPU_D3D10_11_LOG=1` before launching the app to enable verbose `AEROGPU_D3D11DDI:` call traces. (The `module_path=...` line is emitted once per process.)
3. (Optional) After each run, confirm fences advance:
    - `drivers\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --query-fence`

## 6.2) Optional: run the AeroGPU debug/control tool (dbgctl)

For bring-up and debugging, you can use the Escape-based dbgctl tool:

- Tool: `drivers/aerogpu/tools/win7_dbgctl/`
- Docs/build: `drivers/aerogpu/tools/win7_dbgctl/README.md`

If `drivers\aerogpu\build\stage_packaging_win7.cmd` finds an already-built dbgctl
binary at `drivers\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`, it will copy
it into this packaging directory as `aerogpu_dbgctl.exe`.

Examples:

```bat
:: (Build the tool if needed; this does not require the WDK)
cd drivers\aerogpu\tools\win7_dbgctl
build_vs2010.cmd

 :: Run it
 bin\aerogpu_dbgctl.exe --query-version
 bin\aerogpu_dbgctl.exe --query-umd-private
 bin\aerogpu_dbgctl.exe --query-fence
 bin\aerogpu_dbgctl.exe --dump-ring --ring-id 0
 bin\aerogpu_dbgctl.exe --dump-vblank --vblank-samples 10 --vblank-interval-ms 200
 bin\aerogpu_dbgctl.exe --wait-vblank --vblank-samples 120 --timeout-ms 2000
bin\aerogpu_dbgctl.exe --query-scanline --vblank-samples 50 --vblank-interval-ms 10
bin\aerogpu_dbgctl.exe --selftest
```

## 7) Run the guest-side Direct3D validation suite (recommended)

After installation, run the small guest-side Direct3D tests under:

* `drivers/aerogpu/tests/win7/`

These programs render a known pattern and validate GPU readback (`PASS:`/`FAIL:` + non-zero exit code on failure). The suite includes a `run_all.cmd` harness.

The rendering tests also validate that the expected AeroGPU **user-mode display driver (UMD)** DLL
is actually loaded in-process (e.g. `aerogpu_d3d9.dll` vs `aerogpu_d3d9_x64.dll`, and the WOW64
variants on x64). This helps catch common INF/registry issues where rendering appears to work but
the runtime fell back to Microsoft Basic Render Driver / WARP, or where x64 works but WOW64 UMD
registration is broken (`InstalledDisplayDriversWow` / `UserModeDriverNameWow`).

Example:

```bat
cd \path\to\repo\drivers\aerogpu\tests\win7
build_all_vs2010.cmd
:: Choose the VID/DID that matches your VM's Hardware Ids:
run_all.cmd --require-vid=0xA3A0 --require-did=0x0001
:: If using the deprecated legacy device model, pass the matching VID/DID (see docs/abi/aerogpu-pci-identity.md).
:: Note: legacy bring-up requires the legacy INFs under drivers/aerogpu/packaging/win7/legacy/ and enabling the
:: emulator legacy device model (feature emulator/aerogpu-legacy).
```

Use the VID/DID shown in Device Manager → Display adapters → Properties → Details → **Hardware Ids** (or the HW ID used in the `[AeroGPU_Models.*]` sections of the INF).

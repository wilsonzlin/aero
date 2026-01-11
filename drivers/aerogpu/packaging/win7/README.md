# AeroGPU Windows 7 Driver Package (WDDM 1.1)

This directory contains a **Windows 7 SP1** driver package skeleton for the AeroGPU WDDM stack:

- Kernel-mode miniport (`aerogpu.sys`)
- User-mode display drivers (UMDs)
  - **Required:** Direct3D 9 UMD
  - **Optional:** Direct3D 10/11 UMD

It also includes scripts for **test-signing** and **install/uninstall** in a Win7 VM.

## CI-produced artifacts (out/packages / release ZIP)

CI stages the INF(s) and built binaries at the **package root** (e.g. `out/packages/aerogpu/x64/`), but includes this `packaging/win7/` folder as extra documentation + helper scripts.

The helper scripts (`install.cmd`, `sign_test.cmd`, etc.) auto-detect the package root when run from within this folder, so you do **not** need to copy/move the INF/binaries into `packaging/win7/` for CI-produced packages.

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
> If you built via the CI scripts, skip staging and instead copy/install the ready-to-install package under `out/packages/aerogpu/<arch>/` (see section 3).

### Required (D3D9)

| File | Arch | Destination after install |
|------|------|---------------------------|
| `aerogpu.sys` | x86/x64 | `C:\Windows\System32\drivers\` |
| `aerogpu_d3d9.dll` | x86 | `C:\Windows\System32\` (x86 OS) / `C:\Windows\SysWOW64\` (x64 OS) |
| `aerogpu_d3d9_x64.dll` | x64 | `C:\Windows\System32\` (x64 OS) |

### Optional (D3D10/11)

Only needed if you install using `aerogpu_dx11.inf`:

| File | Arch | Destination after install |
|------|------|---------------------------|
| `aerogpu_d3d10.dll` | x86 | `C:\Windows\System32\` (x86 OS) / `C:\Windows\SysWOW64\` (x64 OS) |
| `aerogpu_d3d10_x64.dll` | x64 | `C:\Windows\System32\` (x64 OS) |

## 2) Confirm the PCI Hardware ID(s) (required)

By default, both `aerogpu.inf` and `aerogpu_dx11.inf` bind to the AeroGPU PCI Hardware IDs:

```
PCI\VEN_A3A0&DEV_0001  (canonical / current)
PCI\VEN_1AED&DEV_0001  (legacy)
```

These correspond to the new (versioned) and legacy (bring-up) ABIs; see `docs/abi/aerogpu-pci-identity.md` for the full context and the matching emulator device models. The Win7 KMD supports both ABIs and auto-detects which one is active based on MMIO magic; see `drivers/aerogpu/kmd/README.md`.

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

See **4.1 Host-signed package** for the Win7 VM install steps.

### Optional: sign inside the Win7 VM with `sign_test.cmd`

To run `sign_test.cmd` inside the Windows 7 VM, you need tooling from a Windows SDK/WDK available on `PATH`:

- `makecert.exe`
- `signtool.exe`
- `inf2cat.exe` (recommended; required if the INF declares `CatalogFile=...`)

The **Windows 7 WDK (7600)** includes all of these tools and is often the most straightforward option for Win7 VMs.
Newer Windows SDK/WDK releases (Windows Kits 10+) also include them (CI uses a pinned Windows Kits 10 toolchain).

## 4) Install (Windows 7 SP1 VM)

### 4.1 Host-signed package (recommended)

1. Copy the signed package directory into the Win7 VM (for example):

- Windows 7 x86: `out/packages/aerogpu/x86/`
- Windows 7 x64: `out/packages/aerogpu/x64/`

2. Copy the signing certificate into the VM:

- `out/certs/aero-test.cer`

3. In the Win7 VM, open an **elevated** Command Prompt (Run as Administrator) and trust the certificate (and enable test signing if needed):

```bat
trust_test_cert.cmd aero-test.cer
shutdown /r /t 0
```

> Tip: `trust_test_cert.cmd` lives in this directory. Copy it into the VM next to `aero-test.cer` (or pass an explicit cert path).

4. After reboot, install the **signed** package by pointing at the INF in the copied package directory:

```bat
pnputil -i -a C:\path\to\out\packages\aerogpu\x64\aerogpu.inf
:: or (if you copied this packaging folder too):
install.cmd C:\path\to\out\packages\aerogpu\x64\aerogpu.inf
```

If installing the optional D3D10/11 UMD variant, install `aerogpu_dx11.inf` from the copied package directory instead.

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
- If you have `devcon.exe` available, you can place it in this directory (next to `install.cmd`) and the script will use it as a fallback for device update if `pnputil` fails.

To install with the optional D3D10/11 UMDs:

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
3. Confirm UMD DLL placement:
    - x64 VM:
      - `C:\Windows\System32\aerogpu_d3d9_x64.dll` exists
      - `C:\Windows\SysWOW64\aerogpu_d3d9.dll` exists
      - (if you installed `aerogpu_dx11.inf`) `C:\Windows\System32\aerogpu_d3d10_x64.dll` exists and `C:\Windows\SysWOW64\aerogpu_d3d10.dll` exists
    - x86 VM:
      - `C:\Windows\System32\aerogpu_d3d9.dll` exists
      - (if you installed `aerogpu_dx11.inf`) `C:\Windows\System32\aerogpu_d3d10.dll` exists

## 6.1) Optional: run the AeroGPU debug/control tool (dbgctl)

For bring-up and debugging, you can use the Escape-based dbgctl tool:

- Tool: `drivers/aerogpu/tools/win7_dbgctl/`
- Docs/build: `drivers/aerogpu/tools/win7_dbgctl/README.md`

If `drivers\\aerogpu\\build\\stage_packaging_win7.cmd` finds an already-built dbgctl
binary at `drivers\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`, it will copy
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
:: (legacy)
run_all.cmd --require-vid=0x1AED --require-did=0x0001
```

Use the VID/DID shown in Device Manager → Display adapters → Properties → Details → **Hardware Ids** (or the HW ID used in the `[AeroGPU_Models.*]` sections of the INF).

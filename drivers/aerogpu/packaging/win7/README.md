# AeroGPU Windows 7 Driver Package (WDDM 1.1)

This directory contains a **Windows 7 SP1** driver package skeleton for the AeroGPU WDDM stack:

- Kernel-mode miniport (`aerogpu.sys`)
- User-mode display drivers (UMDs)
  - **Required:** Direct3D 9 UMD
  - **Optional:** Direct3D 10/11 UMD

It also includes scripts for **test-signing** and **install/uninstall** in a Win7 VM.

## 1) Expected build outputs

Copy the built driver binaries into this directory (same folder as the `.inf` files):

> Tip: if you built via `drivers\aerogpu\build\build_all.cmd`, you can stage this folder automatically:
>
> ```bat
> :: For a Win7 x64 VM (copies x64 aerogpu.sys + x86/x64 UMDs)
> drivers\aerogpu\build\stage_packaging_win7.cmd fre x64
>
> :: For a Win7 x86 VM (copies x86 aerogpu.sys + x86 UMDs)
> drivers\aerogpu\build\stage_packaging_win7.cmd fre x86
> ```

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
| `aerogpu_d3d10.dll` | x86 | `C:\Windows\SysWOW64\` (x64 OS) |
| `aerogpu_d3d10_x64.dll` | x64 | `C:\Windows\System32\` (x64 OS) |

## 2) Set the correct PCI Hardware ID (required)

Both `aerogpu.inf` and `aerogpu_dx11.inf` currently contain a **placeholder** Hardware ID:

```
PCI\VEN_1234&DEV_1111
```

Before installing, replace it with the **actual AeroGPU PCI HW ID** exposed by the virtual device model, for example:

1. In the Win7 VM: Device Manager → Display adapters (or unknown device) → Properties → Details → *Hardware Ids*
2. Copy the `PCI\VEN_....&DEV_....` value.
3. Paste it into the INF(s) in the `[AeroGPU_Models.*]` sections.

## 3) Prerequisites (test signing tools)

To run `sign_test.cmd` you need tooling from a Windows SDK/WDK:

- `makecert.exe`
- `signtool.exe`
- `inf2cat.exe` (recommended; required if the INF declares `CatalogFile=...`)

The **Windows 7 WDK (7600)** includes all of these tools and is the most straightforward option for Win7 VMs.

## 4) Test-signing + install steps (Win7 SP1 VM)

### 4.1 Enable test signing + sign the package

1. Boot the Win7 VM.
2. Copy this folder into the VM (including the built `.sys`/`.dll` files).
3. Open an **elevated** Command Prompt (Run as Administrator).
4. Run:

```bat
sign_test.cmd
```

This will:

- `bcdedit /set testsigning on`
- Create a self-signed code signing cert (`aerogpu_test.cer` / `aerogpu_test.pfx`)
- Add it to **Trusted Root** and **Trusted Publishers**
- Generate `.cat` files via `inf2cat` (if available)
- Sign `*.sys`, `*.dll`, and `*.cat`

5. **Reboot** the VM (required after enabling test signing).

### 4.2 Install

After reboot, run (as Administrator):

```bat
install.cmd
```

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
   - x86 VM:
     - `C:\Windows\System32\aerogpu_d3d9.dll` exists

## 7) Run the guest-side Direct3D validation suite (recommended)

After installation, run the small guest-side Direct3D tests under:

* `drivers/aerogpu/tests/win7/`

These programs render a known pattern and validate GPU readback (`PASS:`/`FAIL:` + non-zero exit code on failure). The suite includes a `run_all.cmd` harness.

Example:

```bat
cd \path\to\repo\drivers\aerogpu\tests\win7
build_all_vs2010.cmd
run_all.cmd --require-vid=0x1234 --require-did=0x1111
```

Replace the VID/DID with the value shown in Device Manager → Display adapters → Properties → Details → **Hardware Ids**, or the HW ID used in the `[AeroGPU_Models.*]` sections of the INF.

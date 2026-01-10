# Windows 7 guest driver stack (virtio + GPU path)

This directory contains the **Windows 7 guest driver** workflow for Aero: build tooling, packaging, and installation/injection steps for the **virtio performance drivers** required by the emulator.

## What we ship today

To get a Windows 7 guest using high-performance virtio paths quickly and repeatably, Aero currently **packages drivers from the upstream virtio-win distribution** into an “Aero driver pack” ZIP.

The driver pack contains (Win7 x86 + amd64):

| Aero device | Upstream driver | Notes |
|---|---|---|
| `virtio-blk` | `viostor` | Storage (critical for install + performance). |
| `virtio-net` | `NetKVM` | Network. |
| `virtio-snd` | `viosnd` | Audio. |
| `virtio-input` | `vioinput` | Keyboard/mouse (HID). |

This repo **does not commit** `.sys` binaries. Instead, we provide scripts that create a reproducible driver pack from a pinned virtio-win ISO.

## Quickstart: build an Aero driver pack ZIP

1. Download a **virtio-win ISO** (stable) on a Windows build machine (Windows 10/11 recommended).
   - Example: `virtio-win.iso` from the virtio-win project’s “stable-virtio” direct downloads.

2. Run:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-driver-pack.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso
```

Output:

- `drivers\out\aero-win7-driver-pack\` (staging dir)
- `drivers\out\aero-win7-driver-pack.zip` (what the web UI/injector consumes)

## In-guest install workflow (post-install)

1. Copy `aero-win7-driver-pack.zip` into the Win7 guest.
2. Extract it.
3. Run as Administrator:

```bat
install.cmd
```

This uses `pnputil` to add the correct-architecture Win7 driver INFs.

## Offline injection workflow (slipstream into install media)

If you want Windows Setup to see virtio storage/network during install, inject drivers into the WIMs:

1. Build the driver pack and extract it to a folder (or use the staging folder).
2. Ensure you have the **Aero test signing certificate** (`.cer`) that was used to sign the driver catalogs.
   - This repo’s signing pipeline outputs it as: `out/certs/aero-test.cer`
3. Inject into `boot.wim` (setup environment) and `install.wim` (the OS image). The injector also installs the certificate into the offline stores (`ROOT` + `TrustedPublisher`) so both WinPE and the installed OS trust the drivers:

```powershell
# Storage/network available during Windows Setup:
.\drivers\scripts\inject-win7-wim.ps1 -WimFile D:\iso\sources\boot.wim -Index 2 -DriverPackRoot .\drivers\out\aero-win7-driver-pack -CertPath .\out\certs\aero-test.cer

# Inject into the installed OS image (repeat for each index/edition you care about):
.\drivers\scripts\inject-win7-wim.ps1 -WimFile D:\iso\sources\install.wim -Index 1 -DriverPackRoot .\drivers\out\aero-win7-driver-pack -CertPath .\out\certs\aero-test.cer
```

Rebuild the ISO after injection (outside the scope of this repo; use your preferred `oscdimg`/ISO tool).

## Driver signing / test mode

For custom Aero drivers (e.g. the optional GPU path), Windows 7 will require either:

- properly signed drivers, or
- **test signing** enabled in the guest.

See: `drivers/docs/signing-win7.md`.

## WDK build environment (for source builds)

See: `drivers/docs/wdk-build.md`.

## Basic validation plan (in-guest)

- **Device Manager**
  - Verify devices bind to `viostor`, `NetKVM`, `viosnd`, `vioinput`.
- **Storage throughput**
  - `winsat disk -seq -read` and `winsat disk -seq -write`
  - Large file copy in/out of the guest.
- **Network**
  - `ipconfig /all` shows link + DHCP lease
  - `ping` gateway and `iperf3` to a host endpoint (if available)
- **Audio**
  - Play a WAV in Windows Media Player, verify output device exists.
- **Input**
  - Verify keyboard/mouse are responsive without PS/2 fallback.

## Host-side protocol tests (shared structs)

`drivers/protocol/virtio/` contains `#[repr(C)]` virtio protocol structs intended to be shared with the emulator implementation.

Run:

```bash
cargo test --manifest-path drivers/protocol/virtio/Cargo.toml
```

## Optional: custom GPU path (WDDM)

Design notes live in `drivers/docs/gpu-path.md`. This is the long-term “fast path” for DirectX command interception/translation, but it is not required for the initial virtio bring-up.

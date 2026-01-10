# WDK build environment (Windows 7 drivers)

This repo focuses on a repeatable **driver pack** workflow. Building or modifying the underlying drivers requires a Windows driver build environment.

## Recommended approach: Enterprise WDK (EWDK)

The Enterprise WDK provides a self-contained, scriptable build environment (MSBuild + WDK tooling) that can be pinned to a specific version for reproducibility.

High-level steps:

1. Download a specific EWDK version and keep it pinned (don’t “latest”).
2. Run `LaunchBuildEnv.cmd` from the EWDK to open a build shell.
3. Build driver solutions from that shell using `msbuild`.

In-tree Aero driver projects should live under `drivers/wdk/` and can be built via:

```powershell
.\drivers\wdk\build.ps1
```

## Windows 7 targeting notes

Windows 7 is WDDM 1.1 and uses older kernel/driver ABI expectations. When building drivers intended to run on Win7:

- Ensure your project targets **Windows 7** (not Windows 10-only APIs).
- Prefer driver models that match the device class:
  - Storage: StorPort miniport (common for virtio-blk)
  - Networking: NDIS miniport (virtio-net)
  - Input: HID + KMDF (virtio-input)
  - Audio: PortCls/WaveRT + (often) KMDF helpers (virtio-snd)
- If you ship KMDF-based drivers, include the appropriate **WDF coinstaller** for Win7 (or require it preinstalled).

## virtio-win source builds

The upstream virtio-win drivers (viostor/NetKVM/viosnd/vioinput) are the practical baseline for Win7 virtio support.

If you need to modify/rebuild them:

1. Clone the upstream driver sources.
2. Build using the EWDK/WDK toolchain the project expects.
3. Re-run `drivers/scripts/make-driver-pack.ps1` pointing at the built output (or adjust the script to pack from your build tree instead of the ISO).

This repo intentionally keeps the build steps scripted and the output packaged into a single ZIP so the emulator can treat it as an artifact.

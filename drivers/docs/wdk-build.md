# WDK build environment (Windows 7 drivers)

This repo focuses on a repeatable **driver pack** workflow. Building or modifying the underlying drivers requires a Windows driver build environment.

## Recommended approach: Enterprise WDK (EWDK)

The Enterprise WDK provides a self-contained, scriptable build environment (MSBuild + WDK tooling) that can be pinned to a specific version for reproducibility.

High-level steps:

1. Download a specific EWDK version and keep it pinned (don’t “latest”).
2. Run `LaunchBuildEnv.cmd` from the EWDK to open a build shell.
3. Build driver solutions from that shell using `msbuild`.

In-tree Aero driver projects (when present) live under `drivers/wdk/` and can be built via:

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

## AeroGPU (Win7 WDDM) source builds

The in-tree AeroGPU Windows 7 WDDM driver stack lives under `drivers/aerogpu/` (start at `drivers/aerogpu/README.md`).

It is built with **WDK10 + MSBuild** (no WDK 7.1 `build.exe` flow). The primary entrypoint is:

- `drivers\aerogpu\aerogpu.sln`

From a WDK/VS developer prompt (or an EWDK `LaunchBuildEnv.cmd` shell):

```powershell
msbuild .\drivers\aerogpu\aerogpu.sln /m /t:Build /p:Configuration=Release /p:Platform=x64
msbuild .\drivers\aerogpu\aerogpu.sln /m /t:Build /p:Configuration=Release /p:Platform=Win32
```

For a fully scripted/reproducible build (matching CI), use the pipeline documented in `docs/16-windows7-driver-build-and-signing.md`:

```powershell
.\ci\install-wdk.ps1
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json -Drivers aerogpu
```

## virtio-win source builds

The upstream virtio-win drivers (viostor/NetKVM/viosnd/vioinput) are the practical baseline for Win7 virtio support.

If you need to modify/rebuild them:

1. Clone the upstream driver sources.
2. Build using the EWDK/WDK toolchain the project expects.
3. Re-run `drivers/scripts/make-driver-pack.ps1` pointing at the built output (or adjust the script to pack from your build tree instead of the ISO).

This repo intentionally keeps the build steps scripted and the output packaged into a single ZIP so the emulator can treat it as an artifact.

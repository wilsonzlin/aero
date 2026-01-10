# WDK workspace (in-tree Aero drivers)

This directory is reserved for **in-tree Aero Windows drivers** that we may need beyond upstream virtio-win (notably the optional GPU path).

Today, Aero consumes upstream virtio-win for the baseline virtio stack and focuses on making that workflow repeatable (`drivers/scripts/`).

## Reproducible build recommendation

Use the **Enterprise WDK (EWDK)** so that the build environment can be pinned and recreated without requiring a full Visual Studio installation on the build machine.

From an EWDK build environment shell:

```powershell
.\drivers\wdk\build.ps1
```

## What this script does

`build.ps1` is a convenience wrapper that finds and builds all `.sln` files under `drivers/wdk/`.

It is intentionally simple; real driver projects will define their own build matrix (Win7 x86/x64, checked/free, etc).


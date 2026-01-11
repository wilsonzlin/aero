# Legacy AeroGPU Windows 7 driver stack (archived)

This directory intentionally contains only small **pointer/stub** files (this README and a
redirecting `docs/driver_install.md` and stub `inf/aerogpu.inf`), not a real driver source tree.

Historically, the repo had a `guest/windows/` Win7 AeroGPU prototype driver tree. It has since
been **archived** to avoid accidental installs of an incomplete/obsolete package.

## Supported Win7 AeroGPU drivers (recommended)

Use the maintained Win7 driver package under:

- [`drivers/aerogpu/packaging/win7/README.md`](../../drivers/aerogpu/packaging/win7/README.md)

## Archived prototype (historical reference only)

The archived prototype sources and its old install guide live under:

- [`prototype/legacy-win7-aerogpu-1ae0/guest/windows/`](../../prototype/legacy-win7-aerogpu-1ae0/guest/windows/)

Important limitations of the archived prototype:

- Targets the deprecated AeroGPU **1AE0** prototype PCI identity (not the supported **1AED** / **A3A0** ABIs).
- On Windows 7 x64 it is **not WOW64-complete** (no x86 UMD), so **32-bit D3D9 apps will fail**.

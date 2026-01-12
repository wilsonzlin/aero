# Win7 virtio (KMDF) scaffolding

This directory contains a minimal, Windows 7–compatible KMDF build layout for virtio
drivers:

* `virtio-core` – a small, reusable static library with virtio-pci *modern* transport
  discovery + feature negotiation (`VERSION_1`).
* `virtio-transport-test` – a KMDF “smoke test” driver that binds to a virtio modern
  PCI device and exercises the transport init/feature negotiation path (no virtqueues).
  It is intentionally **not** CI-packaged (no `ci-package.json`) so it does not ship in
  Guest Tools / driver bundle artifacts.

The intent is to provide a repeatable starting point for future virtio drivers
(`virtio-input`, `virtio-blk`, `virtio-net`, …) that must still run on Windows 7.

## Toolchain requirements

* Visual Studio Build Tools (or Visual Studio) + a modern Windows Driver Kit (WDK).
  * CI provisions a pinned Windows Kits toolchain via `ci/install-wdk.ps1`.
* Drivers target Windows 7 (`TargetVersion=Windows7`) and KMDF 1.9 (`KmdfLibraryVersion=1.9` in the INF).

## Building

The projects are MSBuild/WDK driver projects (no `build.exe` dependency) and can be built with `msbuild.exe`.

### Build from command line (recommended)

From a Developer PowerShell / command prompt with the WDK installed:

```bat
cd \path\to\repo\drivers\win7\virtio
msbuild virtio-transport-test\virtio-transport-test.vcxproj /t:Build /p:Configuration=Release /p:Platform=Win32
```

For x64:

```bat
msbuild virtio-transport-test\virtio-transport-test.vcxproj /t:Build /p:Configuration=Release /p:Platform=x64
```

### Build from Visual Studio

Open `win7-virtio.sln` (or the individual `*.vcxproj`) in Visual Studio, select a configuration/platform, and build.

## Installing the test driver in a Win7 VM

1. Build `virtio-transport-test` for the VM’s architecture.
2. Copy the following to the VM:
   * `drivers\win7\virtio\virtio-transport-test\virtio-transport-test.inf`
   * The built `virtio-transport-test.sys` (from the project’s build output directory)
3. Enable test signing:
    ```bat
    bcdedit /set testsigning on
    shutdown /r /t 0
   ```
4. Install:
   * Device Manager → find the virtio PCI device → Update Driver → Have Disk… →
     select `virtio-transport-test.inf`.

### Hardware ID / device binding

The INF intentionally defaults to a **non-contract** virtio PCI ID so it cannot
steal binding from real Aero devices when multiple driver packages are staged:

* `PCI\VEN_1AF4&DEV_1040`

To bind to a different virtio modern device, edit the hardware ID in:

* `virtio-transport-test/virtio-transport-test.inf`

## Viewing logs

The driver uses `DbgPrint()` for logging.

Options:

* **DbgView** (Sysinternals):
  * Run as Administrator
  * Enable **Capture Kernel**
* **WinDbg / KD** kernel debugging:
  * Enable debugging (`bcdedit /debug on`) and attach as usual.

On successful start you should see debug output showing:

* virtio-pci vendor capabilities discovered (common/notify/isr/device cfg)
* device features read + negotiated feature set (requesting `VERSION_1`)

## Hardware-free virtio-pci capability parser tests (Linux/CI)

Virtio 1.0 PCI “modern” devices expose their transport registers through PCI
vendor-specific capabilities. This repo includes a small portable C99 parser +
unit tests with synthetic PCI config spaces so capability discovery can be
regression-tested without requiring real hardware.

From the repo root:

```bash
bash ./drivers/win7/virtio/tests/build_and_run.sh
```

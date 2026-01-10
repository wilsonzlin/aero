# Win7 virtio (KMDF / WDK 7.1) scaffolding

This directory contains a minimal, Windows 7–compatible KMDF build layout for virtio
drivers:

* `virtio-core` – a small, reusable static library with virtio-pci *modern* transport
  discovery + feature negotiation (`VERSION_1`).
* `virtio-transport-test` – a KMDF “smoke test” driver that binds to a virtio modern
  PCI device and exercises the transport init/feature negotiation path (no virtqueues).

The intent is to provide a repeatable starting point for future virtio drivers
(`virtio-input`, `virtio-blk`, `virtio-net`, …) that must still run on Windows 7.

## Toolchain requirements

* Windows Driver Kit (WDK) 7.1 (7600.16385.1)
* Visual Studio 2010 or Visual Studio 2012
  * WDK 7.1 ships with VS2010 integration; VS2012 can open VS2010 solutions.

## Building

The projects are **NMake wrapper projects** that invoke the classic WDK 7.1 `build.exe`
system. Build from an appropriate **WDK Build Environment** command prompt so that
`build.exe` and its environment variables are configured.

### Build from command line (recommended)

From a WDK build environment command prompt:

```bat
cd \path\to\repo\drivers\win7\virtio
msbuild virtio.sln /t:Build /p:Configuration=Release /p:Platform=Win32
```

For x64, use the x64 WDK environment prompt and:

```bat
msbuild virtio.sln /t:Build /p:Configuration=Release /p:Platform=x64
```

### Build from Visual Studio

Launch Visual Studio from a WDK Build Environment prompt (or ensure the WDK build
tools are on your `PATH`), open `virtio.sln`, select a configuration/platform, and
build.

## Installing the test driver in a Win7 VM

1. Build `virtio-transport-test` for the VM’s architecture.
2. Copy the following to the VM:
   * `drivers\win7\virtio\virtio-transport-test\virtio-transport-test.inf`
   * The built `virtio-transport-test.sys` (from the project’s `obj*` output dir)
3. Enable test signing:
   ```bat
   bcdedit /set testsigning on
   shutdown /r /t 0
   ```
4. Install:
   * Device Manager → find the virtio PCI device → Update Driver → Have Disk… →
     select `virtio-transport-test.inf`.

### Hardware ID / device binding

The INF currently matches the virtio-input modern PCI ID:

* `PCI\VEN_1AF4&DEV_1052`

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
./drivers/win7/virtio/tests/build_and_run.sh
```

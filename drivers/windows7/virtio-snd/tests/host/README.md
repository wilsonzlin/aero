<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd host unit tests

These tests build and run the Windows 7 virtio-snd **protocol engines** (control/tx/rx)
in a normal host environment (Linux/macOS/Windows) using small WDK shims. This lets CI catch
regressions in:

- Control message framing/parsing (contract-v1 subset)
- TX/RX virtqueue descriptor + SG building
- TX/RX completion/status handling and pool behavior

## Prerequisites

- CMake in `PATH` (`cmake` + `ctest`).
- A C compiler toolchain:
  - On Linux/macOS, Clang/GCC should work.
  - On Windows, Visual Studio / “Build Tools for Visual Studio” (MSVC) is recommended.
    - Run from a “Developer PowerShell/Command Prompt for VS” so `cl.exe` is available.
    - Ninja is optional.
- On Windows, PowerShell (`pwsh` or Windows PowerShell). If script execution is blocked, use
  `-ExecutionPolicy Bypass` (or `Set-ExecutionPolicy -Scope Process Bypass`).

## Run

### Full suite (recommended)

The canonical entrypoint is the parent CMake project at `drivers/windows7/virtio-snd/tests/`,
which builds **all** host-buildable virtio-snd tests (including `virtiosnd_proto_tests`) and
adds this directory as a subdirectory.

From the repo root:

```sh
./scripts/run-host-tests.sh
```

On Windows:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1
```

Replace `pwsh` with `powershell.exe` if you are using Windows PowerShell.

To force a clean rebuild:

```sh
./scripts/run-host-tests.sh --clean
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Clean
```

The default build directory is `out/virtiosnd-tests`. Override with:

```sh
./scripts/run-host-tests.sh --build-dir out/my-virtiosnd-tests
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -BuildDir out\my-virtiosnd-tests
```

Multi-config generators (Visual Studio, Ninja Multi-Config) require a build/test configuration.
`run-host-tests.ps1` auto-detects this and uses `-Configuration` (default: `Release`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Configuration Debug
```

Note: `-Configuration` is only used when the selected CMake generator is multi-config (for example
Visual Studio). For single-config generators (Ninja/Makefiles), the script configures
`CMAKE_BUILD_TYPE=Release`; to do a Debug build in that mode, configure/build manually with
`-DCMAKE_BUILD_TYPE=Debug`.

Troubleshooting (Windows):

- If you see “`cl.exe` not found” / CMake cannot compile, open a **Developer PowerShell/Command Prompt
  for VS** (so MSVC environment variables are set).
- To force a specific CMake generator, set `CMAKE_GENERATOR` and re-run with `-Clean`:
  - PowerShell: `$env:CMAKE_GENERATOR = 'Ninja'` or `$env:CMAKE_GENERATOR = 'Visual Studio 17 2022'`
- If script execution is blocked by policy, use `-ExecutionPolicy Bypass` (as shown) or run
  `Set-ExecutionPolicy -Scope Process Bypass`.

Or run directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests -B out/virtiosnd-tests
cmake --build out/virtiosnd-tests
ctest --test-dir out/virtiosnd-tests --output-on-failure
```

### This directory only (subset)

For fast iteration on just the shim-based protocol-engine tests in this folder, you can also
run:

> Note: this subset does **not** include `virtiosnd_proto_tests` (which lives in the parent
> `drivers/windows7/virtio-snd/tests/` project).

```sh
./scripts/run-host-tests.sh --host-only
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -HostOnly
```

The default build directory for `--host-only` is `out/virtiosnd-host-tests`. Override with:

```sh
./scripts/run-host-tests.sh --host-only --build-dir out/my-virtiosnd-host-tests
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -HostOnly -BuildDir out\my-virtiosnd-host-tests
```

Or run directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests --output-on-failure
```

Note: for multi-config generators (Visual Studio, Ninja Multi-Config), pass `-Configuration <cfg>`
to the PowerShell runner, or add `--config <cfg>` / `ctest -C <cfg>` when invoking CMake/CTest
directly.

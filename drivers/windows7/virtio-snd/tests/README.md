<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd host tests (CMake)

This directory is the **canonical** host-buildable virtio-snd unit test suite entrypoint.

It builds the full superset of host tests, including:

- `virtiosnd_sg_tests`
- `virtiosnd_proto_tests` (integrated tests that compile a subset of `drivers/windows7/virtio-snd/src/*.c`)
- everything under [`host/`](./host/) (added as a subdirectory)

## Run

Prerequisites:

- CMake in `PATH` (`cmake` + `ctest`).
- A C compiler toolchain.
  - On Windows, Visual Studio / “Build Tools for Visual Studio” (MSVC) is recommended.
    - Run from a “Developer PowerShell/Command Prompt for VS” so `cl.exe` is available.
    - Ninja is optional.
- On Windows, PowerShell 7+ (`pwsh`) or Windows PowerShell 5.1 (`powershell.exe`). If script execution is blocked, use
  `-ExecutionPolicy Bypass` (or `Set-ExecutionPolicy -Scope Process Bypass`).

### Linux/macOS (Bash)

Helper script:

```sh
./scripts/run-host-tests.sh
```

### Windows (PowerShell)

PowerShell runner: `drivers/windows7/virtio-snd/scripts/run-host-tests.ps1`.

Default run:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1
```

Replace `pwsh` with `powershell.exe` if you are using Windows PowerShell.

Clean rebuild:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Clean
```

Custom build output directory (relative to the repo root, or absolute):

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

### Direct CMake invocation

From the repo root (any platform):

```sh
cmake -S drivers/windows7/virtio-snd/tests -B out/virtiosnd-tests
cmake --build out/virtiosnd-tests
ctest --test-dir out/virtiosnd-tests --output-on-failure
```

Note: for multi-config generators (Visual Studio, Ninja Multi-Config), add:

- `--config <cfg>` to `cmake --build`
- `-C <cfg>` to `ctest`
- The Bash helper script (`scripts/run-host-tests.sh`) does not currently accept a configuration
  parameter; use the direct commands above if you configured a multi-config generator.

## Subset: `host/` only

For faster iteration on just the shim-based protocol-engine tests under `host/`:

### Linux/macOS (Bash)

```sh
./scripts/run-host-tests.sh --host-only
```

### Windows (PowerShell)

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -HostOnly
```

or build `host/` directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests --output-on-failure
```

Note: for multi-config generators (Visual Studio, Ninja Multi-Config), pass `-Configuration <cfg>`
to the PowerShell runner, or add `--config <cfg>` / `ctest -C <cfg>` when invoking CMake/CTest
directly.

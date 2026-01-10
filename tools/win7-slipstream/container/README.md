# Win7 Slipstream (Container Runtime)

This directory provides a containerized runtime for `aero-win7-slipstream`, so users on macOS/Windows/Linux don’t need to locally install the external Linux tools the slipstream process depends on.

## What the container provides

The image includes these tools (installed via `apt`):

- `xorriso` (ISO authoring)
- `p7zip` (`p7zip-full`, for 7z/zip extraction)
- `wimlib-imagex` (`wimtools`, for manipulating `install.wim`)
- `hivexregedit` (`libhivex-bin`, for offline registry editing)
- `ca-certificates`

## What you must supply

- A Windows 7 ISO (`.iso`) **provided at runtime via bind mount**
- Any drivers/packages you want to slipstream **provided at runtime via bind mount**
- An output directory **provided at runtime via bind mount**

This image intentionally does **not** contain any Windows images or copyrighted content.

## Build

From the repository root (recommended, so the Rust source is in the build context):

```bash
docker build -t aero/win7-slipstream -f tools/win7-slipstream/container/Dockerfile .
```

If you copied this directory out as a standalone build context containing the Rust crate, you can build with:

```bash
docker build -t aero/win7-slipstream .
```

## Run

Example that mounts an input ISO, a drivers directory, and an output directory, then runs `patch-iso` inside the container.

> The exact `patch-iso` CLI flags depend on `aero-win7-slipstream`; adjust as needed.

```bash
docker run --rm -it \
  --mount type=bind,source="$PWD/win7.iso",target=/input/win7.iso,readonly \
  --mount type=bind,source="$PWD/drivers",target=/drivers,readonly \
  --mount type=bind,source="$PWD/out",target=/out \
  aero/win7-slipstream \
  patch-iso \
    --input-iso /input/win7.iso \
    --drivers /drivers \
    --output-iso /out/win7-slipstream.iso
```

## Convenience wrapper scripts

For a “just run it” workflow (including building the image if it doesn’t exist yet):

- Bash: `tools/win7-slipstream/scripts/slipstream-in-container.sh`
- PowerShell: `tools/win7-slipstream/scripts/slipstream-in-container.ps1`

Both wrappers mount your current working directory to `/work` inside the container and pass through all CLI arguments to `aero-win7-slipstream`.

The Bash wrapper also detects common `patch-iso` flags (`--input-iso`, `--drivers`, `--output-iso`), bind-mounts those paths even if they’re outside the current working directory, and rewrites them to container paths.

## Podman notes

- Replace `docker` with `podman` (the Dockerfile is compatible).
- On SELinux hosts you may need to add `,Z` (or `,z`) to bind mounts (e.g. `--mount ... ,Z`) so the container can access the files.

## Windows Docker Desktop notes

- Prefer PowerShell and `--mount` to avoid quoting/drive-letter edge cases.
- Example:

```powershell
docker run --rm -it `
  --mount type=bind,source="$((Get-Location).Path)\\win7.iso",target=/input/win7.iso,readonly `
  --mount type=bind,source="$((Get-Location).Path)\\drivers",target=/drivers,readonly `
  --mount type=bind,source="$((Get-Location).Path)\\out",target=/out `
  aero/win7-slipstream `
  patch-iso --input-iso /input/win7.iso --drivers /drivers --output-iso /out/win7-slipstream.iso
```

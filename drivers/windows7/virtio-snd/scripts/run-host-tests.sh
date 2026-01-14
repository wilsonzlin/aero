#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0

set -eu

usage() {
    cat <<EOF
Usage: $(basename "$0") [--host-only] [--clean] [--build-dir <dir>]

Configure, build, and run the virtio-snd host-buildable unit tests.

On Windows, use the PowerShell equivalent:
  pwsh -NoProfile -ExecutionPolicy Bypass -File .\\drivers\\windows7\\virtio-snd\\scripts\\run-host-tests.ps1
  (replace `pwsh` with `powershell` if you are using Windows PowerShell)

Defaults:
  (full suite)   --build-dir out/virtiosnd-tests        (relative to the repo root)
  (--host-only)  --build-dir out/virtiosnd-host-tests   (relative to the repo root)

Examples:
  # From the repo root:
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh

  # Clean rebuild:
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --clean

  # Subset only (tests/host):
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --host-only

  # Custom build output directory:
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --build-dir out/my-tests
EOF
}

host_only=0
clean=0
build_dir=""

while [ $# -gt 0 ]; do
    case "$1" in
        --host-only)
            host_only=1
            shift
            ;;
        --clean)
            clean=1
            shift
            ;;
        -B|--build-dir)
            if [ $# -lt 2 ]; then
                echo "error: $1 requires a directory argument" >&2
                usage >&2
                exit 2
            fi
            build_dir=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unexpected argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if ! command -v cmake >/dev/null 2>&1; then
    echo "error: cmake not found in PATH" >&2
    exit 1
fi
if ! command -v ctest >/dev/null 2>&1; then
    echo "error: ctest not found in PATH (usually provided by CMake)" >&2
    exit 1
fi

script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd)
virtiosnd_dir=$(CDPATH= cd "$script_dir/.." && pwd)
repo_root=$(CDPATH= cd "$virtiosnd_dir/../../.." && pwd)

if [ "$host_only" -eq 1 ]; then
    src_dir=$virtiosnd_dir/tests/host
    default_build_dir=$repo_root/out/virtiosnd-host-tests
else
    src_dir=$virtiosnd_dir/tests
    default_build_dir=$repo_root/out/virtiosnd-tests
fi

if [ -z "$build_dir" ]; then
    build_dir=$default_build_dir
else
    case "$build_dir" in
        /*) ;;
        *) build_dir=$repo_root/$build_dir ;;
    esac
fi

if [ "$clean" -eq 1 ]; then
    echo "Cleaning build directory: $build_dir"
    rm -rf "$build_dir"
fi

echo "Configuring: $src_dir -> $build_dir"
cmake -S "$src_dir" -B "$build_dir"

echo "Building: $build_dir"
cmake --build "$build_dir"

echo "Running tests: $build_dir"
(
    cd "$build_dir"
    ctest --output-on-failure
)

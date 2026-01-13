#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0

set -eu

usage() {
    cat <<EOF
Usage: $(basename "$0") [--clean] [--build-dir <dir>]

Configure, build, and run the virtio-snd host protocol unit tests.

Defaults:
  --build-dir out/virtiosnd-host-tests   (relative to the repo root)

Examples:
  # From the repo root:
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh

  # Clean rebuild:
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --clean

  # Custom build output directory:
  ./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --build-dir out/my-tests
EOF
}

clean=0
build_dir=""

while [ $# -gt 0 ]; do
    case "$1" in
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
src_dir=$virtiosnd_dir/tests/host

if [ -z "$build_dir" ]; then
    build_dir=$repo_root/out/virtiosnd-host-tests
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

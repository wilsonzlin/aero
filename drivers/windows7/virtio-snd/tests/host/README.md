<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd host unit tests

These tests build and run the Windows 7 virtio-snd **protocol engines** (control/tx/rx)
in a normal host environment (Linux/macOS) using small WDK shims. This lets CI catch
regressions in:

- Control message framing/parsing (contract-v1 subset)
- TX/RX virtqueue descriptor + SG building
- TX/RX completion/status handling and pool behavior

## Run

### Full suite (recommended)

The canonical entrypoint is the parent CMake project at `drivers/windows7/virtio-snd/tests/`,
which builds **all** host-buildable virtio-snd tests (including `virtiosnd_proto_tests`) and
adds this directory as a subdirectory.

From the repo root:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh
```

To force a clean rebuild:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --clean
```

The default build directory is `out/virtiosnd-tests`. Override with:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --build-dir out/my-virtiosnd-tests
```

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
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --host-only
```

The default build directory for `--host-only` is `out/virtiosnd-host-tests`. Override with:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --host-only --build-dir out/my-virtiosnd-host-tests
```

Or run directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests --output-on-failure
```

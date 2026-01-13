<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd host tests (CMake)

This directory is the **canonical** host-buildable virtio-snd unit test suite entrypoint.

It builds the full superset of host tests, including:

- `virtiosnd_sg_tests`
- `virtiosnd_proto_tests` (integrated tests that compile a subset of `drivers/windows7/virtio-snd/src/*.c`)
- everything under [`host/`](./host/) (added as a subdirectory)

## Run

From the repo root:

```sh
cmake -S drivers/windows7/virtio-snd/tests -B out/virtiosnd-tests
cmake --build out/virtiosnd-tests
ctest --test-dir out/virtiosnd-tests --output-on-failure
```

Or via the helper script:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh
```

## Subset: `host/` only

For faster iteration on just the shim-based protocol-engine tests under `host/`, either:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --host-only
```

or build `host/` directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests --output-on-failure
```


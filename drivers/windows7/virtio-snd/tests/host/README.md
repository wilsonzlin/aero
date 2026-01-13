<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd host unit tests

These tests build and run the Windows 7 virtio-snd **protocol engines** (control/tx/rx)
in a normal host environment (Linux/macOS) using small WDK shims. This lets CI catch
regressions in:

- Control message framing/parsing (contract-v1 subset)
- TX/RX virtqueue descriptor + SG building
- TX/RX completion/status handling and pool behavior

## Run

From the repo root:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh
```

To force a clean rebuild:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --clean
```

The default build directory is `out/virtiosnd-host-tests`. Override with:

```sh
./drivers/windows7/virtio-snd/scripts/run-host-tests.sh --build-dir out/my-virtiosnd-tests
```

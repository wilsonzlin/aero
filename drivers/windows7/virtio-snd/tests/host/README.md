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
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests
```


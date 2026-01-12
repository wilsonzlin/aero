<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Windows 7 virtio common host unit tests

This directory contains host-buildable unit tests for the reusable virtio helper
code under `drivers/windows7/virtio/common/`.

Some helpers are fully portable and can be tested directly in user mode, while
others are written against WDK/WDM APIs. For the latter, these tests provide
small WDK stubs so CI can validate the *logic* on Linux/macOS without requiring
a Windows build.

Notable targets:

- `virtio_common_tests`: split ring + legacy transport tests (pure portable C).
- `virtio_intx_wdm_tests`: unit tests for the WDM INTx helper
  (`virtio_pci_intx_wdm.c`) using a minimal `wdk_stubs/ntddk.h`.
- `virtio_pci_modern_miniport_tests`: unit tests for the Win7 miniport-style
  virtio-pci modern transport helper (`virtio_pci_modern_miniport.c`) using a
  tiny BAR0 MMIO simulator and WDK stubs.

## Run (standalone)

From the repo root:

```sh
cmake -S drivers/windows7/virtio/common/tests -B out/w7-virtio-common-tests
cmake --build out/w7-virtio-common-tests
ctest --test-dir out/w7-virtio-common-tests --output-on-failure
```

## Run (via top-level CMake)

```sh
cmake -S . -B out/all-tests -DAERO_VIRTIO_BUILD_TESTS=ON -DAERO_AEROGPU_BUILD_TESTS=OFF
cmake --build out/all-tests
ctest --test-dir out/all-tests -R virtio_intx_wdm_tests --output-on-failure
```

## WDK stubs

WDK shims for the WDM-dependent helpers live under `wdk_stubs/`.

This repository contains multiple `ntddk.h` stubs for different test suites, so
each CMake test target must ensure its intended stub directory is first on the
include path (the test targets in this directory use
`target_include_directories(... BEFORE ...)`).


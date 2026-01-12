<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Win7 host-harness unit tests (Python)

These tests validate small, host-side helpers used by the Win7 QEMU harness:

- HTTP server helpers (including the deterministic 1 MiB `-large` payload)
- QEMU keyval quoting
- QMP `input-send-event` command formatting for virtio-input injection
- WAV parsing / non-silence verification helpers for virtio-snd

They are intentionally runnable without QEMU or a Windows guest image.

## Run

From the repo root:

```sh
python3 -m unittest discover -s drivers/windows7/tests/host-harness/tests
```


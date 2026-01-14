<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Win7 host-harness unit tests (Python)

These tests validate small, host-side helpers used by the Win7 QEMU harness:

- HTTP server helpers (including the deterministic 1 MiB `-large` payload and upload verification via POST)
- CLI/argparse compatibility checks (ensuring backwards-compatible alias flags remain accepted)
- QEMU keyval quoting
- QMP `input-send-event` command formatting for virtio-input injection
- virtio-input HID report descriptor classification (keyboard vs relative mouse vs absolute tablet)
- WAV parsing / non-silence verification helpers for virtio-snd
- Marker parsing helpers (PASS/FAIL tokens, per-device IRQ/MSI-X diagnostics)
- Serial marker parsing helpers for IRQ diagnostics:
  - standalone `virtio-<dev>-irq|INFO/WARN|...` lines
  - stable per-device host markers that mirror guest `AERO_VIRTIO_SELFTEST|TEST|...|irq_*` fields (e.g. `VIRTIO_BLK_IRQ`)

They are intentionally runnable without QEMU or a Windows guest image.

## Run

From the repo root:

```sh
python3 -m unittest discover -s drivers/windows7/tests/host-harness/tests
```

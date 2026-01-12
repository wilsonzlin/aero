# Tests (virtio-input)

For the consolidated end-to-end virtio-input validation plan (Rust device model + web runtime + Win7 driver), see:

- [`docs/virtio-input-test-plan.md`](../../../../docs/virtio-input-test-plan.md)

## Host-side unit tests (portable)

`hid_translate.c/h` is written to be portable so the virtio-input → HID mapping can be validated without the Windows WDK.

Run:

```bash
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/hid_translate_test \
  hid_translate_test.c ../src/hid_translate.c && /tmp/hid_translate_test
```

## Manual tests

- `qemu/` — QEMU-based manual bring-up notes.
- `offline-install/` — offline/slipstream install notes (DISM).

For exercising HID output report paths, see:

- `tools/hidtest/` (supports `--led`, `--led-hidd`, and negative pointer tests).

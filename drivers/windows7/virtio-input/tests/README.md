# Tests (virtio-input)

For the consolidated end-to-end virtio-input validation plan (Rust device model + web runtime + Win7 driver), see:

- [`docs/virtio-input-test-plan.md`](../../../../docs/virtio-input-test-plan.md)

## Host-side unit tests (portable)

`hid_translate.c/h` (and the thin `virtio_input.c/h` wrapper around it) are written to be portable so the virtio-input → HID
mapping and report queuing behavior can be validated without the Windows WDK.

Run:

```bash
# From drivers/windows7/virtio-input/
bash tests/run.sh
```

The script will attempt to run the test suite with both `gcc` and `clang` (if present).
To force a specific compiler, set `CC`:

```bash
CC=clang bash tests/run.sh
```

### Adding new host-side tests

Drop a `*_test.c` file in this directory. `tests/run.sh` will build each test into a separate
binary, and will automatically link `../src/<name>.c` if it exists (where `<name>` is the
test filename without the `_test` suffix). This is intended for portable C helpers such as
`hid_translate.c` and LED translation/parsing helpers.

Some tests may depend on multiple `../src/*.c` translation units. In that case, include the
additional sources directly from the test file.

## Manual tests

- `qemu/` — QEMU-based manual bring-up notes.
- `offline-install/` — offline/slipstream install notes (DISM).

For exercising HID output report paths, see:

- `tools/hidtest/` (supports `--led`, `--led-hidd`, and negative pointer tests).

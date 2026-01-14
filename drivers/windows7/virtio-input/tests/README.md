# Tests (virtio-input)

For the consolidated end-to-end virtio-input validation plan (Rust device model + web runtime + Win7 driver), see:

- [`docs/virtio-input-test-plan.md`](../../../../docs/virtio-input-test-plan.md)

## Host-side unit tests (portable)

`hid_translate.c/h` (and the thin `virtio_input.c/h` wrapper around it, plus other helpers in `../src/`) are written to be portable so the virtio-input → HID
mapping and report queuing behavior can be validated without the Windows WDK.

Run the full suite:

```bash
# From drivers/windows7/virtio-input/
bash tests/run.sh
```

### Run via the repo-root CMake/CTest suite (CI path)

These tests are also integrated into the repo-root host test build when
`AERO_VIRTIO_BUILD_TESTS=ON`:

```bash
cmake -S . -B build-virtio-host-tests \
  -DAERO_VIRTIO_BUILD_TESTS=ON \
  -DAERO_AEROGPU_BUILD_TESTS=OFF \
  -DCMAKE_BUILD_TYPE=Release
cmake --build build-virtio-host-tests

# Run everything:
ctest --test-dir build-virtio-host-tests --output-on-failure

# Or just the virtio-input tests:
ctest --test-dir build-virtio-host-tests --output-on-failure -R '^(hid_translate_test|virtio_input_integration_test|virtio_statusq_test|led_.*_test|report_ring_test)$'
```

The script will attempt to run the test suite with both `gcc` and `clang` (if present).
To force a specific compiler, set `CC`:

```bash
CC=clang bash tests/run.sh
```

### Individual tests (manual one-liners)

If you just want to build and run a single test directly with `gcc`:

```bash
# From drivers/windows7/virtio-input/tests/
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/hid_translate_test \
  hid_translate_test.c ../src/hid_translate.c && /tmp/hid_translate_test
```

```bash
# From drivers/windows7/virtio-input/tests/
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/led_translate_test \
  led_translate_test.c ../src/led_translate.c && /tmp/led_translate_test
```

```bash
# From drivers/windows7/virtio-input/tests/
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/led_report_parse_test \
  led_report_parse_test.c ../src/led_report_parse.c && /tmp/led_report_parse_test
```

```bash
# From drivers/windows7/virtio-input/tests/
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/led_output_pipeline_test \
  led_output_pipeline_test.c ../src/led_report_parse.c ../src/led_translate.c && /tmp/led_output_pipeline_test
```

```bash
# From drivers/windows7/virtio-input/tests/
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/virtio_statusq_test \
  virtio_statusq_test.c ../src/virtio_statusq.c && /tmp/virtio_statusq_test
```

```bash
# From drivers/windows7/virtio-input/tests/
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/report_ring_test \
  report_ring_test.c ../src/virtio_input.c ../src/hid_translate.c && /tmp/report_ring_test
```

### Adding new host-side tests

Drop a `*_test.c` file in this directory. `tests/run.sh` will build each test into a separate
binary, and will automatically link `../src/<name>.c` if it exists (where `<name>` is the
test filename without the `_test` suffix). This is intended for portable C helpers such as
`hid_translate.c` and LED translation/parsing helpers.

Some tests may depend on multiple `../src/*.c` translation units. In that case, add a line like
`// TEST_DEPS: foo.c bar.c` to the test source; `tests/run.sh` will link the additional
dependencies.
## Manual tests

- `qemu/` — QEMU-based manual bring-up notes.
- `offline-install/` — offline/slipstream install notes (DISM).

For exercising HID output report paths, see:

- `tools/hidtest/` (supports `--led`, `--led-hidd`, and negative pointer tests).

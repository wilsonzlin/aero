# Host harness (PowerShell) for Win7 virtio selftests

End-to-end virtio-input validation plan (device model + driver + web runtime routing):

- [`docs/virtio-input-test-plan.md`](../../../../docs/virtio-input-test-plan.md) (from repo root)

This directory contains the host-side scripts used to run the Windows 7 guest selftests under QEMU and return a deterministic PASS/FAIL exit code.

## Prerequisites

- QEMU (`qemu-system-x86_64` and optionally `qemu-img`)
  - Must support `disable-legacy=on` for modern-only virtio-pci devices
  - Must support `x-pci-revision=0x01` so devices match the Aero contract v1 revision
- PowerShell:
  - Windows PowerShell 5.1 or PowerShell 7+ should work
- A **prepared Windows 7 image** that:
  - has the virtio drivers installed (virtio-blk + virtio-net + virtio-input, modern-only)
    - Stock QEMU virtio-input devices typically report non-Aero `ID_NAME` strings (e.g. `QEMU Virtio Keyboard`).
      The in-tree Aero virtio-input driver defaults to strict contract mode and will refuse to start for non-Aero keyboard/mouse devices (Code 10) unless
      virtio-input compatibility mode is enabled.
      - Enable by setting:
        `HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters\CompatIdName` (REG_DWORD) = `1`
        and rebooting (or by building the driver with `AERO_VIOINPUT_COMPAT_ID_NAME=1`).
      - When using the in-tree provisioning media generator, you can bake this in via:
        `New-AeroWin7TestImage.ps1 -EnableVirtioInputCompatIdName` (alias: `-EnableVirtioInputCompat`).
    - To enable the optional end-to-end virtio-input event delivery smoke tests (HID input reports),
      the guest selftest must be provisioned with:
      - keyboard + relative mouse: `--test-input-events` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS=1`)
      - (optional) extended keyboard/mouse coverage: `--test-input-events-extended`
        (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1`)
      - (optional) Consumer Control / media keys: `--test-input-media-keys`
        (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_MEDIA_KEYS=1`; if provisioning via `New-AeroWin7TestImage.ps1`, pass
        `-TestInputMediaKeys` (alias: `-TestMediaKeys`))
      - (optional) keyboard LED/statusq smoke test: `--test-input-led` (compat: `--test-input-leds`)
        (env vars: `AERO_VIRTIO_SELFTEST_TEST_INPUT_LED=1` / `AERO_VIRTIO_SELFTEST_TEST_INPUT_LEDS=1`;
        if provisioning via `New-AeroWin7TestImage.ps1`, pass `-TestInputLed` / `-TestInputLeds`)
      - tablet / absolute pointer: `--test-input-tablet-events` (alias: `--test-tablet-events`)
        (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1` / `AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1`)
      - Pair these with host-side QMP injection flags (so the harness injects events and requires the corresponding
        markers to PASS):
        - keyboard/mouse: PowerShell `-WithInputEvents` / Python `--with-input-events`
          (aliases: `--with-virtio-input-events`, `--require-virtio-input-events`, `--enable-virtio-input-events`)
        - wheel: PowerShell `-WithInputWheel` / Python `--with-input-wheel`
          (aliases: `--with-virtio-input-wheel`, `--require-virtio-input-wheel`, `--enable-virtio-input-wheel`)
        - extended events: PowerShell `-WithInputEventsExtended` / Python `--with-input-events-extended`
          (alias: `--with-input-events-extra`)
        - media keys: PowerShell `-WithInputMediaKeys` / Python `--with-input-media-keys`
          (aliases: `--with-virtio-input-media-keys`, `--require-virtio-input-media-keys`, `--enable-virtio-input-media-keys`)
        - LED/statusq: PowerShell `-WithInputLed` / Python `--with-input-led`
          (aliases: `--with-virtio-input-led`, `--require-virtio-input-led`, `--enable-virtio-input-led`; compat:
          `-WithInputLeds` / `--with-input-leds`; compat aliases: `--with-virtio-input-leds`, `--require-virtio-input-leds`,
          `--enable-virtio-input-leds`; no QMP injection required)
        - tablet: PowerShell `-WithInputTabletEvents` / Python `--with-input-tablet-events`
          (aliases: `--with-tablet-events`, `--with-virtio-input-tablet-events`, `--require-virtio-input-tablet-events`, `--enable-virtio-input-tablet-events`)
    - To enable the optional end-to-end virtio-net link flap regression test (QMP `set_link` + guest link state polling),
      the guest selftest must be provisioned with:
      - `--test-net-link-flap` (or env var `AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP=1`)
      - Pair this with the host flag:
        - PowerShell: `-WithNetLinkFlap`
        - Python: `--with-net-link-flap` (aliases: `--with-virtio-net-link-flap`, `--require-virtio-net-link-flap`, `--enable-virtio-net-link-flap`)
  - has virtio-snd installed if you intend to test audio
    - the guest selftest will exercise virtio-snd playback automatically when a virtio-snd device is present and confirm
      a capture endpoint is registered
    - when the guest is provisioned with `--test-snd-capture` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`),
      the selftest also runs a full-duplex regression test
      (`virtio-snd-duplex`) that runs render + capture concurrently
    - the guest selftest also includes an optional WASAPI buffer sizing stress test (`virtio-snd-buffer-limits`) that can be
      enabled via `--test-snd-buffer-limits` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`) and required by the
      host harness with `-WithSndBufferLimits` / `--with-snd-buffer-limits`
      (aliases: `--with-virtio-snd-buffer-limits`, `--require-virtio-snd-buffer-limits`, `--enable-snd-buffer-limits`, `--enable-virtio-snd-buffer-limits`)
    - use `--disable-snd` to skip virtio-snd testing, or `--test-snd` / `--require-snd` to fail if the device is missing
    - use `--disable-snd-capture` to skip capture-only checks (playback still runs when the device is present);
      do not use this when running the harness with `-WithVirtioSnd` / `--with-virtio-snd` (aliases: `--require-virtio-snd`, `--enable-virtio-snd`) (capture is required)
  - has `aero-virtio-selftest.exe` installed
  - runs the selftest automatically on boot and logs to `COM1`
  - has at least one **mounted/usable virtio-blk volume** (the selftest writes a temporary file to validate disk I/O)

For the in-tree clean-room Aero virtio driver stack, the canonical INF names are:

- `aero_virtio_blk.inf`
- `aero_virtio_net.inf`
- `aero_virtio_input.inf` (optional; keyboard + relative mouse)
- `aero_virtio_tablet.inf` (optional; tablet / absolute pointer)
- `aero_virtio_snd.inf` (optional)

Note: documentation under `drivers/windows7/tests/` intentionally avoids spelling deprecated legacy INF basenames.
CI scans this tree for those strings. Use canonical `aero_virtio_*.inf` names in examples; refer to any filename alias
generically as the `*.inf.disabled` file.

## Running tests

Example:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -SerialLogPath ./win7-serial.log `
  -HttpLogPath ./win7-http.log `
  -TimeoutSeconds 600
```

`-HttpLogPath` is optional; when set, the harness appends one line per HTTP request (`method path status bytes`) to the file
(best-effort; logging failures are ignored).

### QEMU virtio PCI ID preflight (host-side QMP `query-pci`)

To catch QEMU/device-arg misconfiguration early, the harness can optionally query QEMU's PCI topology via QMP
(`query-pci`) and validate that the expected virtio devices are actually present with the expected IDs/revision
(notably the `REV_01` contract major version gate used by the Aero Win7 driver INFs).

Flags:

- PowerShell: `-QemuPreflightPci` (alias: `-QmpPreflightPci`)
- Python: `--qemu-preflight-pci` (alias: `--qmp-preflight-pci`)

Behavior:

- Default (contract-v1) mode:
  - Requires `VEN_1AF4`
  - Requires `DEV_1041` (virtio-net), `DEV_1042` (virtio-blk), `DEV_1052` (virtio-input)
  - Requires `DEV_1059` when virtio-snd is enabled
  - Requires `REV_01` for those devices
- Transitional mode (`--virtio-transitional` / `-VirtioTransitional`): permissive (only asserts that at least one `VEN_1AF4`
  device exists)

On success, the harness emits a CI-scrapable host marker:

- `AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=...|vendor=1af4|devices=...`

On mismatch, the harness fails fast and includes a compact dump of the `query-pci` results.

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -QemuPreflightPci `
  -TimeoutSeconds 600
```

Example (Python):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --qemu-preflight-pci \
  --timeout-seconds 600
```

For repeatable runs without mutating the base image, use snapshot mode:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -TimeoutSeconds 600
```

### Dry-run / print QEMU commandline

To debug CI failures (or just to see the exact device arguments/quoting), both harnesses can print
the computed QEMU argv without starting the HTTP server or launching QEMU.

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -DryRun
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --dry-run
```

The Python harness prints:

1. A machine-readable JSON argv array (first line)
2. A best-effort single-line command (second line):
   - POSIX: shell-escaped via `shlex.quote`
   - Windows: cmdline-escaped via `subprocess.list2cmdline`

Note: `--print-qemu` and `--print-qemu-cmd` are accepted as aliases for `--dry-run`.

Note: `-PrintQemuArgs` is accepted as an alias for `-DryRun`.

### Virtio-net link flap regression test (host-side QMP `set_link`)

To deterministically exercise virtio-net **link status change handling** (including config interrupts),
the harness can flap the virtio-net link state from the host via QMP.

Provisioning:

- Guest selftest flag: `--test-net-link-flap` (or env var `AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP=1`)
- Provisioning script: `New-AeroWin7TestImage.ps1 -TestNetLinkFlap` (adds `--test-net-link-flap` to the scheduled task)

Running (requires QMP; enabled automatically by the flag):

- PowerShell harness: `-WithNetLinkFlap`
- Python harness: `--with-net-link-flap` (aliases: `--with-virtio-net-link-flap`, `--require-virtio-net-link-flap`, `--enable-virtio-net-link-flap`)

Behavior:

1. Wait for guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY`
2. QMP: `set_link name=aero_virtio_net0 up=false`
   - Some QEMU builds interpret `name` as a **netdev id** rather than a device/QOM id. The harness therefore falls back to
     `name=net0` if the stable device id is rejected.
3. Sleep 3 seconds
4. QMP: `set_link name=<same> up=true`
5. Require guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS|...` (missing/SKIP/FAIL becomes a deterministic harness failure)

The guest `virtio-net-link-flap` marker includes timing fields (`down_sec`, `up_sec`) and, when supported by the
installed `aero_virtio_net` diagnostics device, also reports best-effort config interrupt counters
(`cfg_vector`, `cfg_intr_*`, `cfg_intr_*_delta`) to validate the link transitions were driven by config interrupts.

The host harness also emits a CI-scrapable marker:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|PASS|name=<aero_virtio_net0|net0>|down_delay_sec=3`

### Forcing / limiting virtio MSI-X vector count (QEMU `vectors=`)

To deterministically exercise the Aero virtio drivers' **multi-vector MSI-X** paths *and* fallback behavior when fewer
messages are available (vector starvation), the harness can request a specific MSI-X table size from QEMU by appending
`,vectors=N` to virtio-pci devices it creates.

Global (applies to all virtio devices created by the harness):

- PowerShell: `-VirtioMsixVectors N`
- Python: `--virtio-msix-vectors N`

Per-device overrides (take precedence over the global value):

- PowerShell:
  - `-VirtioNetVectors N`
  - `-VirtioBlkVectors N`
  - `-VirtioInputVectors N`
  - `-VirtioSndVectors N` (only relevant when `-WithVirtioSnd` is enabled)
- Python:
  - `--virtio-net-vectors N`
  - `--virtio-blk-vectors N`
  - `--virtio-input-vectors N`
  - `--virtio-snd-vectors N` (only relevant when `--with-virtio-snd/--require-virtio-snd/--enable-virtio-snd` is enabled)

Notes:

- This requires a QEMU build where the virtio device exposes the `vectors` property. The harness probes
  `-device <name>,help` and fails fast with a clear error if unsupported (disable the flag or upgrade QEMU).
- Typical values to try are `2`, `4`, or `8`.
- Windows may still allocate **fewer** MSI-X messages than requested (for example due to platform/OS limits). The Aero
  drivers are expected to fall back to the number of vectors actually granted (including single-vector MSI-X or INTx), so
  requesting more vectors should not break functional testing.

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -VirtioMsixVectors 4 `
  -TimeoutSeconds 600
```

Example (vector starvation; force “all on vector0” by limiting each device to 1 vector):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -VirtioNetVectors 1 `
  -VirtioBlkVectors 1 `
  -VirtioInputVectors 1 `
  -TimeoutSeconds 600
```

Example (Python; provide enough vectors for per-queue MSI-X on virtio-net, and separate config/queue on virtio-blk):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --virtio-net-vectors 4 \
  --virtio-blk-vectors 2 \
  --timeout-seconds 600
```

### INTx-only mode (disable MSI-X)

By default, QEMU virtio-pci devices use MSI-X interrupts when available. To regression-test the **legacy INTx + ISR**
paths in the Windows 7 drivers (contract v1 baseline), you can force QEMU to expose **no MSI-X capability** for the
virtio devices the harness creates by appending `vectors=0`:

- PowerShell: `-VirtioDisableMsix`
- Python: `--virtio-disable-msix`

Aliases:

- PowerShell: `-ForceIntx` / `-IntxOnly`
- Python: `--force-intx` / `--intx-only`

Notes:

- `-VirtioDisableMsix` / `--virtio-disable-msix` is **mutually exclusive** with the MSI-X vector override flags
  (`-VirtioMsixVectors` / `--virtio-msix-vectors` and per-device `*Vectors` variants) and with
  `-RequireVirtio*Msix` / `--require-virtio-*-msix`.
- This requires a QEMU build that supports the virtio `vectors` property and accepts `vectors=0`. Some QEMU builds may
  reject `vectors=0` (or omit `vectors` entirely). In that case, the harness fails fast and includes the QEMU error in
  the usual stderr sidecar log (`<serial-base>.qemu.stderr.log`).
- When enabled, the harness emits a machine-readable host marker:
  - `AERO_VIRTIO_WIN7_HOST|CONFIG|force_intx=1`

Expected markers (guest-side, for log scraping):

- `virtio-*-irq|INFO|mode=intx|...` (per device)
- virtio-blk miniport IRQ diagnostic: `...|mode=intx|...` (when emitted by the guest selftest)

### Requiring MSI-X to be enabled (host-side QMP check)

By default, the harness will still PASS when Windows falls back to INTx (MSI/MSI-X is optional per the
`AERO-W7-VIRTIO` v1 contract). To make MSI-X a **hard requirement**, you can ask the harness to fail if QEMU reports
MSI-X as disabled on the corresponding PCI function (best-effort introspection via QMP `query-pci` or HMP `info pci`).

- PowerShell:
  - `-RequireVirtioNetMsix` *(alias: `-RequireNetMsix`; also requires the guest `virtio-net-msix` marker to report `mode=msix`)*
  - `-RequireVirtioBlkMsix` *(alias: `-RequireBlkMsix`; also requires the guest `virtio-blk-msix` marker to report `mode=msix`)*
  - `-RequireVirtioSndMsix` *(alias: `-RequireSndMsix`; requires `-WithVirtioSnd`; also requires the guest `virtio-snd-msix` marker to report `mode=msix`)*
  - `-RequireVirtioInputMsix` *(alias: `-RequireInputMsix`; guest marker check: requires `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|...` to report `mode=msix`)*
- Python:
  - `--require-virtio-net-msix` *(alias: `--require-net-msix`; also requires the guest `virtio-net-msix` marker to report `mode=msix`)*
  - `--require-virtio-blk-msix` *(alias: `--require-blk-msix`; also requires the guest `virtio-blk-msix` marker to report `mode=msix`)*
  - `--require-virtio-snd-msix` *(alias: `--require-snd-msix`; requires `--with-virtio-snd`; also requires the guest `virtio-snd-msix` marker to report `mode=msix`)*
  - `--require-virtio-input-msix` *(alias: `--require-input-msix`; guest marker check: requires `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|...` to report `mode=msix`)*

Notes:

- This check relies on QMP support for either `query-pci` or `human-monitor-command` (`info pci`).
- On failure, the harness reports tokens like `VIRTIO_SND_MSIX_NOT_ENABLED` (or `QMP_MSIX_CHECK_UNSUPPORTED` if QEMU cannot report MSI-X state).
- This checks whether MSI-X is enabled on the **QEMU device**, not whether Windows actually granted multiple message
  interrupts. For guest-observed mode/message counts, use the guest `virtio-<dev>-irq|INFO|...` lines and the mirrored
  host markers (`AERO_VIRTIO_WIN7_HOST|VIRTIO_*_IRQ|...` / `...|VIRTIO_*_IRQ_DIAG|...`).
  - Exception: for virtio-net, `-RequireVirtioNetMsix` / `--require-virtio-net-msix` also requires the guest marker
    `AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|...` so the harness validates the **effective** interrupt mode.
  - Exception: for virtio-blk, `-RequireVirtioBlkMsix` / `--require-virtio-blk-msix` also requires the guest marker
    `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...` so the harness validates the **effective** interrupt mode.
  - Exception: for virtio-snd, `-RequireVirtioSndMsix` / `--require-virtio-snd-msix` requires the guest marker
    `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...` so the harness validates the **effective** interrupt mode.
  - Exception: for virtio-input, `-RequireVirtioInputMsix` / `--require-virtio-input-msix` requires the guest marker
    `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|...` so the harness validates the **effective** interrupt mode.

Tip (guest-side fail-fast):

- To make the guest fail immediately (instead of waiting for the harness to parse the final marker), provision the guest
  selftest with:
  - virtio-blk: `aero-virtio-selftest.exe --expect-blk-msi` (or env var `AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1`)
    - When provisioning via `New-AeroWin7TestImage.ps1`, use `-ExpectBlkMsi`.
  - virtio-net: `aero-virtio-selftest.exe --require-net-msix` (or env var `AERO_VIRTIO_SELFTEST_REQUIRE_NET_MSIX=1`)
    - When provisioning via `New-AeroWin7TestImage.ps1`, use `-RequireNetMsix`.
  - virtio-input: `aero-virtio-selftest.exe --require-input-msix` (or env var `AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1`)
    - When provisioning via `New-AeroWin7TestImage.ps1`, use `-RequireInputMsix`.
  - virtio-snd: `aero-virtio-selftest.exe --require-snd-msix` (or env var `AERO_VIRTIO_SELFTEST_REQUIRE_SND_MSIX=1`)
    - When provisioning via `New-AeroWin7TestImage.ps1`, use `-RequireSndMsix`.

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -WithVirtioSnd `
  -RequireVirtioSndMsix `
  -TimeoutSeconds 600
```

### virtio-blk MSI/MSI-X interrupt mode (guest-observed, optional)

Newer `aero-virtio-selftest.exe` binaries emit a dedicated marker describing the virtio-blk interrupt mode/vectors:
`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS/SKIP|mode=intx/msix/unknown|messages=<n>|config_vector=<n>|queue_vector=<n>`.

If the virtio-blk miniport interrupt diagnostics are unavailable (e.g. older miniport contract / truncated IOCTL payload),
the marker is emitted as `SKIP|reason=...|...`.

When `--require-virtio-blk-msix` is used, the **Python** harness additionally requires `mode=msix` from this marker.
The **PowerShell** harness does the same when `-RequireVirtioBlkMsix` is set.

Example (Python):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --require-virtio-blk-msix \
  --timeout-seconds 600 \
  --snapshot
```

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -RequireVirtioBlkMsix `
  -TimeoutSeconds 600
```

### virtio-blk StorPort recovery counters (optional gating)

Newer virtio-blk miniport builds expose StorPort recovery counters via the miniport query IOCTL contract.

The guest selftest emits a dedicated machine-readable marker when the IOCTL payload includes the counter region
(the optional `capacity_change_events` field may be reported as `not_supported` on older miniport builds):

`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n\|not_supported>`

If the payload is too short (older miniport contract / truncated), it emits:

`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=ioctl_payload_truncated|returned_len=...`

For log scraping, the host harness mirrors the last observed guest marker into a stable host marker:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO/SKIP|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n\|not_supported>`

Newer miniport builds may also report timeout/error recovery activity counters (`ResetDetected` → `HwResetBus`) via the
same IOCTL contract. When present, the guest selftest emits:

`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=...|hw_reset_bus=...`

If the IOCTL payload is too short (older miniport contract / truncated), it emits:

`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|SKIP|reason=ioctl_payload_truncated|returned_len=...`

The host harness mirrors this into:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO/SKIP|reset_detected=...|hw_reset_bus=...`

The guest selftest also logs best-effort miniport diagnostics lines (not AERO markers) when the IOCTL payload includes
additional optional fields:

- `virtio-blk-miniport-flags|INFO/WARN|...`
- `virtio-blk-miniport-reset-recovery|INFO/WARN|...`

For log scraping, the host harness mirrors these into stable host markers (informational only; does not affect PASS/FAIL):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|INFO/WARN|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|INFO/WARN|...`

To enforce that the virtio-blk miniport did not mark the device removed/surprise-removed (and optionally did not
leave any reset-in-progress/pending flags), enable:

- PowerShell:
  - `-RequireNoBlkMiniportFlags` (fails if any removed/surprise_removed/reset_in_progress/reset_pending is non-zero)
  - `-FailOnBlkMiniportFlags` (fails if removed or surprise_removed is non-zero only)
- Python:
  - `--require-no-blk-miniport-flags`
  - `--fail-on-blk-miniport-flags`

On failure it emits deterministic tokens:

- `FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO: ...`
- `FAIL: VIRTIO_BLK_MINIPORT_FLAGS_REMOVED: ...`

To enforce that virtio-blk did not trigger timeout/error recovery resets (best-effort; ignores missing/SKIP markers), enable:

- PowerShell:
  - `-RequireNoBlkResetRecovery` (fails on any non-zero `reset_detected` or `hw_reset_bus`)
  - `-FailOnBlkResetRecovery` (fails on any non-zero `hw_reset_bus` only)
- Python:
  - `--require-no-blk-reset-recovery`
  - `--fail-on-blk-reset-recovery`

On failure it emits deterministic tokens:

- `FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO: ...`
- `FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED: ...`

To enforce a minimal “no unexpected aborts/resets” policy (checks `abort`/`reset_device`/`reset_bus` only), enable:

- PowerShell: `-FailOnBlkRecovery`
- Python: `--fail-on-blk-recovery`

On failure it emits:

`FAIL: VIRTIO_BLK_RECOVERY_DETECTED: ...`

Backward compatibility: legacy guest selftests may append these counters to the main virtio-blk marker (best-effort):
- `abort_srb`
- `reset_device_srb`
- `reset_bus_srb`
- `pnp_srb`
- `ioctl_reset`

Example (guest marker):

`AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|...|abort_srb=0|reset_device_srb=0|reset_bus_srb=0|pnp_srb=0|ioctl_reset=0`

For log scraping, the host harness mirrors these into a stable host marker:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO|abort_srb=...|reset_device_srb=...|reset_bus_srb=...|pnp_srb=...|ioctl_reset=...`

To enforce a stricter “no unexpected resets/aborts” policy (checks all fields above) in CI, enable:

- PowerShell: `-RequireNoBlkRecovery`
- Python: `--require-no-blk-recovery`

If any counter is non-zero, the harness fails with a deterministic token:

`FAIL: VIRTIO_BLK_RECOVERY_NONZERO: ...`

Note: this is best-effort. `VIRTIO_BLK_RECOVERY` is derived from either:

- the dedicated guest `virtio-blk-counters` marker (preferred), or
- legacy fields on the guest `virtio-blk` marker (older guest binaries).

### virtio-net MSI/MSI-X interrupt mode (guest-observed, optional)

Newer `aero-virtio-selftest.exe` binaries emit a dedicated marker describing the virtio-net interrupt mode/vectors:
`AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS/FAIL/SKIP|mode=intx/msi/msix/unknown|messages=<n>|config_vector=<n\|none>|rx_vector=<n\|none>|tx_vector=<n\|none>|...`.
If the virtio-net diag interface is unavailable or the IOCTL payload is missing/truncated, the marker is emitted as
`SKIP|reason=...|...` by default (or `FAIL|reason=...|...` when the guest is provisioned with `--require-net-msix`).

Newer virtio-net miniport builds may append additional diagnostic fields (best-effort), for example:
`flags=0x...|intr0=...|intr1=...|intr2=...|dpc0=...|dpc1=...|dpc2=...|rx_drained=...|tx_drained=...`.

When `--require-virtio-net-msix` is used, the **Python** harness additionally requires `mode=msix` from this marker.
The **PowerShell** harness does the same when `-RequireVirtioNetMsix` is set.

### virtio-snd MSI/MSI-X interrupt mode (guest-observed, optional)

Newer `aero-virtio-selftest.exe` binaries emit a dedicated marker describing the virtio-snd interrupt mode/vectors:

- PASS/FAIL (virtio-snd diag interface available):
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS/FAIL|mode=intx/msix/none/unknown|messages=<n>|config_vector=<n\|none>|queue0_vector=<n\|none>|queue1_vector=<n\|none>|queue2_vector=<n\|none>|queue3_vector=<n\|none>|interrupts=<n>|dpcs=<n>|drain0=<n>|drain1=<n>|drain2=<n>|drain3=<n>`
- SKIP/FAIL (virtio-snd diag interface unavailable / device missing):
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|SKIP/FAIL|reason=diag_unavailable|err=<n>`
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|SKIP/FAIL|reason=device_missing`

When the guest selftest is provisioned with `--require-snd-msix` (or env var `AERO_VIRTIO_SELFTEST_REQUIRE_SND_MSIX=1`),
it emits `FAIL` (instead of `SKIP`) when the diagnostics interface is unavailable, and emits `FAIL` when `mode!=msix`.

When `--require-virtio-snd-msix` is used, the **Python** harness additionally requires `mode=msix` from this marker.
The **PowerShell** harness does the same when `-RequireVirtioSndMsix` is set.

### virtio-input event delivery (QMP/HMP input injection)

The default virtio-input selftest (`virtio-input`) validates **enumeration + report descriptors** only.
To regression-test **actual input event delivery** (virtio queues → KMDF HID → user-mode `ReadFile`), the guest
includes an optional `virtio-input-events` section that reads real HID input reports.

To enable end-to-end testing:

1. Provision the guest image so the scheduled selftest runs with `--test-input-events`
   (for example via `New-AeroWin7TestImage.ps1 -TestInputEvents`).
2. Run the host harness with `-WithInputEvents` (alias: `-WithVirtioInputEvents`) / `--with-input-events`
   (aliases: `--with-virtio-input-events`, `--require-virtio-input-events`, `--enable-virtio-input-events`) so it injects keyboard/mouse events via QMP and requires the guest marker:
   `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...`

When enabled, the harness:

1. Waits for the guest readiness marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY`
2. Injects a deterministic input sequence via QMP:
    - keyboard: `'a'` press + release
    - mouse: relative move + left click
3. Requires the guest marker `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...`

The harness also emits a host-side marker for the injection step itself (useful for debugging flaky setups and for log
scraping in CI):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback>|kbd_mode=device/broadcast|mouse_mode=device/broadcast`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback|unknown>|reason=...`

Note: The harness may retry input injection a few times after the guest reports `virtio-input-events|READY` to reduce
timing flakiness (input reports can be dropped if no user-mode read is pending). In that case you may see multiple
`VIRTIO_INPUT_EVENTS_INJECT|PASS` lines (the marker includes `attempt=<n>` and `backend=...`).

#### QEMU feature requirements / fallback behavior

The harness prefers QMP `input-send-event` (with optional `device=` routing to the virtio input devices by QOM id).
If `input-send-event` is unavailable (QMP `CommandNotFound`), it falls back to older injection mechanisms:

- **Keyboard**
  - QMP `send-key` when available
  - otherwise HMP `sendkey <key>` via QMP `human-monitor-command`
- **Mouse (relative)**
  - HMP `mouse_move <dx> <dy>` and `mouse_button <state>` via QMP `human-monitor-command`
- **Device routing**
  - The legacy fallbacks are **broadcast-only** (no per-device targeting).

Note: On some QEMU builds, `input-send-event` may exist but reject the `device=` routing parameter. In that case the
harness falls back to broadcasting the input events and reports `kbd_mode=broadcast` / `mouse_mode=broadcast` in the marker.

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithInputEvents `
  -TimeoutSeconds 600
```


Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-input-events \
  --timeout-seconds 600 \
  --snapshot
```

#### Optional: scroll wheel + horizontal wheel (AC Pan)

To also regression-test **mouse scrolling** end-to-end (including **horizontal scrolling** via `REL_HWHEEL` /
HID Consumer `AC Pan`), run the harness with:

- PowerShell: `-WithInputWheel` (aliases: `-WithVirtioInputWheel`, `-EnableVirtioInputWheel`)
- Python: `--with-input-wheel` (aliases: `--with-virtio-input-wheel`, `--require-virtio-input-wheel`, `--enable-virtio-input-wheel`)

This:

- **implies** `-WithInputEvents` / `--with-input-events`
- injects wheel events via QMP `input-send-event` (rel axes `wheel`/`vscroll` + `hscroll`/`hwheel`, with best-effort fallbacks)
- requires the guest marker:
  `AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS|...`

Note: Like the base input-events injection, the harness may retry injection a few times after the guest reports
`virtio-input-events|READY` to reduce timing flakiness. In that case the guest may observe multiple injected wheel
events; the wheel selftest is designed to handle this (it validates expected per-event deltas, and totals may be
multiples of the injected values).

Note: Some QEMU builds use different axis names for scroll wheels (for example `vscroll` instead of `wheel`, and/or
`hwheel` instead of `hscroll`). The harness retries with best-effort fallback names; if none are accepted,
`-WithInputWheel` / `--with-input-wheel` (or aliases) fails with a clear error (upgrade QEMU or omit the wheel flag).

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithInputWheel `
  -TimeoutSeconds 600
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-input-wheel \
  --timeout-seconds 600 \
  --snapshot
```

Note: If the guest was provisioned without `--test-input-events`, it will emit:
`AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set`.
The host harness only requires `virtio-input-events|PASS` when `-WithInputEvents` / `--with-input-events` is set. When
enabled, a guest `SKIP|flag_not_set` causes a hard failure (PowerShell: `VIRTIO_INPUT_EVENTS_SKIPPED`; Python:
`FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: ...`).

If the guest reports `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|...`, the harness fails
(PowerShell: `VIRTIO_INPUT_EVENTS_FAILED`; Python: `FAIL: VIRTIO_INPUT_EVENTS_FAILED: ...`).

Note: If the guest selftest is too old (or otherwise misconfigured) and does not emit any `virtio-input-events`
marker at all (READY/SKIP/PASS/FAIL) after completing `virtio-input`, the harness fails early
(PowerShell: `MISSING_VIRTIO_INPUT_EVENTS`; Python: `FAIL: MISSING_VIRTIO_INPUT_EVENTS: ...`). Update/re-provision the guest selftest binary.

If input injection fails (for example QMP is unreachable or the QEMU build does not support any supported injection mechanism),
the harness fails (PowerShell: `QMP_INPUT_INJECT_FAILED`; Python: `FAIL: QMP_INPUT_INJECT_FAILED: ...`).

#### Optional: Consumer Control (media keys)

To regression-test **Consumer Control / media keys** end-to-end (virtio-input event → KMDF HID → user-mode HID report read),
run the harness with:

- PowerShell: `-WithInputMediaKeys` (aliases: `-WithVirtioInputMediaKeys`, `-EnableVirtioInputMediaKeys`)
- Python: `--with-input-media-keys` (aliases: `--with-virtio-input-media-keys`, `--require-virtio-input-media-keys`, `--enable-virtio-input-media-keys`)

Guest image requirement:

- Provision the guest selftest to run with `--test-input-media-keys` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_MEDIA_KEYS=1`).
  One way to bake this into the scheduled task is `New-AeroWin7TestImage.ps1 -TestInputMediaKeys` (alias: `-TestMediaKeys`).

When enabled, the harness:

1. Waits for the guest readiness marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|READY`
2. Injects a deterministic media-key sequence via QMP (prefers `input-send-event`, with backcompat fallbacks when unavailable):
   - `qcode=volumeup` press + release
3. Requires the guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS|...`

The harness also emits a host-side marker for the injection step (useful for debugging/log scraping):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback>|kbd_mode=device/broadcast`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback|unknown>|reason=...`

If QMP injection fails (for example the QEMU build does not support multimedia qcodes), the harness fails with a clear token:

- PowerShell: `QMP_MEDIA_KEYS_UNSUPPORTED`
- Python: `FAIL: QMP_MEDIA_KEYS_UNSUPPORTED: ...`

#### Optional: extended virtio-input events (modifiers/buttons/wheel)

The base `virtio-input-events` marker validates that basic keyboard + mouse motion/click reports are delivered end-to-end.
To also validate additional HID report paths deterministically, the guest selftest can emit three extra markers:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS|...` (Shift/Ctrl/Alt + F1)
- `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS|...` (mouse side/extra buttons)
- `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS|...` (wheel + horizontal wheel)

To enable and require these extended markers, run the harness with:

- PowerShell: `-WithInputEventsExtended` (alias: `-WithInputEventsExtra`)
- Python: `--with-input-events-extended` (alias: `--with-input-events-extra`)

This:

- **implies** `-WithInputEvents` / `--with-input-events`
- injects an extended deterministic sequence via QMP `input-send-event`
- requires all three `virtio-input-events-*` markers above to PASS
- requires the guest selftest to be provisioned with `--test-input-events-extended`
  (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1`)
  - If provisioning via `New-AeroWin7TestImage.ps1`, pass `-TestInputEventsExtended` (alias: `-TestInputEventsExtra`).
  - The harness fails if the base `virtio-input-events` marker is skipped/missing

Note: This is separate from `-WithInputWheel` / `--with-input-wheel`, which instead requires the aggregate marker
`AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS|...`. The extended wheel marker (`virtio-input-events-wheel`) covers
wheel + horizontal wheel as part of the extended flow.

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithInputEventsExtended `
  -TimeoutSeconds 600
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-input-events-extended \
  --timeout-seconds 600 \
  --snapshot
```

### virtio-input keyboard LED/statusq (HID output reports)

virtio-input also has an **output** path used for keyboard LED state (CapsLock/NumLock/etc). In the Aero Win7 virtio-input
stack this is transported over the virtio-input **status queue** (statusq).

To regression-test this path end-to-end (user-mode HID `WriteFile` → KMDF HID minidriver → virtio statusq → device
consumes/completes), the guest selftest includes an optional `virtio-input-led` section (compat: `virtio-input-leds`).

The guest selftest emits **both** markers (so either harness spelling can gate it):

- `AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS|...` (newer)
- `AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS|writes=<n>` (compat)

To enable the check:

1. Provision the guest image so the scheduled selftest runs with `--test-input-led`
   (for example via `New-AeroWin7TestImage.ps1 -TestInputLed`, or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_LED=1`).
   - Compatibility: `--test-input-leds` / `-TestInputLeds` / env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_LEDS=1`.
2. Run the host harness with `-WithInputLed` / `--with-input-led`
   (aliases: `--with-virtio-input-led`, `--require-virtio-input-led`, `--enable-virtio-input-led`) so it requires the guest marker:
   `AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS`.
   - Compatibility: `-WithInputLeds` / `--with-input-leds`
     (aliases: `--with-virtio-input-leds`, `--require-virtio-input-leds`, `--enable-virtio-input-leds`) instead requires
     `AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS|writes=<n>`.

When enabled, a guest `virtio-input-led|SKIP|flag_not_set` / `virtio-input-leds|SKIP|flag_not_set` causes a hard failure:

- When gating `virtio-input-led` (`-WithInputLed` / `--with-input-led`):
  - PowerShell: `VIRTIO_INPUT_LED_SKIPPED`
  - Python: `FAIL: VIRTIO_INPUT_LED_SKIPPED: ...`
- When gating `virtio-input-leds` (`-WithInputLeds` / `--with-input-leds`):
  - PowerShell: `VIRTIO_INPUT_LEDS_SKIPPED`
  - Python: `FAIL: VIRTIO_INPUT_LEDS_SKIPPED: ...`

If the guest emits no marker at all after `virtio-input` completes, the harness fails early:

- `-WithInputLed` / `--with-input-led`: `MISSING_VIRTIO_INPUT_LED`
- `-WithInputLeds` / `--with-input-leds`: `MISSING_VIRTIO_INPUT_LEDS`
### virtio-input tablet (absolute pointer) event delivery (QMP input injection)

When a virtio tablet device (`virtio-tablet-pci`) is attached, the guest selftest can optionally validate **absolute
pointer** report delivery end-to-end (virtio queues → KMDF HID → user-mode `ReadFile`) via the marker:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|...`

To enable end-to-end testing:

1. Provision the guest image so the scheduled selftest runs with `--test-input-tablet-events`
   (alias: `--test-tablet-events`; for example via `New-AeroWin7TestImage.ps1 -TestInputTabletEvents` / `-TestTabletEvents`,
   or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1` / `AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1`).
    - Note: This requires that the virtio-input driver is installed and that the tablet device is bound so it exposes a
      HID interface.
      - For an **Aero contract tablet** (HWID `...&SUBSYS_00121AF4&REV_01`), the intended INF is
        `drivers/windows7/virtio-input/inf/aero_virtio_tablet.inf`.
      - `aero_virtio_tablet.inf` is the preferred binding for the contract tablet HWID and wins when it matches (it is a
        more specific match than the opt-in strict generic fallback HWID (`PCI\VEN_1AF4&DEV_1052&REV_01`).
      - If your QEMU/device does **not** expose the Aero contract tablet subsystem ID (`SUBSYS_0012`), `aero_virtio_tablet.inf`
        will not match (tablet-SUBSYS-only).
        - In that case, the device can still bind via the opt-in strict revision-gated generic fallback HWID
          (`PCI\VEN_1AF4&DEV_1052&REV_01`) if you enable the optional legacy alias INF (the `*.inf.disabled` file under
          `drivers/windows7/virtio-input/inf/`), as long as the device reports `REV_01` (for QEMU, ensure
          `x-pci-revision=0x01` is in effect; the harness does this by default).
        - When binding via the generic fallback entry, Device Manager will show the generic **Aero VirtIO Input Device**
          name.
      - Preferred (contract) path: adjust/emulate the subsystem IDs to the contract values (so the tablet enumerates as
        `...&SUBSYS_00121AF4&REV_01` and binds via `aero_virtio_tablet.inf`).
      - If you want to exercise the contract tablet binding specifically, ensure the device exposes the tablet subsystem ID
        (`...&SUBSYS_00121AF4&REV_01`) so `aero_virtio_tablet.inf` can win over the generic fallback match.
      - The `*.inf.disabled` file under `drivers/windows7/virtio-input/inf/` is the optional legacy alias INF used to opt into
        the strict generic fallback match. Enabling it does change HWID matching behavior.
      - Once bound, the driver classifies the device as a tablet via `EV_BITS` (`EV_ABS` + `ABS_X`/`ABS_Y`).
    - When provisioning via `New-AeroWin7TestImage.ps1`, the tablet INF is installed by default when present; if you pass
      an explicit `-InfAllowList`, ensure it includes `aero_virtio_input.inf` (and `aero_virtio_tablet.inf` if you want
        to exercise the contract tablet binding specifically / validate tablet-specific INF matching).
      - If you are intentionally using the legacy alias filename (for compatibility with tooling that looks for it), include
        the enabled alias INF (drop the `.disabled` suffix) instead of `aero_virtio_input.inf`. Note: the alias adds the
        strict generic fallback model line, so it does change HWID matching behavior.
2. Run the host harness with `-WithInputTabletEvents` (aliases: `-WithVirtioInputTabletEvents`, `-EnableVirtioInputTabletEvents`,
         `-WithTabletEvents`, `-EnableTabletEvents`) /
         `--with-input-tablet-events` (aliases: `--with-virtio-input-tablet-events`, `--with-tablet-events`,
        `--enable-virtio-input-tablet-events`, `--require-virtio-input-tablet-events`) so it:
    - attaches `virtio-tablet-pci`
    - injects a deterministic absolute-pointer sequence via QMP `input-send-event`
    - requires the guest marker to PASS
 
To attach `virtio-tablet-pci` **without** QMP injection / marker enforcement (for example to just validate device
enumeration), use:
 
- PowerShell: `-WithVirtioTablet`
- Python: `--with-virtio-tablet`

The injected sequence is:

- move to (0,0) (reset)
- move to (10000,20000) (target)
- left click down + up

The harness also emits a host-side marker for each injection attempt:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt=<n>|backend=<qmp_input_send_event>|tablet_mode=device/broadcast`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=<n>|backend=<qmp_input_send_event|unknown>|reason=...`

Note: Unlike the keyboard/mouse path above, **tablet (absolute) injection requires QMP `input-send-event`**. There is
no widely-supported legacy fallback for absolute pointer injection; if `input-send-event` is missing the harness will
fail with a clear reason.

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithTabletEvents `
  -TimeoutSeconds 600
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-tablet-events \
  --timeout-seconds 600 \
  --snapshot
```

Note: If the guest was provisioned without `--test-input-tablet-events` / `--test-tablet-events`, it will emit:
`AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set`.
When `-WithInputTabletEvents` / `-WithTabletEvents` / `--with-input-tablet-events` / `--with-tablet-events` is enabled,
the host harness treats this as a hard failure.

### virtio-blk runtime resize (QMP block resize)

The guest selftest includes an opt-in `virtio-blk-resize` test that validates **dynamic capacity change** support:
Windows should observe a larger disk size after the host resizes the backing device at runtime.

To enable end-to-end testing:

1. Provision the guest image so the scheduled selftest runs with `--test-blk-resize`
    (or set env var `AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE=1` in the guest).
   - When generating provisioning media with `New-AeroWin7TestImage.ps1`, you can bake this in via:
     `-TestBlkResize` (adds `--test-blk-resize` to the scheduled task).
2. Run the host harness with blk-resize enabled so it:
    - PowerShell: `-WithBlkResize`
    - Python: `--with-blk-resize` (aliases: `--with-virtio-blk-resize`, `--require-virtio-blk-resize`, `--enable-virtio-blk-resize`)
    - waits for `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>`
    - computes `new_bytes = old_bytes + delta` (default delta: 64 MiB)
    - issues a QMP resize (`blockdev-resize` with a fallback to legacy `block_resize`)
    - requires `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|...`

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -WithBlkResize `
  -BlkResizeDeltaMiB 64 `
  -TimeoutSeconds 600
```

Example (Python):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-blk-resize \
  --blk-resize-delta-mib 64 \
  --timeout-seconds 600 \
  --snapshot
```

When enabled, the harness also prints a host-side marker for log scraping/debugging:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|REQUEST|old_bytes=...|new_bytes=...|qmp_cmd=...`

On QMP resize failure, it emits:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|reason=...|old_bytes=...|new_bytes=...|drive_id=...`

It also mirrors the final guest result marker into a stable host marker:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|PASS/FAIL/SKIP/READY|...`

### virtio-blk reset (guest IOCTL; optional)

The guest selftest includes an opt-in `virtio-blk-reset` test that validates the **miniport reset/recovery** path:
it issues `AEROVBLK_IOCTL_FORCE_RESET` and then verifies the disk stack recovers (smoke I/O + post-reset miniport query).

To enable end-to-end testing:

1. Provision the guest image so the scheduled selftest runs with `--test-blk-reset`
   (or set env var `AERO_VIRTIO_SELFTEST_TEST_BLK_RESET=1` in the guest).
   - When generating provisioning media with `New-AeroWin7TestImage.ps1`, bake this in via:
      `-TestBlkReset` (adds `--test-blk-reset` to the scheduled task).
2. Run the host harness with blk-reset gating enabled so it requires the guest marker to PASS
   (and treats SKIP/FAIL/missing as failure):
   - PowerShell: `-WithBlkReset`
   - Python: `--with-blk-reset` (aliases: `--with-virtio-blk-reset`, `--require-virtio-blk-reset`, `--enable-virtio-blk-reset`)

The guest emits one of:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=...|counter_after=...`
- `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=flag_not_set` (test not enabled / guest not provisioned; older selftests may emit `...|SKIP|flag_not_set`)
- `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported` (miniport does not support the reset IOCTL; older selftests may emit `...|SKIP|not_supported`)
- `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=...|err=...` (older selftests may emit `...|FAIL|<reason>|err=...` with no `reason=` field)

The harness also mirrors the final guest result marker into a stable host marker for log scraping/debugging:

`AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|PASS/FAIL/SKIP|...`

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -WithBlkReset `
  -TimeoutSeconds 600
```

Example (Python):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --with-blk-reset \
  --timeout-seconds 600
```

### virtio-net link flap (QMP `set_link`; optional)

The guest selftest includes an opt-in `virtio-net-link-flap` test that validates link state transitions.

When enabled, the host harness:

1. Waits for the guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY`
2. Uses QMP `set_link` to toggle the virtio-net device link **down**, waits a short delay (currently 3 seconds), then
   toggles it **up**
3. Requires the guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS`
4. Emits a host-side marker for log scraping/debugging:
   - PASS: `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|PASS|name=...|down_delay_sec=3`
   - FAIL: `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|FAIL|name=...|down_delay_sec=3|reason=...`

To enable end-to-end testing:

1. Provision the guest image so the scheduled selftest runs with `--test-net-link-flap`
   (or set env var `AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP=1` in the guest).
   - When generating provisioning media with `New-AeroWin7TestImage.ps1`, bake this in via:
     `-TestNetLinkFlap` (adds `--test-net-link-flap` to the scheduled task).
2. Run the host harness with link flap enabled.

Example (PowerShell):

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -WithNetLinkFlap `
  -TimeoutSeconds 600
```

Example (Python):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --with-net-link-flap \
  --timeout-seconds 600
```
### virtio-snd (audio)

If your test image includes the virtio-snd driver, you can ask the harness to attach a virtio-snd PCI device:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithVirtioSnd `
  -TimeoutSeconds 600
```

The harness uses QEMU’s `-audiodev none,...` backend so it remains headless/CI-friendly.

Note: When `-WithVirtioSnd` / `--with-virtio-snd` is enabled (aliases: `-RequireVirtioSnd`, `-EnableVirtioSnd`;
`--require-virtio-snd`, `--enable-virtio-snd`), the host harness expects the guest selftest to run:

- virtio-snd playback (`AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS`)
- virtio-snd capture endpoint checks (`...|virtio-snd-capture|PASS`)
- virtio-snd full-duplex regression (`...|virtio-snd-duplex|PASS`)

The duplex test runs whenever the guest selftest runs the virtio-snd capture smoke test:
  - by default when a virtio-snd device is present (newer selftest binaries), or
  - when forced via `--test-snd-capture` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`).

If your guest selftest is older (or otherwise not configured to run capture smoke testing), it may emit
`virtio-snd-duplex|SKIP|flag_not_set`; in that case the host harness fails with a `...DUPLEX_SKIPPED` reason.
Provision the guest with `--test-snd-capture` / env var `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`.

#### virtio-snd buffer limits stress test

The guest selftest includes an opt-in WASAPI buffer sizing stress test (`virtio-snd-buffer-limits`).
This is disabled by default and must be enabled in the guest command line.

To enable it end-to-end:

1. Provision the guest scheduled task with `--test-snd-buffer-limits` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`,
   for example via `New-AeroWin7TestImage.ps1 -TestSndBufferLimits`).
2. Run the host harness with:
   - PowerShell: `-WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd -WithSndBufferLimits`
   - Python: `--with-virtio-snd/--require-virtio-snd/--enable-virtio-snd --with-snd-buffer-limits`
     (aliases for `--with-snd-buffer-limits`: `--with-virtio-snd-buffer-limits`, `--require-virtio-snd-buffer-limits`,
     `--enable-snd-buffer-limits`, `--enable-virtio-snd-buffer-limits`)

When `-WithSndBufferLimits` / `--with-snd-buffer-limits` is enabled, the harness requires the guest marker:
`AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS|...`.
If the guest reports `SKIP|flag_not_set` (or does not emit the marker at all), the harness treats it as a hard failure
so older selftest binaries or mis-provisioned images cannot accidentally pass.

The harness also emits a stable host-side marker for log scraping:
`AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|PASS/FAIL/SKIP|...`.

On success, the script returns exit code `0` and prints:

```
PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS
```

On failure/timeout, it returns non-zero and prints the matching failure reason.
If QEMU exits early (for example due to an unsupported device property like `disable-legacy` / `x-pci-revision`),
the harness captures QEMU stderr to a sidecar log next to the serial log:

- `<serial-base>.qemu.stderr.log`

For live debugging, you can stream the guest serial output while waiting:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -FollowSerial
```

### Python alternative (Linux-friendly)

If you prefer not to depend on PowerShell, `invoke_aero_virtio_win7_tests.py` provides the same core behavior:

Both harnesses can optionally record per-request HTTP logs (useful for CI artifacts):

- PowerShell: `-HttpLogPath ./win7-http.log`
- Python: `--http-log ./win7-http.log`

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --serial-log ./win7-serial.log \
  --http-log ./win7-http.log \
  --timeout-seconds 600 \
  --snapshot
```

Add `--follow-serial` to stream COM1 serial output while waiting.

### Host-harness unit tests (Python)

The Python harness includes a small unit test suite under `host-harness/tests/` that validates helper logic
without needing QEMU or a guest image (QMP command formatting, deterministic HTTP payloads, wav parsing, etc.).

From the repo root:

```bash
python3 -m unittest discover -s drivers/windows7/tests/host-harness/tests
```

### virtio-snd (audio) device

To attach a virtio-snd device (virtio-sound-pci) during the run, enable it explicitly with:

- PowerShell: `-WithVirtioSnd` (aliases: `-RequireVirtioSnd`, `-EnableVirtioSnd`)
- Python: `--with-virtio-snd` (aliases: `--require-virtio-snd`, `--enable-virtio-snd`)

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithVirtioSnd `
  -VirtioSndAudioBackend none
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-virtio-snd \
  --virtio-snd-audio-backend none
```

#### Wav capture (deterministic)

To capture guest audio output deterministically, use the `wav` backend and provide a path:

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithVirtioSnd `
  -VirtioSndAudioBackend wav `
  -VirtioSndWavPath ./out/virtio-snd.wav
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --with-virtio-snd \
  --virtio-snd-audio-backend wav \
  --virtio-snd-wav-path ./out/virtio-snd.wav
```

#### Host-side wav verification (non-silence)

Guest-side WaveOut success only proves Windows accepted the audio buffer; it does **not** guarantee the virtio-snd driver
actually fed the host audio backend. When using the `wav` backend, the harness can validate that the captured PCM data is
non-silent.

PowerShell:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithVirtioSnd `
  -VirtioSndAudioBackend wav `
  -VirtioSndWavPath ./out/virtio-snd.wav `
  -VerifyVirtioSndWav
```

Python:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --enable-virtio-snd \
  --virtio-snd-audio-backend wav \
  --virtio-snd-wav-path ./out/virtio-snd.wav \
  --virtio-snd-verify-wav
```

Notes:

- Verification requires the **guest virtio-snd driver** to be installed, and the guest selftest must not skip virtio-snd
  via `--disable-snd`. (When a virtio-snd PCI device is present, the selftest runs playback automatically.)
- Ensure the guest virtio-snd driver is not running with the silent null backend enabled (`ForceNullBackend=1`), or wav
  verification will observe silence. Clear it under the virtio-snd device instance registry key:
  - `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend` = `0` (`REG_DWORD`)
  - Find `<DeviceInstancePath>` via Device Manager → Details → “Device instance path”.
  - When `ForceNullBackend=1` is set, the host harness emits a deterministic failure token:
    `FAIL: VIRTIO_SND_FORCE_NULL_BACKEND: ...`
  - Note: the virtio-snd INFs seed this value with `FLG_ADDREG_NOCLOBBER`, so driver reinstall/upgrade does **not**
    reset it; clear it explicitly when done debugging.
- The harness attempts to shut QEMU down **gracefully** (via QMP) so the `wav` audio backend can flush/finalize the RIFF
  header before verification. If QEMU is killed hard, the `data` chunk size may be left as a placeholder (often `0`), and
  verification may fail or need to fall back to best-effort recovery.
- The harness prints a single-line marker suitable for log scraping:
  `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_WAV|PASS|...` or `...|FAIL|reason=<...>`.

## Running in GitHub Actions (self-hosted)

This repo includes an **opt-in** workflow for running the host harness end-to-end under QEMU on a **self-hosted** runner:

- Workflow: [`.github/workflows/win7-virtio-harness.yml`](../../../../.github/workflows/win7-virtio-harness.yml)
- Trigger: `workflow_dispatch` only (no automatic PR runs)
- Runner label: `aero-win7-harness`
- Logs artifact: `win7-virtio-harness-logs` (serial + harness output + HTTP request log; QEMU stderr sidecar (placeholder when not produced); empty serial/HTTP logs are replaced with a placeholder line)
- Job summary: surfaces key PASS/FAIL markers (including guest `virtio-blk-counters` and legacy virtio-blk miniport diag lines), QEMU stderr/argv, and (on failure) collapsible tails of the logs above

### Runner setup

On the self-hosted runner you need:

- QEMU available on `PATH` (or pass the absolute path via the workflow input `qemu_system`)
- Python 3 (`python3`)
- a prepared Win7 disk image available at a stable path on the runner (pass via the workflow input `disk_image_path`)

> Note: The harness uses fixed localhost ports by default (HTTP `18080`, UDP `18081`). The workflow enforces
> `concurrency.group: win7-virtio-harness` to prevent concurrent runs from fighting over ports/images.
> If either port is already in use on your runner, override them via the workflow inputs `http_port` / `udp_port`.
> If you override `udp_port`, provision the guest scheduled task with the same `--udp-port` (for example via
> `New-AeroWin7TestImage.ps1 -UdpPort <port>`).
> To run against older guest selftest binaries that do not implement the UDP test marker, set `disable_udp=true`.

By default the workflow runs QEMU with `memory_mb=2048` and `smp=2`. Override these via the workflow inputs if your
runner has different resource constraints.

If your guest image was provisioned with a non-default `--http-path`, override it via the workflow input `http_path`
(the default matches the harness default `/aero-virtio-selftest`). This must start with `/` and must not contain
whitespace.

For debugging, you can pass extra QEMU args through the workflow input `qemu_extra_args` (one argument per line).
These are forwarded to QEMU **after `--`**, so they are not parsed as harness flags. The workflow trims leading/trailing
whitespace; blank lines and lines starting with `#` are ignored.

To enable the optional host-side QEMU PCI ID preflight (`query-pci` via QMP), set the workflow input
`qemu_preflight_pci=true`. This helps catch missing/ignored `x-pci-revision=0x01` (REV_01) configuration early.

To print the computed QEMU argv (JSON + best-effort single-line command) without starting QEMU/HTTP/QMP, set the workflow
input `dry_run=true`. This passes `--dry-run` to the Python harness and exits 0 after printing. In this mode, the
workflow does not require the disk image path to exist (it prints a warning instead), and it does not require QEMU to be
installed (it prints a warning and reports `qemu: (not found)`), which is useful when you just want to inspect the
generated QEMU args. You can also leave `disk_image_path` empty in `dry_run` mode; the workflow will create a temporary
placeholder file under the logs directory and use that path for argument generation.
The workflow job summary also embeds the dry-run QEMU argv JSON and copy/paste command for convenience.

To force INTx-only mode (disable MSI-X by passing `vectors=0` on the virtio devices), set the workflow input
`force_intx=true`. This passes `--force-intx` (alias: `--virtio-disable-msix`) to the Python harness. Note: some QEMU
builds may reject `vectors=0`; in that case the harness will fail fast with the QEMU error in the stderr sidecar log.

To require the guest virtio drivers to use a specific interrupt family, set either:

- `require_intx=true` (passes `--require-intx`), or
- `require_msi=true` (passes `--require-msi`; accepts both MSI and MSI-X as “MSI family”).

These inputs are mutually exclusive. Note: `force_intx=true` is incompatible with `require_msi=true` (the workflow will
fail fast before launching QEMU).

To opt back into QEMU’s transitional virtio-pci devices (legacy + modern) for backcompat with older QEMU builds/guest
images, set the workflow input `virtio_transitional=true` (passes `--virtio-transitional`).

Notes:

- This is incompatible with `with_virtio_snd=true` (virtio-snd testing requires the modern-only contract-v1 layout).
- This is incompatible with `with_blk_resize=true` (blk resize uses the contract-v1 drive layout with stable IDs).

To request a specific MSI-X vector table size from QEMU (`vectors=N`), set:

- `virtio_msix_vectors=<N>` for a global default, and/or per-device overrides:
  - `virtio_net_vectors=<N>`
  - `virtio_blk_vectors=<N>`
  - `virtio_input_vectors=<N>`
  - `virtio_snd_vectors=<N>` (requires `with_virtio_snd=true`)

These inputs require `N > 0` and are mutually exclusive with `force_intx=true` (which forces `vectors=0`).

To require that MSI-X is enabled/effective for a given device (fail when the device is not operating in MSI-X mode),
set the corresponding workflow input (passes the same-named `--require-virtio-*-msix` flag to the Python harness):

- `require_virtio_net_msix=true`
- `require_virtio_blk_msix=true`
- `require_virtio_input_msix=true`
- `require_virtio_snd_msix=true` (requires `with_virtio_snd=true`)

These inputs are incompatible with `force_intx=true` and with `require_intx=true`.

The workflow can also optionally exercise the end-to-end virtio-input event delivery path (guest HID report verification +
host-side input injection via QMP/HMP) by setting the workflow input `with_virtio_input_events=true`.
This requires a guest image provisioned with `--test-input-events` (for example via
`New-AeroWin7TestImage.ps1 -TestInputEvents`) so the guest selftest enables the `virtio-input-events` read loop.

To also exercise the optional virtio-input wheel marker (`virtio-input-wheel`), set the workflow input
`with_virtio_input_wheel=true` (requires the same guest `--test-input-events` provisioning).

To also exercise the optional virtio-input Consumer Control / media keys marker (`virtio-input-media-keys`), set the workflow input
`with_virtio_input_media_keys=true`. This requires a guest image provisioned with `--test-input-media-keys` (for example via
`New-AeroWin7TestImage.ps1 -TestInputMediaKeys` (alias: `-TestMediaKeys`)).

To also require the optional virtio-input LED/statusq marker (`virtio-input-led`), set the workflow input
`with_virtio_input_led=true`. This requires a guest image provisioned with `--test-input-led` (for example via
`New-AeroWin7TestImage.ps1 -TestInputLed`).

For compatibility with older automation that gates on the legacy `virtio-input-leds` marker, set the workflow input
`with_virtio_input_leds=true`. This passes `--with-input-leds` to the Python harness and requires the guest marker
`AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS|writes=<n>`.

- Provisioning: older guest images require `--test-input-leds` / `New-AeroWin7TestImage.ps1 -TestInputLeds`. Newer guest
  selftest binaries also accept `--test-input-led` and still emit the legacy marker for compatibility.
  - Note: if both `with_virtio_input_led=true` and `with_virtio_input_leds=true` are set, the workflow prefers the legacy
    `--with-input-leds` path.

To require virtio-input PCI binding/service validation (at least one virtio-input PCI device bound to the expected
driver service), set the workflow input `require_virtio_input_binding=true`. This requires a guest selftest new enough
to emit the marker `AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|...` (older images may fail with
`MISSING_VIRTIO_INPUT_BINDING`).

To exercise the extended virtio-input markers (`virtio-input-events-modifiers/buttons/wheel`), set the workflow input
`with_virtio_input_events_extended=true`. This requires a guest image provisioned with `--test-input-events` and
`--test-input-events-extended` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1`; for example via
`New-AeroWin7TestImage.ps1 -TestInputEventsExtended` (alias: `-TestInputEventsExtra`)).

To also exercise the virtio-input tablet (absolute pointer) end-to-end path, set the workflow input
`with_virtio_input_tablet_events=true`. This requires a guest image provisioned with `--test-input-tablet-events`
(alias: `--test-tablet-events`) so the guest selftest enables the `virtio-input-tablet-events` read loop.

To attach a virtio-tablet device without enabling the tablet events selftest/injection path, set the workflow input
`with_virtio_tablet=true`. This passes `--with-virtio-tablet` to the Python harness and attaches `virtio-tablet-pci`
in addition to the virtio keyboard/mouse devices. Ensure the guest tablet driver is installed:

- Preferred/contract binding: `aero_virtio_tablet.inf` for `...&SUBSYS_00121AF4&REV_01` (wins when it matches).
- If the device does not expose the Aero contract tablet subsystem ID (`SUBSYS_0012`), `aero_virtio_tablet.inf` will not match (tablet-SUBSYS-only).
  In that case, it can still bind via the opt-in strict fallback HWID (`PCI\VEN_1AF4&DEV_1052&REV_01`, no `SUBSYS`) if you
  enable the optional legacy alias INF (the `*.inf.disabled` file under `drivers/windows7/virtio-input/inf/`). It will appear as
  the generic **Aero VirtIO Input Device**.
  Ensure the device reports `REV_01` (for QEMU, ensure `x-pci-revision=0x01` is in effect; the harness does this by default).

To exercise the optional virtio-blk runtime resize test (`virtio-blk-resize`), set the workflow input
`with_blk_resize=true`. This triggers a host-side QMP resize (`blockdev-resize` with a fallback to legacy `block_resize`)
after the guest emits the readiness marker (`...|virtio-blk-resize|READY|...`), and requires the guest marker
`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|...`.

- The guest image must be provisioned with `--test-blk-resize` (or env var `AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE=1`),
  for example via `New-AeroWin7TestImage.ps1 -TestBlkResize`.
- Optional: set `blk_resize_delta_mib=<N>` to override the default growth delta (MiB).

To ensure you are using a guest image provisioned with the virtio-blk MSI/MSI-X expectation gate, set the workflow input
`require_expect_blk_msi=true` (passes `--require-expect-blk-msi`). This fails if the guest CONFIG marker does not include
`expect_blk_msi=1` (i.e. the image was not provisioned with `aero-virtio-selftest.exe --expect-blk-msi`).

To fail when the guest virtio-blk driver reports StorPort recovery/reset/abort activity (best-effort; requires the guest
to emit recovery counters markers), set either:

- `require_no_blk_recovery=true` (passes `--require-no-blk-recovery`), or
- `fail_on_blk_recovery=true` (passes `--fail-on-blk-recovery`, a narrower subset).

To fail when the guest virtio-blk reset recovery diagnostic marker reports reset activity (best-effort; requires the guest
to emit `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|...`), set either:

- `require_no_blk_reset_recovery=true` (passes `--require-no-blk-reset-recovery`), or
- `fail_on_blk_reset_recovery=true` (passes `--fail-on-blk-reset-recovery`, a narrower subset).

To fail when the guest virtio-blk miniport flags diagnostic marker reports removal/reset activity (best-effort; ignores
missing/WARN markers and requires the guest to emit `...|virtio-blk-miniport-flags|INFO|...`), set either:

- `require_no_blk_miniport_flags=true` (passes `--require-no-blk-miniport-flags`), or
- `fail_on_blk_miniport_flags=true` (passes `--fail-on-blk-miniport-flags`, a narrower subset).

To require the optional virtio-blk reset test (`virtio-blk-reset`), set the workflow input `with_blk_reset=true`.
This requires a guest image provisioned with `--test-blk-reset` (or env var `AERO_VIRTIO_SELFTEST_TEST_BLK_RESET=1`),
for example via `New-AeroWin7TestImage.ps1 -TestBlkReset`.

To exercise the optional virtio-net link flap test (`virtio-net-link-flap`), set the workflow input
`with_net_link_flap=true`. This triggers a host-side QMP `set_link` down/up sequence after the guest emits the readiness
marker (`...|virtio-net-link-flap|READY|...`) and requires the guest marker
`AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS|...`.

- The guest image must be provisioned with `--test-net-link-flap` (or env var `AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP=1`),
  for example via `New-AeroWin7TestImage.ps1 -TestNetLinkFlap`.

To require virtio-net checksum offload to be exercised (at least one TX packet with checksum offload enabled),
set the workflow input `require_net_csum_offload=true`. This checks the guest marker
`AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=...` and fails if it is missing/FAIL or reports
`tx_csum=0`.

To attach virtio-snd and require the guest virtio-snd playback/capture/duplex tests to PASS, set the workflow input
`with_virtio_snd=true`. By default the workflow uses the `wav` backend (`virtio_snd_audio_backend=wav`), uploads
`virtio-snd.wav`, and (by default) verifies the captured audio is non-silent (`virtio_snd_verify_wav=true`).

To override the non-silence thresholds used by wav verification, set either (non-negative integers; requires
`with_virtio_snd=true`):

- `virtio_snd_wav_peak_threshold=<N>`
- `virtio_snd_wav_rms_threshold=<N>`

To require the virtio-snd buffer limits stress test (`virtio-snd-buffer-limits`), set the workflow input
`with_snd_buffer_limits=true` (requires `with_virtio_snd=true` and a guest image provisioned with `--test-snd-buffer-limits`
or env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`,
for example via `New-AeroWin7TestImage.ps1 -TestSndBufferLimits`).

To exercise the end-to-end virtio-net link flap regression test (QMP `set_link`), set the workflow input
`with_net_link_flap=true`. This requires a guest image provisioned with `--test-net-link-flap` (or env var
`AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP=1`) and a QEMU build that supports QMP `set_link`.

### Invoking the workflow

1. Place your prepared Windows 7 image somewhere on the runner (example: `/var/lib/aero/win7/win7-aero-tests.qcow2`).
2. In GitHub, go to **Actions** → **Win7 virtio host harness (self-hosted)** → **Run workflow**.
3. Set `disk_image_path` to the runner-local path above.

The default workflow settings run QEMU in snapshot mode (disk writes are discarded). Disable `snapshot`
only if you explicitly want the base image to be mutated.

## How the harness works

- Starts a tiny HTTP server on `127.0.0.1:<HttpPort>`
  - QEMU slirp/user networking exposes host as `10.0.2.2` inside the guest, so the guest can HTTP GET `http://10.0.2.2:<HttpPort><HttpPath>`.
  - The harness also serves a deterministic large payload at `http://10.0.2.2:<HttpPort><HttpPath>-large`:
    - HTTP 200
    - body size: **1 MiB**
    - bytes: repeating `0..255` pattern
    - includes a correct `Content-Length`
    - includes `ETag: "8505ae4435522325"` (FNV-1a 64-bit of the payload) and `Cache-Control: no-store`
    - used by the guest virtio-net selftest to stress sustained RX and validate data integrity (size + hash)
    - the same `...-large` endpoint also accepts a deterministic 1 MiB HTTP POST upload and validates integrity
      (stresses sustained TX)
- Starts a tiny UDP echo server on `127.0.0.1:<UdpPort>`
  - QEMU slirp/user networking exposes host as `10.0.2.2` inside the guest, so the guest can send a UDP datagram to
    `10.0.2.2:<UdpPort>` and expect an identical echo reply.
  - Note: This port must match the guest selftest's `--udp-port` (default: `18081`). If you override the host harness UDP
    port, provision the guest scheduled task with the same value (for example via `New-AeroWin7TestImage.ps1 -UdpPort <port>`).
- Launches QEMU with:
  - `-chardev file,...` + `-serial chardev:...` (guest COM1 → host log)
  - `virtio-net-pci,id=aero_virtio_net0,disable-legacy=on,x-pci-revision=0x01` with `-netdev user`
    (modern-only; enumerates as `PCI\VEN_1AF4&DEV_1041&REV_01`; stable `id=` enables QMP `set_link`)
  - `virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01` + `virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01` (virtio-input; modern-only; enumerates as `PCI\VEN_1AF4&DEV_1052&REV_01`)
  - `-drive if=none,id=drive0` + `virtio-blk-pci,drive=drive0,disable-legacy=on,x-pci-revision=0x01` (modern-only; enumerates as `PCI\VEN_1AF4&DEV_1042&REV_01`)
  - (optional) `virtio-snd` PCI device when `-WithVirtioSnd` / `--with-virtio-snd` is set (`disable-legacy=on,x-pci-revision=0x01`; modern-only; enumerates as `PCI\VEN_1AF4&DEV_1059&REV_01`)
- (optional) If `-QemuPreflightPci` / `--qemu-preflight-pci` is enabled, connects to QEMU via QMP and runs `query-pci` to
  validate that the expected virtio devices are present with the expected Vendor/Device/Revision IDs (particularly `REV_01`).
- Watches the serial log for:
  - `AERO_VIRTIO_SELFTEST|RESULT|PASS` / `AERO_VIRTIO_SELFTEST|RESULT|FAIL`
  - When `RESULT|PASS` is seen, the harness also requires that the guest emitted per-test markers for:
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS`
    - (only when blk reset is enabled via `-WithBlkReset` / `--with-blk-reset`) `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS`
    - (only when blk resize is enabled via `-WithBlkResize` / `--with-blk-resize`) `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS`
    - (only when virtio-input binding gating is enabled via `-RequireVirtioInputBinding` / `--require-virtio-input-binding`)
      `AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS`
    - (only when LED/statusq testing is enabled via `-WithInputLed` / `--with-input-led`) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS`
    - (only when `-WithInputLeds` / `--with-input-leds` is enabled) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS`
    - (only when virtio-input event injection is enabled via `-WithInputEvents`/`--with-input-events` or implied by wheel/extended flags)
      `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS`
    - (only when wheel injection is enabled via `-WithInputWheel` / `--with-input-wheel`) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS`
    - (only when media keys injection is enabled via `-WithInputMediaKeys` / `--with-input-media-keys`) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS`
    - (only when extended injection is enabled via `-WithInputEventsExtended` / `--with-input-events-extended`) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS`
    - (only when extended injection is enabled via `-WithInputEventsExtended` / `--with-input-events-extended`) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS`
    - (only when extended injection is enabled via `-WithInputEventsExtended` / `--with-input-events-extended`) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS`
    - (only when tablet injection is enabled via `-WithInputTabletEvents` / `--with-input-tablet-events`)
      `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS` or `...|SKIP` (if `-WithVirtioSnd` / `--with-virtio-snd` is set, it must be `PASS`)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS` or `...|SKIP` (if `-WithVirtioSnd` / `--with-virtio-snd` is set, it must be `PASS`)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS` or `...|SKIP` (if `-WithVirtioSnd` / `--with-virtio-snd` is set, it must be `PASS`)
    - (only when `-WithSndBufferLimits` / `--with-snd-buffer-limits` is enabled) `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS`
    - (only when net checksum offload is required via `-RequireNetCsumOffload` / `--require-net-csum-offload` / `--require-virtio-net-csum-offload`)
      `AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=...` (and `tx_csum > 0`)
    - (only when UDP checksum offload is required via `-RequireNetUdpCsumOffload` / `--require-net-udp-csum-offload`)
      `AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_udp=...` (and `tx_udp > 0`)
    - (only when link flap is enabled via `-WithNetLinkFlap` / `--with-net-link-flap`) `AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS`

The Python/PowerShell harnesses may also emit additional host-side markers after the run for log scraping/diagnostics:

```
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|PASS/FAIL/INFO|write_ok=...|write_bytes=...|write_mbps=...|flush_ok=...|read_ok=...|read_bytes=...|read_mbps=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MSIX|PASS/SKIP|mode=...|messages=...|config_vector=...|queue_vector=...|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO|abort_srb=...|reset_device_srb=...|reset_bus_srb=...|pnp_srb=...|ioctl_reset=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO/SKIP|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n\|not_supported>|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO/SKIP|reset_detected=...|hw_reset_bus=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|PASS/FAIL/SKIP|performed=...|counter_before=...|counter_after=...|err=...|reason=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|REQUEST|old_bytes=...|new_bytes=...|qmp_cmd=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|reason=...|old_bytes=...|new_bytes=...|drive_id=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|PASS/FAIL/SKIP/READY|disk=...|old_bytes=...|new_bytes=...|elapsed_ms=...|reason=...|err=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS/FAIL/INFO|large_ok=...|large_bytes=...|large_fnv1a64=...|large_mbps=...|upload_ok=...|upload_bytes=...|upload_mbps=...|msi=...|msi_messages=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|PASS/FAIL|name=...|down_delay_sec=...|reason=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|PASS/FAIL/SKIP|bytes=...|small_bytes=...|mtu_bytes=...|reason=...|wsa=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS/FAIL/SKIP|server=...|query=...|sent=...|recv=...|rcode=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|PASS/FAIL/INFO|tx_csum=...|rx_csum=...|fallback=...|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO/WARN|reason=...|host_features=...|guest_features=...|irq_mode=...|irq_message_count=...|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n\|none>|rx_vector=<n\|none>|tx_vector=<n\|none>|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND|PASS/FAIL/SKIP|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n\|none>|queue0_vector=<n\|none>|queue1_vector=<n\|none>|queue2_vector=<n\|none>|queue3_vector=<n\|none>|interrupts=<n>|dpcs=<n>|drain0=<n>|drain1=<n>|drain2=<n>|drain3=<n>|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_CAPTURE|PASS/FAIL/SKIP|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_DUPLEX|PASS/FAIL/SKIP|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|PASS/FAIL/SKIP|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=...|capture=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|INFO/SKIP|completions=...|pcm_period=...|xrun=...|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|mapping=...|...
AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|PASS/FAIL|devices=...|wrong_service=...|missing_service=...|problem=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|PASS/FAIL|service=...|pnp_id=...|reason=...|expected=...|actual=...|...
```

These do not affect overall PASS/FAIL.

The harness may also mirror the guest's UDP DNS smoke-test marker (when present) into a stable host marker:

```
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS/FAIL/SKIP|server=...|query=...|sent=...|recv=...|rcode=...|reason=...
```

This is informational only and does not affect overall PASS/FAIL.

The harness may also mirror the guest's `virtio-net-offload-csum` checksum offload marker into a stable host marker:

```
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|PASS/FAIL/INFO|tx_csum=...|rx_csum=...|fallback=...|...
```

This is informational only and does not affect overall PASS/FAIL unless:

- `-RequireNetCsumOffload` / `--require-net-csum-offload` / `--require-virtio-net-csum-offload`, or
- `-RequireNetUdpCsumOffload` / `--require-net-udp-csum-offload` / `--require-virtio-net-udp-csum-offload`

is enabled.

## Optional/Compatibility Features

### IRQ diagnostics (INTx vs MSI/MSI-X)

The harnesses may also surface IRQ-related diagnostics in the output for log scraping. These are informational by
default and do not affect overall PASS/FAIL.

- Standalone guest lines:
  - `virtio-<dev>-irq|INFO|...`
  - `virtio-<dev>-irq|WARN|...`
  - Note: newer guest selftests also emit a virtio-blk miniport IOCTL-derived diagnostics line:
    - `virtio-blk-miniport-irq|INFO|mode=...|messages=...|message_count=...|msix_config_vector=...|msix_queue0_vector=...`
    - (and WARN variants like `virtio-blk-miniport-irq|WARN|reason=...|...` when the miniport contract is missing/truncated)
    - Note: virtio-blk miniport diagnostics may report `mode=msi` even when MSI-X vectors are assigned. The harness treats
      any non-`0xFFFF` MSI-X vector indices as evidence of MSI-X and reports `irq_mode=msix` in the stable
      `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|...` host marker.
- Mirrored host markers (for log scraping):
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_<DEV>_IRQ_DIAG|INFO/WARN|...`

When the guest includes `irq_*` fields on its `AERO_VIRTIO_SELFTEST|TEST|...` markers, the harness may also emit stable per-device host markers:

```
AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|msix_config_vector=...|msix_queue_vector=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...
AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...
```

#### IRQ mode enforcement (optional)

To turn the guest IRQ mode diagnostics into a hard PASS/FAIL condition, enable one of:

- PowerShell: `-RequireIntx` / `-RequireMsi`
- Python: `--require-intx` / `--require-msi`

These flags are mutually exclusive. `-RequireMsi` / `--require-msi` accept both `mode=msi` and `mode=msix` (MSI-X is treated as part of the MSI family).

Guest-side complement:

- The guest selftest can also fail the virtio-blk test if it is still using INTx (expected MSI/MSI-X) via `--expect-blk-msi`.
  - If provisioning via `New-AeroWin7TestImage.ps1`, bake this into the scheduled task with `-ExpectBlkMsi`.

On mismatch (or when the guest does not emit a recognizable IRQ mode marker), the harness fails with a deterministic token, for example:

`FAIL: IRQ_MODE_MISMATCH: virtio-net expected=intx got=msi`

Notes:

- The harness prefers the standalone guest lines `virtio-<dev>-irq|INFO/WARN|mode=...` when present.
- For virtio-blk, it also understands dedicated `virtio-blk-irq` markers and falls back to `irq_mode=...` on `AERO_VIRTIO_SELFTEST|TEST|virtio-blk|...`.

### virtio-net driver diagnostics (`virtio-net-diag`)

Newer guest selftest binaries emit a best-effort virtio-net driver diagnostic line (feature bits, offload toggles, IRQ
mode, queue sizes/indices, MSI-X vector mapping, and optional ctrl-vq state/counters) when the in-guest driver exposes
the optional diagnostics device:

- `virtio-net-diag|INFO|host_features=...|guest_features=...|irq_mode=...|irq_message_count=...|msix_config_vector=...|msix_rx_vector=...|msix_tx_vector=...|...`
- `virtio-net-diag|WARN|reason=not_supported|...` (when unavailable)
- Newer virtio-net drivers may also append TX checksum offload usage counters:
  - `tx_tcp_csum_offload_pkts`, `tx_tcp_csum_fallback_pkts`
  - `tx_udp_csum_offload_pkts`, `tx_udp_csum_fallback_pkts`

The host harness mirrors the last observed line into a stable host-side marker for log scraping:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO/WARN|...`

This is informational only and does not affect overall PASS/FAIL.

### virtio-net checksum offload counters (`virtio-net-offload-csum`) (optional gating)

The guest selftest emits a dedicated machine-readable marker surfacing virtio-net checksum offload behavior:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=<u64>|rx_csum=<u64>|fallback=<u64>|...`

Newer guest/selftest builds may append additional breakdown fields (best-effort), for example:

- `tx_tcp`, `tx_udp`, `rx_tcp`, `rx_udp`
- `tx_tcp4`, `tx_tcp6`, `tx_udp4`, `tx_udp6`
- `rx_tcp4`, `rx_tcp6`, `rx_udp4`, `rx_udp6`

These counters are queried from the driver via the `\\.\AeroVirtioNetDiag` diagnostics device using the IOCTL
`AEROVNET_IOCTL_QUERY_OFFLOAD_STATS` (see `drivers/windows7/virtio-net/include/aero_virtio_net.h`).

By default this marker is informational and does not affect PASS/FAIL. This marker may be present even when checksum
offload is not actually exercised (for example `tx_csum=0`).

- PowerShell: `-RequireNetCsumOffload`
- PowerShell: `-RequireNetUdpCsumOffload` (UDP TX only)
- Python: `--require-net-csum-offload` (alias: `--require-virtio-net-csum-offload`)
- Python: `--require-net-udp-csum-offload` (alias: `--require-virtio-net-udp-csum-offload`) (UDP TX only)
 
When `-RequireNetCsumOffload` / `--require-net-csum-offload` / `--require-virtio-net-csum-offload` is enabled, the harness fails if the marker is missing/FAIL, missing the `tx_csum` field, or reports `tx_csum=0`
(deterministic tokens: `MISSING_VIRTIO_NET_CSUM_OFFLOAD`, `VIRTIO_NET_CSUM_OFFLOAD_FAILED`,
`VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS`, `VIRTIO_NET_CSUM_OFFLOAD_ZERO`).
 
When `-RequireNetUdpCsumOffload` / `--require-net-udp-csum-offload` / `--require-virtio-net-udp-csum-offload` is enabled, the harness fails if the marker is missing/FAIL, missing `tx_udp`
(and no `tx_udp4`/`tx_udp6` fallback fields are present), or reports `tx_udp=0`
(deterministic tokens: `MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD`, `VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED`,
`VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS`, `VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO`).

### virtio-snd negotiated mix format (`virtio-snd-format`)

The guest selftest emits an informational marker surfacing the negotiated virtio-snd endpoint formats as visible via
Windows shared-mode mix format strings:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=<...>|capture=<...>`

The host harness mirrors this into a stable host-side marker for log scraping:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=<...>|capture=<...>`

This is informational only and does not affect overall PASS/FAIL.

### virtio-snd `eventq`

Contract v1 reserves virtio-snd `eventq` and forbids the driver from depending on it, but the Win7 virtio-snd driver is
written to tolerate event traffic. For eventq-specific debugging, use the `DebugLogs` virtio-snd build and capture
kernel debug output while the harness runs (playback/capture will run via `aero-virtio-selftest.exe` when the device is
attached).

Newer guest selftest binaries also emit an informational marker reporting eventq counters:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=...|pcm_period=...|xrun=...|...`

The host harness mirrors this into a stable host-side marker for log scraping:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|INFO/SKIP|...`

### virtio-snd negotiated mix format (VIO-020)

Newer guest selftest binaries emit an informational marker surfacing the virtio-snd endpoint mix formats selected by
Windows (shared-mode) / the driver negotiation logic:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=<...>|capture=<...>`

Both the Python and PowerShell harnesses mirror this into a stable host-side marker for log scraping:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=<...>|capture=<...>`

### Why `x-pci-revision=0x01`?

The Aero Windows 7 virtio device contract encodes the **contract major version** in the PCI
Revision ID (contract v1 = `0x01`).

Some QEMU virtio device types report `REV_00` by default. Once the Aero drivers enforce the
contract Revision ID, the Win7 virtio driver packages will not bind unless QEMU is told to
advertise `REV_01` (the shipped INFs are revision-gated, and some drivers also validate at runtime).

The harness sets `disable-legacy=on` for virtio-net/virtio-blk/virtio-input (and virtio-snd when enabled) so QEMU does **not** expose
the legacy I/O-port transport (transitional devices enumerate with the older `0x1000..` PCI Device IDs such as `1AF4:1000`, `1AF4:1001`, and `1AF4:1011`). This matches
[`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md) (`AERO-W7-VIRTIO` v1),
which is modern-only.

When `-WithVirtioSnd` / `--with-virtio-snd` is enabled, the harness also forces `disable-legacy=on` and
`x-pci-revision=0x01` on the virtio-snd device so it matches the Aero contract v1 HWID (`PCI\VEN_1AF4&DEV_1059&REV_01`)
and the strict `aero_virtio_snd.inf` binds under QEMU.
#### Verifying what your QEMU build reports (no guest required)

You can probe the PCI IDs (including Revision ID) that your local QEMU build advertises for the harness devices with:

```bash
python3 drivers/windows7/tests/host-harness/probe_qemu_virtio_pci_ids.py --qemu-system qemu-system-x86_64 --mode default
python3 drivers/windows7/tests/host-harness/probe_qemu_virtio_pci_ids.py --qemu-system qemu-system-x86_64 --mode contract-v1

# Include virtio-snd as well (requires QEMU virtio-sound-pci/virtio-snd-pci + -audiodev support):
python3 drivers/windows7/tests/host-harness/probe_qemu_virtio_pci_ids.py --qemu-system qemu-system-x86_64 --with-virtio-snd --mode default
python3 drivers/windows7/tests/host-harness/probe_qemu_virtio_pci_ids.py --qemu-system qemu-system-x86_64 --with-virtio-snd --mode contract-v1
```

### Transitional virtio fallback (older QEMU / legacy drivers)

If your QEMU build does not support `disable-legacy=on` (or you need transitional device IDs in the older `0x1000..` range), you can opt back into the previous layout:

- PowerShell: add `-VirtioTransitional`
- Python: add `--virtio-transitional`

Notes:

- Transitional mode is primarily a **backcompat** option for older QEMU builds and/or older guest images.
  - It uses QEMU’s default `virtio-blk`/`virtio-net` devices and relaxes per-test marker requirements so older
    `aero-virtio-selftest.exe` binaries can still be used.
  - It *attempts* to attach virtio-input keyboard/mouse devices (`virtio-keyboard-pci` + `virtio-mouse-pci`) so the
    guest virtio-input selftest can run, but will warn and skip them if the QEMU binary does not advertise those
    devices.
    - In transitional mode, virtio-input **may** enumerate with the older transitional ID space (e.g. `DEV_1011`)
      depending on QEMU, so you need a virtio-input driver package that binds the IDs your QEMU build exposes.

Note: transitional mode is incompatible with virtio-snd testing (`-WithVirtioSnd` / `--with-virtio-snd`), since virtio-snd
testing requires the contract-v1 overrides (`disable-legacy=on,x-pci-revision=0x01`).

## Provisioning an image (recommended approach)

Windows images are **not** distributed in this repo.

The recommended flow:
1. Install Windows 7 normally in QEMU once (using your own ISO + key).
2. Install the Aero virtio drivers (virtio-blk + virtio-net).
3. Copy `aero-virtio-selftest.exe` into the guest.
4. Create a scheduled task to run the selftest at boot (SYSTEM).

The guest-side README includes an example `schtasks /Create ...` command.

If you want to fully automate provisioning, see `New-AeroWin7TestImage.ps1` (template generator / scaffold).

### Driver allowlisting (recommended)

`New-AeroWin7TestImage.ps1` generates a guest-side `provision.cmd` that installs drivers via `pnputil`.

For safety and determinism, the provisioning script installs **only an allowlisted set of INF files** by default
(virtio blk/net/input/snd). This avoids accidentally installing experimental/test INFs (for example
`virtio-transport-test.inf`) that can match the same HWIDs and steal device binding.

Note: the harness uses **modern-only** virtio device IDs for virtio-net/virtio-blk/virtio-input/virtio-snd
(`DEV_1041`/`DEV_1042`/`DEV_1052`/`DEV_1059`) and sets `x-pci-revision=0x01` so strict contract-v1 INFs can bind.

Note: Stock QEMU virtio-input devices report virtio-input `ID_NAME` strings like `QEMU Virtio Keyboard` / `QEMU Virtio Mouse`.
The in-tree Aero virtio-input driver is strict-by-default and expects the Aero contract `ID_NAME` values, so for QEMU-based
testing the guest must enable the driver's ID_NAME compatibility mode:

- `HKLM\SYSTEM\CurrentControlSet\Services\aero_virtio_input\Parameters\CompatIdName` = `1` (REG_DWORD)

`New-AeroWin7TestImage.ps1` can bake this into the generated `provision.cmd` via:

- `-EnableVirtioInputCompatIdName` (alias: `-EnableVirtioInputCompat`)

Install only one INF per HWID. If you keep multiple in-tree packages in the same drivers directory, disambiguate by
passing a relative INF path via `-InfAllowList`.

For virtio-snd, the canonical INF (`aero_virtio_snd.inf`) matches only `PCI\VEN_1AF4&DEV_1059&REV_01`, so your QEMU
build must support `disable-legacy=on` and `x-pci-revision=0x01` for virtio-snd testing.
The repo also contains an optional legacy filename alias INF (`virtio-snd.inf.disabled`; rename to `virtio-snd.inf` to
enable) for compatibility with older workflows/tools. It installs the same driver/service and matches the same
contract-v1 HWIDs, but it is not used by default.

Canonical in-tree driver packages:

- virtio-blk: `drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf` (binds `DEV_1042&REV_01`)
- virtio-net: `drivers/windows7/virtio-net/inf/aero_virtio_net.inf` (binds `DEV_1041&REV_01`)
- virtio-input (keyboard + mouse): `drivers/windows7/virtio-input/inf/aero_virtio_input.inf` (binds
  `DEV_1052&SUBSYS_00101AF4&REV_01` (keyboard) and `DEV_1052&SUBSYS_00111AF4&REV_01` (mouse))
  - Optional strict generic fallback: the repo also contains a legacy alias INF (the `*.inf.disabled` file under
    `drivers/windows7/virtio-input/inf/`) which can be enabled locally. It adds an opt-in strict, revision-gated generic
    fallback HWID (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01` (shown as **Aero VirtIO Input Device**).
    - Alias drift policy: the alias INF is allowed to diverge from `aero_virtio_input.inf` only in the models sections
      (`[Aero.NTx86]` / `[Aero.NTamd64]`) where it adds the fallback entry. Outside those models sections, from the first
      section header onward it is expected to remain byte-identical (banner/comments may differ).
    - Do **not** install both basenames at the same time: they have overlapping bindings and can cause confusing/non-deterministic
      device selection.
- virtio-input (tablet): `drivers/windows7/virtio-input/inf/aero_virtio_tablet.inf` (binds `DEV_1052&SUBSYS_00121AF4&REV_01`; preferred
  binding for contract tablets and wins when it matches)
  - If a tablet device does **not** expose `SUBSYS_0012`, it can still bind via the opt-in strict fallback HWID above when the
    legacy alias INF is enabled locally, and it will appear as the generic **Aero VirtIO Input Device**.
- virtio-snd: `drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf` (binds `DEV_1059&REV_01`)

If QEMU cannot expose modern-only virtio-snd (no `disable-legacy` property on the device), virtio-snd may enumerate as
the transitional ID `DEV_1018` and `aero_virtio_snd.inf` will not bind. Use a QEMU build that supports
`disable-legacy=on` for virtio-snd (or omit virtio-snd from provisioning/tests).

Use `-InstallAllInfs` to install every `*.inf` found under `AERO\\drivers` instead.

If you are provisioning an image with **upstream virtio-win** driver packages (e.g. `viostor.inf` / `netkvm.inf`),
use `-InstallAllInfs` or provide a custom `-InfAllowList`.

For fully repeatable provisioning, pass `-InfAllowList` explicitly:

`New-AeroWin7TestImage.ps1` also supports baking `--blk-root` into the installed scheduled task (useful if the VM boots
from a non-virtio disk but has a separate virtio data volume):

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aero_virtio_blk.inf",
    "aero_virtio_net.inf",
    "aero_virtio_input.inf",
    "aero_virtio_tablet.inf",
    "aero_virtio_snd.inf"
  ) `
  -BlkRoot "D:\aero-virtio-selftest\"
```

To assert that virtio-blk is operating in **MSI/MSI-X** mode (and fail if the driver reports INTx), generate the image
with `-ExpectBlkMsi` (adds the guest selftest flag `--expect-blk-msi` / env var `AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1`).

To make virtio-net MSI-X a **guest-side** hard requirement (and fail the overall guest selftest when `virtio-net-msix`
reports `mode!=msix`), generate the image with `-RequireNetMsix` (adds the guest selftest flag `--require-net-msix` /
env var `AERO_VIRTIO_SELFTEST_REQUIRE_NET_MSIX=1`).

To make virtio-input MSI-X a **guest-side** hard requirement (and fail the overall guest selftest when `virtio-input-msix`
reports `mode!=msix`), generate the image with `-RequireInputMsix` (adds the guest selftest flag `--require-input-msix` /
env var `AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1`).

To make virtio-snd MSI-X a **guest-side** hard requirement (and fail the overall guest selftest when `virtio-snd-msix`
reports `mode!=msix`), generate the image with `-RequireSndMsix` (adds the guest selftest flag `--require-snd-msix` /
env var `AERO_VIRTIO_SELFTEST_REQUIRE_SND_MSIX=1`).

For MSI/MSI-X-specific CI, you can also ask the host harness to fail deterministically when the guest image was not
provisioned with this expectation:

- PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -RequireExpectBlkMsi`
- Python: `invoke_aero_virtio_win7_tests.py --require-expect-blk-msi`

To exercise virtio-snd, make sure you:
- include the virtio-snd driver in the drivers directory you provision into the guest, and
- attach a virtio-snd device when running the harness (`-WithVirtioSnd` / `--with-virtio-snd`).

If you want the guest selftest to fail when virtio-snd is missing (instead of reporting `SKIP`), provision the scheduled
task with `--require-snd` (for example via `New-AeroWin7TestImage.ps1 -RequireSnd`).

To disable the guest selftest's virtio-snd section even if a device is present (adds `--disable-snd` to the scheduled task):
Note: If you run the host harness with `-WithVirtioSnd` / `--with-virtio-snd`, it expects the virtio-snd test to run
and PASS (not SKIP).

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aero_virtio_blk.inf",
    "aero_virtio_net.inf",
    "aero_virtio_input.inf",
    "aero_virtio_tablet.inf",
    "aero_virtio_snd.inf"
  ) `
  -DisableSnd
```

To disable the guest selftest's **capture-only** checks (adds `--disable-snd-capture` to the scheduled task) while still
exercising playback when a virtio-snd device is present:

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aero_virtio_blk.inf",
    "aero_virtio_net.inf",
    "aero_virtio_input.inf",
    "aero_virtio_tablet.inf",
    "aero_virtio_snd.inf"
  ) `
  -DisableSndCapture
```

Note: if you run the host harness with `-WithVirtioSnd` / `--with-virtio-snd`, it expects virtio-snd-capture to PASS
(not SKIP), so do not use `-DisableSndCapture` in that mode.

To run the virtio-snd **capture** smoke test (and enable the full-duplex regression test):

- Newer `aero-virtio-selftest.exe` binaries auto-run capture + duplex tests whenever a virtio-snd device is present
  (this is required for the strict host harness mode when `-WithVirtioSnd` / `--with-virtio-snd` is enabled).
- For older selftest binaries, provision the scheduled task with `--test-snd-capture` (for example via
  `New-AeroWin7TestImage.ps1 -TestSndCapture`), or set `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1` in the guest environment.

- Add `-RequireSndCapture` to fail if no virtio-snd capture endpoint is present.
- Add `-RequireNonSilence` to fail the smoke test if only silence is captured.
- Add `-TestSndBufferLimits` to provision the guest scheduled task with `--test-snd-buffer-limits` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`)
  (required when running the host harness with `-WithSndBufferLimits` / `--with-snd-buffer-limits`).
- Add `-AllowVirtioSndTransitional` to accept a transitional virtio-snd PCI ID (typically `PCI\VEN_1AF4&DEV_1018`) in the guest selftest
  (intended for debugging/backcompat outside the strict harness setup).
  - Tip: when using this mode, also stage/install the QEMU compatibility driver package
    (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`), for example by including `aero-virtio-snd-legacy.inf`
    in `-InfAllowList`.

If your `-DriversDir` contains duplicate INF basenames, disambiguate by passing a relative path (e.g.
`"windows7\\virtio-net\\x64\\aero_virtio_net.inf"` or `"windows7\\virtio-input\\x64\\aero_virtio_input.inf"` when using `out/packages`). To restore the legacy "install everything" behavior for debugging, pass `-InstallAllInfs`.

### Enabling test-signing mode (unsigned / test-signed drivers)

On Windows 7 x64, kernel drivers must be signed (or the machine must be in test-signing mode).

If your Aero virtio drivers are not yet production-signed, `New-AeroWin7TestImage.ps1` can embed a `bcdedit /set testsigning on`
step into the provisioning script:

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aero_virtio_blk.inf",
    "aero_virtio_net.inf",
    "aero_virtio_input.inf",
    "aero_virtio_tablet.inf",
    "aero_virtio_snd.inf"
  ) `
  -EnableTestSigning `
  -AutoReboot
```

### Installing Windows 7 from a user-supplied ISO (interactive)

If you don't already have a prepared VM, `Start-AeroWin7Installer.ps1` can launch an interactive Windows 7 install
under QEMU with a virtio disk + virtio NIC and (optionally) an attached provisioning ISO:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Start-AeroWin7Installer.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -QemuImg qemu-img `
  -Win7IsoPath ./Win7SP1.iso `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -CreateDisk `
  -ProvisioningIsoPath ./aero-win7-provisioning.iso
```

This is still **interactive** (Windows Setup UI), but it standardizes the QEMU device layout and makes it easy
to load virtio storage drivers from the provisioning ISO during installation.

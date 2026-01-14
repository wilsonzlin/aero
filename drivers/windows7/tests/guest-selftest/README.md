# `aero-virtio-selftest.exe` (Windows 7 guest tool)

This is a small **Windows 7 user-mode console tool** intended to run inside the guest at boot and report
virtio driver health via **COM1 serial** (host-captured), stdout, and a log file on `C:\`.

For the consolidated virtio-input end-to-end validation plan (device model + driver + web runtime), see:

- [`docs/virtio-input-test-plan.md`](../../../../docs/virtio-input-test-plan.md)

## What it tests

- **virtio-blk**
  - Detect a virtio disk device (SetupAPI hardware IDs).
  - Query the `aero_virtio_blk` miniport (via `IOCTL_SCSI_MINIPORT`) and validate basic configuration/feature bits.
    - When the miniport reports the v2+ IOCTL payload, the virtio-blk machine marker also includes StorPort recovery
      counters for log scraping:
      `abort_srb`, `reset_device_srb`, `reset_bus_srb`, `pnp_srb`, `ioctl_reset`.
  - Optional interrupt-mode expectation:
    - Enable with `--expect-blk-msi` (or env var `AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1`).
    - When enabled, the virtio-blk test **FAIL**s if the miniport reports it is still operating in **INTx**
      mode (expected **MSI/MSI-X**).
    - Useful when running the host harness with explicit MSI-X vectors (`--virtio-msix-vectors` or per-device
      `--virtio-<dev>-vectors`) or when validating MSI/MSI-X support end-to-end.
  - Issue a SCSI pass-through `REPORT_LUNS` (0xA0) command (via `IOCTL_SCSI_PASS_THROUGH_DIRECT`) and validate a sane
    single-LUN response.
  - Create a temporary file on a **virtio-backed volume** and perform:
    - sequential write + readback verification
    - `FlushFileBuffers` success check
    - sequential read pass
  - Optional runtime resize test (`virtio-blk-resize`):
    - Disabled by default; enable with `--test-blk-resize` (or env var `AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE=1`).
    - Emits `...|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>`, then waits for a host-side QMP resize and polls until
      Windows reports a larger disk size (hard timeout, default 60s).
- **virtio-net**
  - Detect a virtio network adapter (SetupAPI hardware IDs).
  - Wait for link + DHCP IPv4 address (non-APIPA).
  - Deterministic UDP echo roundtrip to the host harness (exercises UDP TX/RX):
    - Sends a small datagram (32 bytes) and a near-MTU datagram (1400 bytes) to `10.0.2.2:<udp_port>` and expects the
      payload to be echoed back.
    - Default `udp_port` is `18081` (configurable via `--udp-port`).
    - This is intended to exercise UDP checksum offload paths end-to-end.
  - DNS resolution (`getaddrinfo`)
  - Best-effort UDP DNS query smoke test (informational; does not affect overall PASS/FAIL):
    - Sends a minimal UDP DNS query to the adapter's configured DNS server(s) (as reported by `GetAdaptersInfo`).
    - Emits the marker `virtio-net-udp-dns` (PASS/FAIL/SKIP) but the overall virtio-net test does not depend on it.
  - HTTP GET to a configurable URL (WinHTTP) to validate basic connectivity.
  - Deterministic large HTTP download (`<http_url>-large`) to stress sustained RX throughput and verify data integrity:
    - downloads **1 MiB** of bytes `0..255` repeating
    - requires a correct `Content-Length: 1048576`
    - validates both total bytes read and a fixed hash (FNV-1a 64-bit)
    - logs `Content-Type`/`ETag` headers when present for additional diagnostics
  - Deterministic large HTTP upload (HTTP POST to `<http_url>-large`) to stress sustained TX throughput:
    - uploads **1 MiB** of bytes `0..255` repeating
    - expects a 2xx response from the host harness, which validates integrity (SHA-256)
- **virtio-input**
  - Enumerate HID devices (SetupAPI via `GUID_DEVINTERFACE_HID`).
  - Detect virtio-input devices by matching virtio-input PCI/HID IDs:
    - `VEN_1AF4&DEV_1052` (modern) and `VEN_1AF4&DEV_1011` (transitional)
    - or HID-style `VID_1AF4&PID_0001` (keyboard) / `VID_1AF4&PID_0002` (mouse) / `VID_1AF4&PID_0003` (tablet)
      (older/alternate builds may use PCI-style PIDs like `PID_1052` / `PID_1011`)
  - Aero contract note:
    - `AERO-W7-VIRTIO` v1 expects the modern virtio-input PCI ID (`DEV_1052`) with `REV_01`.
    - The in-tree Aero Win7 virtio-input INF is revision-gated, so QEMU-style `REV_00` virtio-input devices will not bind unless you override the revision (for example `x-pci-revision=0x01`).
    - The host harness can optionally validate the QEMU-emitted PCI Vendor/Device/Revision IDs via QMP (`query-pci`) before waiting for guest results:
      - PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -QemuPreflightPci` (alias: `-QmpPreflightPci`)
      - Python: `invoke_aero_virtio_win7_tests.py --qemu-preflight-pci` (alias: `--qmp-preflight-pci`)
  - PCI binding / service validation (`virtio-input-bind`):
    - Enumerates `PCI\VEN_1AF4&DEV_1052` devnodes and validates:
      - bound service name is `aero_virtio_input` (`SPDRP_SERVICE`)
      - ConfigManager health: `CM_Get_DevNode_Status` reports no problem (`problem==0` and no `DN_HAS_PROBLEM`)
    - Emits a machine marker:
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|devices=<n>`
      - or `...|FAIL|devices=<n>|wrong_service=<n>|missing_service=<n>|problem=<n>`
    - The Win7 host harness treats this marker as required in strict/contract-v1 mode so older guest selftest binaries can't accidentally pass.
  - Read the HID report descriptor (`IOCTL_HID_GET_REPORT_DESCRIPTOR`) and sanity-check that:
    - at least one **keyboard-only** HID device exists
    - at least one **relative-mouse-only** HID device exists (X/Y reported as *Relative*)
    - additional **tablet / absolute-pointer** virtio-input HID devices are allowed and are counted separately
    - no matched HID device advertises both keyboard and mouse application collections (contract v1 expects two separate PCI functions).
  - Optional interrupt-mode diagnostics (`virtio-input-msix`):
    - The selftest queries the virtio-input HID minidriver for its interrupt configuration via a diagnostics IOCTL and emits:
      `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS/FAIL/SKIP|mode=<intx|msix>|messages=<n>|mapping=...|...`.
    - This marker is informational by default (so older configurations that fall back to INTx can still PASS overall).
    - Use `--require-input-msix` (or env var `AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1`) to make the selftest **FAIL**
      when virtio-input is not using MSI-X (mode != `msix`).
    - The host harness can also enforce this marker:
      - PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -RequireVirtioInputMsix`
      - Python: `invoke_aero_virtio_win7_tests.py --require-virtio-input-msix`
  - Optional end-to-end **event delivery** smoke test (`virtio-input-events`):
    - Disabled by default (so the selftest remains fully headless and does not depend on host-side input injection).
    - Enable with `--test-input-events` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS=1`).
    - The selftest opens the virtio-input keyboard + mouse HID interfaces and reads **input reports** directly via `ReadFile`
      on the HID device path (no window focus required).
    - Intended to be paired with host-side QMP injection (`input-send-event`) when the harness is run with:
      - PowerShell: `-WithInputEvents` (aliases: `-WithVirtioInputEvents`, `-EnableVirtioInputEvents`)
      - Python: `--with-input-events` (aliases: `--with-virtio-input-events`, `--enable-virtio-input-events`, `--require-virtio-input-events`)
    - Expected injected sequence (used by the host harness via QMP `input-send-event`):
      - keyboard: `'a'` press + release
      - mouse: small relative move + left click
    - When enabled, the test emits a readiness marker (`AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY`), then waits
      (with a hard timeout) for host-injected events (intended to be paired with QMP `input-send-event` injection) and emits
      `...|PASS|...` or `...|FAIL|reason=...|...`.
  - Optional end-to-end **scroll wheel** smoke test (`virtio-input-wheel`):
    - Runs as part of the `--test-input-events` flow (no separate guest flag).
    - Intended to be paired with host-side QMP injection (`input-send-event`) when the harness is run with:
      - PowerShell: `-WithInputWheel` (aliases: `-WithVirtioInputWheel`, `-EnableVirtioInputWheel`)
      - Python: `--with-input-wheel` (aliases: `--with-virtio-input-wheel`, `--require-virtio-input-wheel`, `--enable-virtio-input-wheel`)
    - Validates that the mouse HID input reports include:
      - vertical wheel (HID Generic Desktop `Wheel`)
      - horizontal wheel (HID Consumer `AC Pan`, sourced from Linux `REL_HWHEEL`)
    - Expected injected deltas (deterministic):
      - wheel: `+1`
      - horizontal pan: `-2`
    - Note: The host harness may retry injection a few times after the guest reports `virtio-input-events|READY` to reduce
      timing flakiness. In that case the guest may observe multiple injected scroll events; the wheel selftest is
      designed to handle this, and totals may be multiples of the injected values.
    - Emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS/FAIL/SKIP|...`.
  - Optional end-to-end **extended input events** (`virtio-input-events-*` subtests):
    - Disabled by default.
    - Enable all extended subtests with:
      - `--test-input-events-extended` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1`)
    - Or enable individual subtests:
      - `--test-input-events-modifiers` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_MODIFIERS=1`)
      - `--test-input-events-buttons` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_BUTTONS=1`)
      - `--test-input-events-wheel` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_WHEEL=1`)
    - Intended to be paired with host-side QMP injection when the harness is run with:
      - PowerShell: `-WithInputEventsExtended` (alias: `-WithInputEventsExtra`)
      - Python: `--with-input-events-extended` (alias: `--with-input-events-extra`)
    - Emits separate `PASS/FAIL/SKIP` markers:
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|...` (Shift/Ctrl/Alt + F1)
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|...` (mouse side/extra buttons)
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|...` (wheel + horizontal wheel)
  - Optional end-to-end **Consumer Control / media keys** event delivery smoke test (`virtio-input-media-keys`):
    - Disabled by default (requires host-side QMP injection).
    - Enable with `--test-input-media-keys` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_MEDIA_KEYS=1`).
    - The selftest opens the virtio-input Consumer Control HID interface (if exposed as a separate HID collection) and reads
      input reports via `ReadFile` (no window focus required).
    - Intended to be paired with host-side QMP injection (`input-send-event`) when the harness is run with:
      - PowerShell: `-WithInputMediaKeys` (aliases: `-WithVirtioInputMediaKeys`, `-EnableVirtioInputMediaKeys`)
      - Python: `--with-input-media-keys` (aliases: `--with-virtio-input-media-keys`, `--enable-virtio-input-media-keys`)
    - When enabled, the test emits a readiness marker (`AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|READY`), then waits
      for host-injected events and emits `...|PASS|...` or `...|FAIL|reason=...|...`.
  - Optional virtio-input MSI-X requirement (`virtio-input-msix`):
    - The selftest emits a `virtio-input-msix` marker describing interrupt mode (`mode=intx/msix`) and vector mapping.
    - Use `--require-input-msix` to fail the overall selftest when virtio-input is not using MSI-X.
  - Optional end-to-end **tablet (absolute pointer)** event delivery smoke test (`virtio-input-tablet-events`):
    - Disabled by default (requires host-side QMP injection).
    - Enable with `--test-input-tablet-events` (alias: `--test-tablet-events`) or env var
      `AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1` / `AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1`.
    - The selftest opens the virtio tablet HID interface and reads input reports via `ReadFile` (no window focus required).
    - Requires a virtio tablet device (typically QEMU `-device virtio-tablet-pci`) to be present and bound to virtio-input.
    - Intended to be paired with host-side QMP injection (`input-send-event`) when the harness is run with:
      - PowerShell: `-WithInputTabletEvents` (aliases: `-WithVirtioInputTabletEvents`, `-EnableVirtioInputTabletEvents`, `-WithTabletEvents`, `-EnableTabletEvents`)
      - Python: `--with-input-tablet-events` (aliases: `--with-virtio-input-tablet-events`, `--with-tablet-events`, `--enable-virtio-input-tablet-events`, `--require-virtio-input-tablet-events`)
    - Expected injected sequence (used by the host harness via QMP `input-send-event`):
      - absolute move to (0,0) (reset)
      - absolute move to (10000,20000) (target)
      - left click down + up
    - When enabled, the test emits a readiness marker (`AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY`), then waits
      (with a hard timeout) for host-injected events and emits `...|PASS|...` or `...|FAIL|reason=...|...`.
- **virtio-snd** (optional; playback runs automatically when a supported virtio-snd device is detected)
  - Detect the virtio-snd PCI function via SetupAPI hardware IDs:
    - `PCI\VEN_1AF4&DEV_1059` (modern; strict INF matches `PCI\VEN_1AF4&DEV_1059&REV_01`)
      - If the VM/device does not report `REV_01`, the Aero contract driver will not bind and the selftest will report binding diagnostics (for example `driver_not_bound` / `wrong_service`) and log that `REV_01` is missing.
      - For QEMU-based testing with the strict contract-v1 package, you typically need `disable-legacy=on,x-pci-revision=0x01` for the virtio-snd device so Windows enumerates `PCI\VEN_1AF4&DEV_1059&REV_01`.
    - If QEMU is not launched with `disable-legacy=on`, virtio-snd may enumerate as the transitional PCI ID `PCI\VEN_1AF4&DEV_1018` (often `REV_00`).
      - The Aero contract INF is strict and will not bind; install the opt-in transitional package (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
      - Pass `--allow-virtio-snd-transitional` to accept the transitional ID (intended for QEMU bring-up/regression).
  - Validate that the PCI device is bound to the expected in-tree driver service and emit
    actionable diagnostics (PNP instance ID, ConfigManagerErrorCode / Device Manager “Code X”, driver INF name
    when queryable).
    - Contract v1 (`aero_virtio_snd.inf`): expects service `aero_virtio_snd`
    - QEMU compatibility package (`aero-virtio-snd-legacy.inf`): expects service `aeroviosnd_legacy`
  - Enumerate audio render endpoints via MMDevice API and start a shared-mode WASAPI render stream.
  - Query the endpoint **shared-mode mix format** via `IAudioClient::GetMixFormat()` and initialize the stream using that
    format (with a 48kHz/16-bit/stereo fallback if `GetMixFormat` fails).
    - This keeps the selftest compatible with virtio-snd devices that negotiate a non-contract format/rate (for example
      44.1kHz or 24-bit) via `PCM_INFO`.
  - Render a short deterministic tone (440Hz) in the initialized stream format.
  - Best-effort: unmute the selected render endpoint, set its master volume to a non-trivial level, and
    set the per-session volume to 100% (so host-side wav capture is not accidentally silent due to a
    muted/zero-volume image or a muted per-application volume mixer entry).
  - Debug note: the in-tree virtio-snd driver supports a per-device `ForceNullBackend` registry flag
    (under the device instance’s `Device Parameters\\Parameters` subkey) that disables the virtio transport and routes
    the endpoint through the null backend. This makes host-side wav capture silent; the selftest will
    emit `...|FAIL|force_null_backend` if the flag is enabled.
    - Registry path: `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend`
    - Changing this value requires a reboot or disable/enable cycle so Windows re-runs `START_DEVICE`.
  - Additional bring-up flag: `AllowPollingOnly` under the same per-device `Device Parameters\\Parameters` key:
    - Registry path: `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly`
    - When set to `1`, the driver may start in polling-only mode if no usable interrupt resource can be connected (neither MSI/MSI-X nor INTx). Intended for early device-model bring-up.
    - Changing this value requires a reboot or disable/enable cycle so Windows re-runs `START_DEVICE`.
  - Backwards compatibility note: older driver installs may have these values under the driver software key instead of the per-device key.
    The driver and selftest both check the per-device location first, then fall back to the driver key.
  - If WASAPI fails, a WinMM `waveOut` fallback is attempted.
  - By default, if a supported virtio-snd PCI function is detected, the selftest exercises playback automatically.
    - If no supported device is detected, virtio-snd is reported as **SKIP**.
    - Use `--require-snd` (alias: `--test-snd`) to make missing virtio-snd fail the overall selftest.
  - Playback failures cause the overall selftest to **FAIL**.
  - Also emits a separate `virtio-snd-capture` marker by attempting to detect a virtio-snd **capture** endpoint
    (MMDevice `eCapture`).
    - Missing capture is reported as **SKIP** by default; use `--require-snd-capture` to make it **FAIL**.
    - By default, when virtio-snd playback is exercised (device present or `--require-snd`), the selftest also runs a
      shared-mode WASAPI capture smoke test when a capture endpoint exists.
    - Use `--test-snd-capture` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`) to force the capture smoke test
      even when virtio-snd playback would otherwise be skipped (for example when running an older selftest binary or
      when debugging outside the strict host harness setup).
      - The smoke test initializes a capture stream using the endpoint shared-mode mix format (`GetMixFormat`), with a
        fallback to the legacy contract format (**48kHz / 16-bit / mono PCM**) if `GetMixFormat` fails.
      - By default, the smoke test **PASS**es even if the captured audio is only silence.
      - Use `--require-non-silence` to fail the capture smoke test if no non-silent buffers are observed.
    - When a capture smoke test runs (default when virtio-snd is present), the selftest also runs a **full-duplex**
      regression check (`virtio-snd-duplex`):
      - Opens a matching **render** endpoint and **capture** endpoint in shared-mode WASAPI and initializes both using
        their shared-mode mix formats (`GetMixFormat`, with fallbacks if needed).
      - Starts both streams and runs them concurrently for a short fixed duration while:
        - continuously submitting a deterministic tone on the render path, and
        - continuously draining capture buffers and counting frames.
      - PASS criteria:
        - all WASAPI calls succeed (Init/Start/GetBuffer/ReleaseBuffer/GetNextPacketSize/GetBuffer/ReleaseBuffer/Stop)
        - capture returns `frames > 0` (ensures capture does not stall while render is running)
      - The duplex test records whether any non-silence was observed for diagnostics, but does **not** require non-silence.
  - Use `--disable-snd` to force **SKIP** for both playback and capture.
  - Use `--disable-snd-capture` to force **SKIP** for capture only (while still exercising playback).
  - Optional buffer sizing stress test:
    - Use `--test-snd-buffer-limits` to run a WASAPI stress check that attempts to initialize a render stream with an
      intentionally large buffer duration/period, to exercise virtio-snd buffer sizing limits (for example large cyclic
      buffers / payload caps).
    - Emits a separate `virtio-snd-buffer-limits` marker.
    - PASS criteria:
      - `IAudioClient::Initialize` either succeeds, or fails with a handled/expected HRESULT (commonly `AUDCLNT_E_*` /
        `E_INVALIDARG`), and the selftest remains responsive.
    - FAIL criteria:
      - the Initialize attempt hangs (the selftest times it out), or
      - Initialize succeeds but returns an obviously inconsistent buffer size (for example `GetBufferSize` fails or
        reports 0 frames).
  - The selftest also emits an informational marker surfacing the negotiated mix formats as visible through the Windows
    audio stack:
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=<...>|capture=<...>`
    - The host harness mirrors this as:
      - `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=<...>|capture=<...>`

Note: For deterministic DNS testing under QEMU slirp, the default `--dns-host` is `host.lan`
(with fallbacks like `gateway.lan` / `dns.lan`).

## Output markers

The host harness parses these markers from COM1 serial:

 ```
  AERO_VIRTIO_SELFTEST|START|...
  # virtio-blk/virtio-net/virtio-snd/virtio-input include interrupt diagnostics (`irq_mode` / `irq_message_count`) as
  # key/value fields so the host harness can mirror them into host-side markers (VIRTIO_*_IRQ).
  # virtio-blk additionally includes MSI-X routing fields and basic I/O throughput metrics (VIRTIO_BLK_IO).
  # Older guests may emit just `AERO_VIRTIO_SELFTEST|TEST|virtio-<dev>|PASS` with no extra fields.
  AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|irq_message_count=2|msix_config_vector=0x0000|msix_queue_vector=0x0001|write_ok=1|write_bytes=33554432|write_mbps=123.45|flush_ok=1|read_ok=1|read_bytes=33554432|read_mbps=234.56
  AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set
  AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...|irq_mode=msi|irq_message_count=1
  AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|messages=3|mapping=per-queue|...
  AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set
  AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|flag_not_set
  AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set

 # Optional: end-to-end virtio-blk runtime resize (requires host-side QMP resize):
 # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>
 # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=<N>|old_bytes=<u64>|new_bytes=<u64>|elapsed_ms=<u32>

 # Optional: end-to-end virtio-input event delivery (requires host-side QMP injection):
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...
 #
 # Optional: end-to-end virtio-input mouse wheel delivery (requires host-side QMP injection and --test-input-events):
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS|wheel_total=...|hwheel_total=...|...
 #
 # Optional: end-to-end virtio-input tablet (absolute pointer) event delivery (requires host-side QMP injection):
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|...
 # virtio-snd may be SKIP/PASS/FAIL depending on flags and device presence.
 # Capture is reported separately as "virtio-snd-capture".
#
# Example: virtio-snd not present (or not required) => skip:
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|SKIP|device_missing
AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS|bytes=1400|small_bytes=32|mtu_bytes=1400
AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS
AERO_VIRTIO_SELFTEST|RESULT|PASS

# Example: virtio-snd present => playback + capture markers:
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=...|pcm_period=...|xrun=...
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS|mode=...|init_hr=0x...|expected_failure=...|buffer_bytes=...
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|method=wasapi|frames=...|non_silence=...|silence_only=...
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS|frames=...|non_silence=...
AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS|bytes=1400|small_bytes=32|mtu_bytes=1400
AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS
AERO_VIRTIO_SELFTEST|RESULT|PASS

# Example: virtio-snd failure => overall FAIL:
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|...
AERO_VIRTIO_SELFTEST|RESULT|FAIL
```

Notes:
- IRQ diagnostics are emitted as standalone lines (in addition to the stable `AERO_VIRTIO_SELFTEST|TEST|...` markers):
  - The per-device TEST markers include:
    - `irq_mode=intx|msi|msix|none`
    - `irq_message_count=<n>` (0 for INTx/none)
    - `virtio-blk` additionally includes `msix_config_vector=0x....` and `msix_queue_vector=0x....` when the
      virtio-blk miniport IOCTL exposes them.
  - The tool also emits standalone diagnostics (best-effort):
    - `virtio-blk-miniport-irq|INFO|mode=<intx|msi|unknown>|messages=<n>|message_count=<n>|msix_config_vector=0x....|msix_queue0_vector=0x....`
      (and WARN variants like `virtio-blk-miniport-irq|WARN|...` when the miniport contract is missing/truncated)
    - `virtio-<dev>-irq|INFO|mode=intx`
    - `virtio-<dev>-irq|INFO|mode=msi|messages=<n>` (message interrupts; does not distinguish MSI vs MSI-X)
    - `virtio-<dev>-irq|INFO|mode=msix|messages=<n>|msix_config_vector=0x....|...` (when a driver exposes richer MSI-X diagnostics)
    - `virtio-snd-irq|INFO|mode=none|...` (polling-only; no interrupt objects are connected)
      (and WARN variants like `virtio-<dev>-irq|WARN|reason=...`).
  The host harness mirrors the per-test `irq_*` fields into `AERO_VIRTIO_WIN7_HOST|VIRTIO_*_IRQ|...` markers, and the
  standalone lines into `AERO_VIRTIO_WIN7_HOST|VIRTIO_*_IRQ_DIAG|...` markers for log scraping/CI
  (for example `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ_DIAG|INFO|mode=msi|messages=4`).
- Dedicated MSI-X **TEST markers** are also emitted for some devices (used by the host harness when `--require-virtio-*-msix` is enabled):
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=intx/msix|messages=<n>|config_vector=<v>|queue_vector=<v>`
    (emitted when the virtio-blk miniport IOCTL includes interrupt diagnostics; the harness can require `mode=msix`)
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS/SKIP|mode=intx/msi/msix/unknown|messages=<n>|config_vector=<v>|rx_vector=<v>|tx_vector=<v>`
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS/SKIP|mode=intx/msix/none/unknown|messages=<n>|config_vector=<v>|queue0_vector=<v>|...`
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS/FAIL/SKIP|mode=intx/msix/unknown|messages=<n>|mapping=...|...`
    (the marker is always emitted; `--require-input-msix` makes non-`mode=msix` fail the overall selftest)
- If no supported virtio-snd PCI function is detected (and no capture flags are set), the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP` and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set`.
- The optional virtio-blk runtime resize marker is always emitted:
  - Default (not enabled): `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set`
  - When `--test-blk-resize` (or `AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE=1`) is enabled:
    - emits `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>` once the polling loop is armed
    - then emits `...|PASS|...` when Windows observes a larger disk size, or `...|FAIL|reason=...|...` on timeout/errors.
- The optional virtio-input end-to-end event delivery markers are always emitted:
  - Keyboard + relative mouse (`virtio-input-events`):
    - Default (not enabled): `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set`
    - When `--test-input-events` (or `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS=1`) is enabled:
      - emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY` once the read loop is armed
      - emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...` or `...|FAIL|reason=...|...`
  - Tablet / absolute pointer (`virtio-input-tablet-events`):
    - Default (not enabled): `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set`
    - When `--test-input-tablet-events` (alias: `--test-tablet-events`) (or `AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1` /
      `AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1`) is enabled:
      - emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY` once the read loop is armed
      - emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|...` or `...|FAIL|reason=...|...`
      - if no tablet device is present, emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|no_tablet_device`
  - The overall selftest `RESULT` is only affected by these tests when the corresponding flag/env var is enabled.
- If `--require-snd` / `--test-snd` is set and the PCI device is missing, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL`.
  (In this case, the capture marker uses `...|device_missing` and is `SKIP` by default unless `--require-snd-capture` is set.)
- If the virtio-snd PCI device is present but not bound to the expected driver, the tool emits a reason code:
  - `wrong_service` (bound to an unexpected service)
  - `driver_not_bound` (no `SPDRP_SERVICE` / no driver installed)
  - `device_error` (Config Manager “Code X” / `DN_HAS_PROBLEM`)
  (and will `SKIP`/`FAIL` the capture marker similarly, depending on `--require-snd-capture`).
- If the virtio-snd capture endpoint is missing, the tool emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing`
  (unless `--require-snd-capture` is set).
- When a capture smoke test runs (default when virtio-snd is present, or forced via `--test-snd-capture` / `--require-non-silence`),
  the `virtio-snd-capture` marker includes extra diagnostics such as `method=...`, `frames=...`, and whether any non-silence
  was observed. If `--require-non-silence` is set and only silence is captured, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|silence`.
- If the virtio-snd test is disabled via `--disable-snd`, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP` and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled`.
- If capture is disabled via `--disable-snd-capture`, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled` (playback behavior unchanged).
- The `virtio-net` marker includes large transfer diagnostics:
  - `large_ok`, `large_bytes`, `large_fnv1a64`, `large_mbps`
  - `upload_ok`, `upload_bytes`, `upload_mbps`
- The `virtio-net-udp` marker includes UDP echo diagnostics:
  - `bytes`: size of the last attempted UDP datagram (typically 1400 on PASS)
  - `small_bytes`, `mtu_bytes`: configured payload sizes
  - `reason`, `wsa`: failure diagnostics (WSA error code), and `reason=-` on PASS
- The `virtio-net` marker also includes best-effort MSI/MSI-X allocation diagnostics:
  - `msi`: `1` when Windows assigned message-signaled interrupts; `0` for INTx; `-1` if the query failed
  - `msi_messages`: number of allocated messages (`0` for INTx; `-1` if the query failed)
- The `virtio-net` marker also includes generic IRQ fields (used by the host harness to emit `VIRTIO_NET_IRQ`):
  - `irq_mode`: `msi` / `intx` / `none`
  - `irq_message_count`: number of message interrupts (`0` for INTx)
  - `irq_reason`: optional (present when `irq_mode=none`)
- The virtio-net section also emits a standalone UDP echo test marker (used by the host harness):
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS|bytes=<n>|small_bytes=<n>|mtu_bytes=<n>|reason=<...>|wsa=<err>`
  - The overall virtio-net test fails if this UDP echo test fails.
- The virtio-net section also emits an additional (informational) UDP DNS query marker:
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|PASS/FAIL/SKIP|server=<ip>|query=<host>|sent=<n>|recv=<n>|rcode=<n>`
  - This marker is best-effort and does not affect overall PASS/FAIL.
- The virtio-net section also emits an optional ctrl virtqueue diagnostic line (not parsed by the harness):
  - `virtio-net-ctrl-vq|INFO|...`
  - This is best-effort and may emit `...|diag_unavailable` if the in-guest driver did not expose the registry-backed diagnostics.
- The virtio-net section also emits an optional driver diagnostics line (parsed and mirrored by the host harness):
  - `virtio-net-diag|INFO|host_features=...|guest_features=...|irq_mode=...|irq_message_count=...|msix_config_vector=...|msix_rx_vector=...|msix_tx_vector=...|...`
  - `virtio-net-diag|WARN|reason=not_supported|...` (for example when the driver does not expose `\\.\AeroVirtioNetDiag`)
  - The host harness mirrors this into `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO/WARN|...` for log scraping/CI.
  - Newer virtio-net drivers also include best-effort TX checksum offload usage counters on this line:
    - `tx_tcp_csum_offload_pkts`, `tx_tcp_csum_fallback_pkts`
    - `tx_udp_csum_offload_pkts`, `tx_udp_csum_fallback_pkts`
    - These counters reflect how many NET_BUFFERs used virtio-net checksum offload vs fell back to software checksum in the miniport.
- The `virtio-blk` marker includes basic file I/O diagnostics:
  - `write_ok`, `write_bytes`, `write_mbps`
  - `flush_ok`
  - `read_ok`, `read_bytes`, `read_mbps`
- The duplex marker (`virtio-snd-duplex`) is emitted whenever the virtio-snd section runs:
  - `SKIP|flag_not_set` when virtio-snd is skipped (and capture smoke testing is not forced).
  - `PASS|frames=...|non_silence=...` when the duplex test runs successfully.
  - `FAIL|reason=...|hr=...` if any WASAPI call fails or capture returns no frames.
  - `SKIP|endpoint_missing` when the duplex test is enabled but a matching endpoint cannot be found.
- The buffer sizing stress marker (`virtio-snd-buffer-limits`) is emitted when `--test-snd-buffer-limits` is set:
  - `PASS|...` when the large-buffer Initialize attempt completes without hanging (success or expected failure).
  - `FAIL|reason=...|hr=...` when the attempt times out or returns inconsistent results.
- The virtio-snd section also emits an informational mix-format marker (`virtio-snd-format`) describing the shared-mode
  endpoint formats Windows selected (useful for debugging non-contract devices and audio routing issues):
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=<...>|capture=<...>`
  - The host harness mirrors this into `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|...` for log scraping/CI.

## Building

### Prereqs

- Visual Studio (MSVC) capable of producing Windows 7 compatible binaries
- Windows 7 compatible SDK / toolset (or a newer SDK while still targeting `_WIN32_WINNT=0x0601`)
- CMake (optional, but recommended)

Notes on Win7 compatibility:
- The provided CMake config builds with the **static MSVC runtime** (`/MT`) and sets the subsystem version to **6.01**,
  so the resulting `aero-virtio-selftest.exe` can run on a clean Windows 7 SP1 install without an additional VC++ runtime step.
- The virtio-snd test uses WASAPI/MMDevice (and a WinMM fallback) and requires linking `mmdevapi`, `ole32`, `uuid`, and `winmm`
  (handled by the CMake config).

### Build with CMake (recommended)

From a Developer Command Prompt:

```bat
cd drivers\windows7\tests\guest-selftest
mkdir build
cd build
cmake -G "Visual Studio 17 2022" -A x64 ..
cmake --build . --config Release
```

Output:
- `build\Release\aero-virtio-selftest.exe`

Build x86:

```bat
cmake -G "Visual Studio 17 2022" -A Win32 ..
cmake --build . --config Release
```

## Installing in the guest

Copy `aero-virtio-selftest.exe` to the guest, then configure it to run automatically on boot.

Recommended (runs as SYSTEM at startup):

```bat
mkdir C:\AeroTests
copy aero-virtio-selftest.exe C:\AeroTests\

schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

To require virtio-snd (fail the overall run if the virtio-snd PCI device is missing):

```bat
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --require-snd --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

(Alias: `--test-snd`.)

Note: Aero contract v1 requires `REV_01` and a modern-only virtio-snd PCI function. If the device does not report
`REV_01` (or does not expose the modern virtio-snd PCI ID), the Aero driver will not bind and the selftest will
report virtio-snd as missing.

To explicitly skip virtio-snd:

```bat
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --disable-snd --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

If the VM has multiple disks (e.g. IDE boot disk + separate virtio data disk), you can force the virtio-blk test location:

```bat
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --blk-root D:\aero-virtio-selftest\ --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

The host harness expects the tool to run automatically and print a final `AERO_VIRTIO_SELFTEST|RESULT|...` line to COM1.
When the host harness attaches virtio-snd (`-WithVirtioSnd` / `--with-virtio-snd`), it also expects both
`AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS`, `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS`, and
`AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS` (not `SKIP`).

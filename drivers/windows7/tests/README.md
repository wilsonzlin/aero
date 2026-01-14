# Windows 7 virtio functional tests (QEMU harness)

This directory contains a **basic, automatable functional test harness** for the Windows 7 virtio drivers.

Goals:
- Run **end-to-end**, **repeatable** tests under **QEMU**.
- Validate **virtio-blk** (disk I/O), **virtio-net** (DHCP + outbound TCP), **virtio-input** (HID enumeration + optional
  end-to-end event delivery), and **virtio-snd** (audio endpoint enumeration + basic playback) without manual UI
  interaction.
- Produce logs over **COM1 serial** so the host can deterministically parse **PASS/FAIL**.
- Keep the structure extensible (more tests later).

Non-goals:
- No Windows images are committed (see [Image strategy](#image-strategy-no-redistribution)).

See also:

- virtio-input end-to-end test plan (device model + driver + web runtime): [`docs/virtio-input-test-plan.md`](../../../docs/virtio-input-test-plan.md)

## Layout

```
drivers/windows7/tests/
  guest-selftest/   # Win7 user-mode console tool: aero-virtio-selftest.exe
  host-harness/     # PowerShell scripts to boot QEMU + parse serial PASS/FAIL
  README.md         # (this file)
```

---

## Guest selftest tool

`guest-selftest/` builds `aero-virtio-selftest.exe`, a Win7 user-mode console tool that:
- Detects virtio devices via SetupAPI (hardware IDs like `VEN_1AF4` / `VIRTIO`).
- Runs a virtio-blk file I/O test (write/readback, sequential read, flush) on a **virtio-backed volume**.
- Includes an opt-in virtio-blk runtime resize test (`virtio-blk-resize`) that validates **dynamic capacity change**
  support (Windows should observe a larger disk size after the host grows the backing device at runtime).
  - By default the guest selftest reports `virtio-blk-resize|SKIP|flag_not_set`; provision the guest to run the selftest
    with `--test-blk-resize` / env var `AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE=1` to enable it.
  - When the host harness is run with `--with-blk-resize` / `-WithBlkResize`, it waits for the guest `READY` marker,
    issues a QMP resize (`blockdev-resize` with fallback to legacy `block_resize`), and requires `virtio-blk-resize|PASS`.
- Includes an opt-in virtio-blk miniport reset/recovery stability test (`virtio-blk-reset`) that validates the
  **driver reset path** end-to-end.
  - Disabled by default; provision the guest to run the selftest with `--test-blk-reset` / env var
    `AERO_VIRTIO_SELFTEST_TEST_BLK_RESET=1` to enable it.
  - When the host harness is run with `--with-blk-reset` / `-WithBlkReset`, it requires the guest `virtio-blk-reset`
    marker to `PASS` (treats `SKIP`/`FAIL`/missing as failure).
- Runs a virtio-net test (wait for DHCP, UDP echo roundtrip to the host harness, DNS resolve, HTTP GET).
- Runs a virtio-input HID sanity test (detect virtio-input HID devices + validate separate keyboard-only + mouse-only HID devices).
  - Also validates the underlying virtio-input PCI function(s) are bound to the expected driver service (`aero_virtio_input`)
    and have no PnP/ConfigManager errors:
    - summary marker: `virtio-input-bind`
    - detailed marker (service/pnp_id): `virtio-input-binding`
- Emits a `virtio-input-events` marker that can be used to validate **end-to-end input report delivery** when the host
  harness injects deterministic keyboard/mouse events via QMP (`input-send-event`).
  - This path reads HID input reports directly from the virtio-input HID interface so it does not depend on UI focus.
  - By default the guest selftest reports `virtio-input-events|SKIP|flag_not_set`; provision the guest to run the
    selftest with `--test-input-events` to enable it.
- Emits a `virtio-input-media-keys` marker that can be used to validate **end-to-end Consumer Control (media keys)**
  input report delivery when the host harness injects a deterministic media key via QMP (`input-send-event`).
  - By default the guest selftest reports `virtio-input-media-keys|SKIP|flag_not_set`; provision the guest to run the
    selftest with `--test-input-media-keys` to enable it.
- Emits an optional `virtio-input-led` (and legacy `virtio-input-leds`) marker that can be used to validate the virtio-input
  **statusq output path** end-to-end (user-mode HID output report write → KMDF HID minidriver → virtio statusq →
  device consumes/completes).
  - By default the guest selftest reports `virtio-input-led|SKIP|flag_not_set`; provision the guest to run the selftest
    with `--test-input-led` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_LED=1`) to enable it.
    - Legacy: `--test-input-leds` / env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_LEDS=1`.
- Emits a `virtio-input-tablet-events` marker that can be used to validate **end-to-end absolute pointer (tablet)**
  input report delivery when the host harness attaches a `virtio-tablet-pci` device and injects deterministic QMP
  `abs` + click events (`input-send-event`).
  - By default the guest selftest reports `virtio-input-tablet-events|SKIP|flag_not_set`; provision the guest to run the
    selftest with `--test-input-tablet-events` (alias: `--test-tablet-events`) / env var
    `AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1` / `AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1` to enable it.
    - Note: This requires that the virtio-input driver is installed and that the tablet device is bound so it exposes a
      HID interface.
    - For an **Aero contract tablet** (HWID `...&SUBSYS_00121AF4&REV_01`), the intended INF is
      `drivers/windows7/virtio-input/inf/aero_virtio_tablet.inf`.
    - `aero_virtio_tablet.inf` is the preferred binding for the contract tablet HWID. It is more specific than
      `aero_virtio_input.inf` and wins when both are installed and it matches.
    - If your QEMU/device does **not** expose the Aero contract tablet subsystem ID (`SUBSYS_0012`):
      - `aero_virtio_tablet.inf` will not match (tablet-SUBSYS-only).
      - The device can still bind via the opt-in strict fallback HWID `PCI\VEN_1AF4&DEV_1052&REV_01` if you enable the optional
        legacy alias INF (the `*.inf.disabled` file under `drivers/windows7/virtio-input/inf/`). It will appear as the generic
        **Aero VirtIO Input Device**.
      - Ensure the device reports `REV_01` (for QEMU, ensure `x-pci-revision=0x01` is in effect; the harness does this by default).
      - Preferred/contract path: emulate the contract tablet subsystem ID (`SUBSYS_00121AF4`) so the device binds via
        `aero_virtio_tablet.inf` and appears with the tablet device name.
      - The optional `*.inf.disabled` file under `drivers/windows7/virtio-input/inf/` exists for legacy compatibility and opt-in
        fallback binding:
        - It is allowed to diverge from `aero_virtio_input.inf` only in the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`)
          where it adds the strict generic fallback model line `PCI\VEN_1AF4&DEV_1052&REV_01` (no `SUBSYS`).
        - Outside those models sections, from the first section header onward it is expected to remain byte-identical
          (banner/comments may differ).
        - Enabling it does **change** HWID matching behavior (it enables strict generic fallback binding).
      - Do **not** install or ship both the canonical INF and the alias INF at the same time: they have overlapping bindings and
        can cause confusing/non-deterministic device selection.
      - Note: documentation under `drivers/windows7/tests/` intentionally avoids spelling deprecated legacy INF basenames.
        CI scans this tree for those strings. If you need to refer to a filename alias, refer to it generically as
        the `*.inf.disabled` file.
      - Caveat: avoid installing overlapping virtio-input INFs that can match the same HWIDs and steal device binding.
    - Once bound, the driver classifies the device as a tablet via `EV_BITS` (`EV_ABS` + `ABS_X`/`ABS_Y`).
- Optionally runs a virtio-snd test (PCI detection + endpoint enumeration + short playback) when a supported virtio-snd
  device is detected (or when `--require-snd` / `--test-snd` is set).
  - Detects the virtio-snd PCI function by hardware ID:
    - `PCI\VEN_1AF4&DEV_1059` (modern; Aero contract v1 requires `PCI\VEN_1AF4&DEV_1059&REV_01` for strict INF binding)
    - If the device enumerates as transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`; stock QEMU defaults), the selftest
      only accepts it when `--allow-virtio-snd-transitional` is set. In that mode, install the opt-in legacy package
      (`drivers/windows7/virtio-snd/inf/aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
- Also emits a `virtio-snd-capture` marker (capture endpoint detection + optional WASAPI capture smoke test).
- Also emits an informational `virtio-snd-format` marker exposing the negotiated endpoint mix formats (as visible via
  the Windows shared-mode mix format returned by `IAudioClient::GetMixFormat()`):
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=<...>|capture=<...>`
- When run with `--test-snd-buffer-limits` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`), also runs a
  WASAPI buffer sizing stress test and emits the marker `virtio-snd-buffer-limits` (PASS/FAIL).
- Logs to:
  - stdout
  - `C:\aero-virtio-selftest.log`
  - `COM1` (serial)

 The selftest emits machine-parseable markers:
  
  ```
  # virtio-blk includes interrupt diagnostics (from the miniport IOCTL query) as key/value fields:
   AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=0x0000|msix_queue_vector=0x0001

   # Optional: virtio-blk runtime resize (requires `--test-blk-resize` in the guest and host-side QMP resize):
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=<N>|old_bytes=<u64>|new_bytes=<u64>|elapsed_ms=<u32>

   # Optional: virtio-blk miniport reset/recovery stability test (requires `--test-blk-reset` in the guest):
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=flag_not_set
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=...|counter_after=...
   # AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=...|err=...
 
       AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...
        AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|devices=<n>
        AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=aero_virtio_input|pnp_id=<...>|hwid0=<...>
       AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet|SKIP|not_present|tablet_devices=0|tablet_collections=0
       AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set
       AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|flag_not_set
       AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP|flag_not_set
       AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|SKIP|flag_not_set
       AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set
 
  # Optional: end-to-end virtio-input event delivery (requires `--test-input-events` in the guest and host-side QMP injection):
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...

  # Optional: virtio-input keyboard LED/statusq output report writes (requires `--test-input-led` (legacy: `--test-input-leds`) in the guest):
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS|...
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS|writes=<n>

  # Optional: end-to-end virtio-input media keys (Consumer Control) event delivery (requires `--test-input-media-keys` in the guest and host-side QMP injection):
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|READY
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS|...

  # Optional: end-to-end virtio-input tablet (absolute pointer) event delivery (requires `--test-input-tablet-events` / `--test-tablet-events` in the guest and host-side QMP injection):
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY
  # AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|...

  # (virtio-snd is emitted as PASS/FAIL/SKIP depending on device/config):
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set
  # or:
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|...
  # Negotiated endpoint mix formats (diagnostic only):
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=...|capture=...
  # Optional: virtio-snd eventq counters (diagnostic only):
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=...|pcm_period=...|xrun=...|...
  AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS
  AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS
  # Checksum offload counters (via \\.\AeroVirtioNetDiag IOCTL):
  AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=...|rx_csum=...|fallback=...|...
  AERO_VIRTIO_SELFTEST|RESULT|PASS
```

The guest may also emit optional interrupt-mode diagnostics markers (informational):

- `virtio-<dev>-irq|INFO|...`
- `virtio-<dev>-irq|WARN|...`

 The host harness waits for the final `AERO_VIRTIO_SELFTEST|RESULT|...` line and also enforces that key per-test markers
(virtio-blk + virtio-input + virtio-input-bind + virtio-snd + virtio-snd-capture + virtio-net + virtio-net-udp) were emitted so older selftest binaries
can’t accidentally pass.

When the harness is run with:
- `-WithVirtioSnd` / `--with-virtio-snd`, `virtio-snd`, `virtio-snd-capture`, and `virtio-snd-duplex` must `PASS` (not `SKIP`).
  - If the guest fails virtio-snd due to the bring-up toggle `ForceNullBackend=1`, the host harness emits a deterministic token:
    `FAIL: VIRTIO_SND_FORCE_NULL_BACKEND: ...` (clear `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend` to `0`).
- `-WithSndBufferLimits` / `--with-snd-buffer-limits`, `virtio-snd-buffer-limits` must `PASS` (and a guest `SKIP|flag_not_set`
  or missing marker is treated as a hard failure).
- `-WithBlkResize` / `--with-blk-resize`, `virtio-blk-resize` must `PASS` (and a guest `SKIP|flag_not_set` or missing marker
  is treated as a hard failure). This path also triggers a runtime QMP resize.
- `-WithBlkReset` / `--with-blk-reset`, `virtio-blk-reset` must `PASS` (and a guest `SKIP`/`FAIL` or missing marker is treated
  as a hard failure). This requires the guest image provisioned with `--test-blk-reset`.
- `-RequireNetCsumOffload` / `--require-net-csum-offload`, the harness additionally requires `virtio-net-offload-csum` to report
  `PASS` and `tx_csum > 0` (at least one checksum-offloaded TX packet was observed).
- `--require-net-udp-csum-offload` (Python harness), the harness additionally requires `virtio-net-offload-csum` to report
  `PASS` and `tx_udp > 0` (at least one UDP checksum-offloaded TX packet was observed).

Note:
- The guest selftest also emits standalone IRQ diagnostic lines for `virtio-net` / `virtio-snd` / `virtio-input`:
  - `virtio-<dev>-irq|INFO|mode=intx`
  - `virtio-<dev>-irq|INFO|mode=msi|messages=<n>` (message interrupts; does not distinguish MSI vs MSI-X)
  - virtio-snd may also emit richer MSI-X diagnostics (when the driver exposes the optional `\\.\aero_virtio_snd_diag` interface):
    - `virtio-snd-irq|INFO|mode=msix|messages=<n>|msix_config_vector=0x....|...`
      - Includes per-queue MSI-X routing (`msix_queue0_vector..msix_queue3_vector`) and diagnostic counters
        (`interrupt_count`, `dpc_count`, `drain0..drain3`).
    - `virtio-snd-irq|INFO|mode=none|...` (polling-only; no interrupt objects are connected)
  The host harness mirrors these into `AERO_VIRTIO_WIN7_HOST|VIRTIO_*_IRQ_DIAG|...` markers for log scraping.
- When the guest includes `irq_*` fields on its per-test markers, the host harness may also emit stable per-device host
  markers for log scraping (informational by default):
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|...`
- The host harness may also emit additional summary/diagnostic markers (informational; do not affect PASS/FAIL), such as:
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|...`
  - `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|...`
- To assert that virtio-blk is actually using MSI/MSI-X (fail if the miniport reports INTx), run the guest
  selftest with `--expect-blk-msi` (or env var `AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1`). This is useful when
  running the host harness with explicit MSI-X vectors (`--virtio-msix-vectors` or per-device `--virtio-<dev>-vectors`)
  or when validating MSI/MSI-X support.
  - If you provision the guest via `host-harness/New-AeroWin7TestImage.ps1`, bake this into the scheduled task with
    `-ExpectBlkMsi` (adds `--expect-blk-msi`).
- virtio-snd is optional. When a supported virtio-snd PCI function is detected, the selftest exercises playback
  automatically (even without `--test-snd`). Use `--require-snd` / `--test-snd` to make missing virtio-snd fail the
  overall selftest, and `--disable-snd` to force skipping playback + capture.
  - For stock QEMU transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`), also pass `--allow-virtio-snd-transitional` and
    install the legacy virtio-snd package (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
- Capture is reported separately via the `virtio-snd-capture` marker. Missing capture is `SKIP` by default unless
  `--require-snd-capture` is set. Use `--test-snd-capture` (or env var `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`) to
  force the capture smoke test (older selftest binaries required this; newer ones run it automatically when virtio-snd
  is present). Use `--disable-snd-capture` to skip capture-only checks while still exercising playback.
  - When capture is enabled, the selftest also emits a `virtio-snd-format` INFO marker reporting the render + capture
    shared-mode mix formats (useful when running against non-contract virtio-snd devices that negotiate optional
    formats/rates).

## Optional/Compatibility Features

The core Win7 harness is designed to validate the **strict contract v1 required behavior** first, but it also has
hooks for diagnosing *optional* features when running against non-contract virtio implementations (for example, stock
QEMU).

These optional diagnostics are not currently treated as hard PASS/FAIL gates unless explicitly enabled by the harness
configuration.

### MSI-X (interrupt mode diagnostics)

- Contract v1 requires INTx and permits MSI-X only as an optional enhancement.
- The host harness includes a best-effort parser for virtio-blk interrupt-mode diagnostics when the guest selftest
  includes interrupt-related key/value fields on the `virtio-blk` marker:
  - Guest marker (example): `AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|irq_message_count=2|msix_config_vector=0x0000|msix_queue_vector=0x0001`
  - Host marker: `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|irq_message_count=2|msix_config_vector=0x0000|msix_queue_vector=0x0001`
- This host marker is **diagnostic only** (it does not currently affect overall PASS/FAIL).
- Newer guest selftest binaries also emit dedicated MSI-X routing markers; the harness mirrors them into stable host markers:
  - virtio-blk:
    - Guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS/SKIP|mode=...|messages=...|config_vector=...|queue_vector=...`
    - Host marker: `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MSIX|PASS/SKIP|mode=...|messages=...|config_vector=...|queue_vector=...`
  - virtio-net:
    - Guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n\|none>|rx_vector=<n\|none>|tx_vector=<n\|none>|...`
    - Host marker: `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n\|none>|rx_vector=<n\|none>|tx_vector=<n\|none>|...`
    - Additional diagnostic fields may be appended by newer driver/selftest builds (best-effort), for example:
      `flags=0x...|intr0=...|dpc0=...|rx_drained=...|...`
  - virtio-snd:
    - Guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n\|none>|queue0_vector=<n\|none>|queue1_vector=<n\|none>|queue2_vector=<n\|none>|queue3_vector=<n\|none>|interrupts=<n>|dpcs=<n>|drain0=<n>|drain1=<n>|drain2=<n>|drain3=<n>|...`
    - Host marker: `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n\|none>|queue0_vector=<n\|none>|queue1_vector=<n\|none>|queue2_vector=<n\|none>|queue3_vector=<n\|none>|interrupts=<n>|dpcs=<n>|drain0=<n>|drain1=<n>|drain2=<n>|drain3=<n>|...`
  - virtio-input:
    - Guest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS/FAIL/SKIP|mode=...|messages=...|mapping=...|...`
    - Host marker: `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|mapping=...|...`

To intentionally exercise MSI-X paths (and optionally **require** MSI-X):

- Request a larger MSI-X table size from QEMU (requires QEMU virtio `vectors` property):
  - PowerShell (global): `-VirtioMsixVectors N`
  - PowerShell (per device): `-VirtioNetVectors N`, `-VirtioBlkVectors N`, `-VirtioInputVectors N`, `-VirtioSndVectors N`
  - Python (global): `--virtio-msix-vectors N`
  - Python (per device): `--virtio-net-vectors N`, `--virtio-blk-vectors N`, `--virtio-input-vectors N`, `--virtio-snd-vectors N`
- Fail the harness if QEMU reports MSI-X **disabled** (QMP introspection check):
  - PowerShell: `-RequireVirtioBlkMsix` / `-RequireVirtioNetMsix` / `-RequireVirtioSndMsix`
  - Python: `--require-virtio-blk-msix` / `--require-virtio-net-msix` / `--require-virtio-snd-msix`
- When `RequireVirtio*Msix` is enabled, the harness may also require a **guest-side** marker confirming the *effective*
  interrupt mode (end-to-end):
  - `virtio-net`: `AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|...`
  - `virtio-blk`: `AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...`
  - `virtio-snd`: `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...`
  - `virtio-input`: `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|...`
    - PowerShell: `-RequireVirtioInputMsix`
    - Python: `--require-virtio-input-msix`
    - Optional guest-side hard requirement:
      - Guest selftest: `--require-input-msix` (or `AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1`)
      - Provisioning: `New-AeroWin7TestImage.ps1 -RequireInputMsix` (alias: `-RequireVirtioInputMsix`)
- For virtio-blk specifically, you can also make MSI/MSI-X a **guest-side** hard requirement:
  - Guest selftest: `--expect-blk-msi` (or `AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1`)
  - Provisioning: `New-AeroWin7TestImage.ps1 -ExpectBlkMsi`
- For virtio-net specifically, you can also make MSI-X a **guest-side** hard requirement:
  - Guest selftest: `--require-net-msix` (or `AERO_VIRTIO_SELFTEST_REQUIRE_NET_MSIX=1`)
  - Provisioning: `New-AeroWin7TestImage.ps1 -RequireNetMsix` (alias: `-RequireVirtioNetMsix`)
- For virtio-snd specifically, you can also make MSI-X a **guest-side** hard requirement:
  - Guest selftest: `--require-snd-msix` (or `AERO_VIRTIO_SELFTEST_REQUIRE_SND_MSIX=1`)
  - Provisioning: `New-AeroWin7TestImage.ps1 -RequireSndMsix` (alias: `-RequireVirtioSndMsix`)

### virtio-net “offload-sensitive” large transfers

virtio-net offloads (checksum/TSO/GSO) are **out of scope** for the strict Aero contract v1 device model, but are
commonly implemented by other virtio-net devices and can affect performance/robustness.

The guest selftest includes deterministic 1 MiB download/upload transfers and reports:

- `large_ok`, `large_bytes`, `large_fnv1a64`, `large_mbps`
- `upload_ok`, `upload_bytes`, `upload_mbps`

These fields can be used to compare performance across hypervisors/device configurations (for example, when toggling
virtio-net offload-related device properties on QEMU via `QemuExtraArgs` / extra CLI args).

### virtio-snd `eventq`

Contract v1 reserves virtio-snd `eventq` for future use and forbids the driver from depending on events. For
compatibility, the Win7 virtio-snd driver still initializes `eventq` and tolerates event traffic best-effort.

When present (non-contract, e.g. newer device models), the driver may also treat some standard virtio-snd PCM events as
**optional enhancements** while still keeping the WaveRT pacing loop **timer-driven** as the baseline:

- `VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED`: can act as an additional WaveRT period/DPC wakeup source (with duplicate coalescing).
- `VIRTIO_SND_EVT_PCM_XRUN`: can act as a hint to attempt best-effort stream recovery (typically `STOP`/`START`).

The harness validates correct render/capture/duplex behavior under QEMU; for eventq-specific debugging, use the
virtio-snd `DebugLogs` build and capture kernel debug output while running the selftest.

Newer guest selftest binaries also emit an informational marker reporting eventq counters:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=...|pcm_period=...|xrun=...|...`

The host harness mirrors this into a stable host-side marker for log scraping:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|INFO/SKIP|...`

### virtio-input QEMU compatibility mode (`CompatIdName`)

The Win7 virtio-input driver is **strict by default** (Aero contract v1): it expects the Aero `ID_NAME` strings and
contract `ID_DEVIDS` values.

Stock QEMU virtio-input devices often report non-Aero `ID_NAME` strings (for example `QEMU Virtio Keyboard`) and may
not satisfy strict `ID_DEVIDS` validation. If the virtio-input driver fails to start under QEMU (Code 10 /
`STATUS_NOT_SUPPORTED`) due to identifier mismatches, enable the driver's opt-in compatibility mode:

```bat
reg add HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters ^
  /v CompatIdName /t REG_DWORD /d 1 /f
```

Then reboot (or disable/enable the device).

To bake this into a guest image created by the in-tree provisioning scripts, generate provisioning media with:

- `New-AeroWin7TestImage.ps1 -EnableVirtioInputCompatIdName` (alias: `-EnableVirtioInputCompat`)

Disable compat mode when validating strict Aero contract conformance.

### Building (Windows)

See `guest-selftest/README.md`.

Note: The virtio-blk test requires a **mounted** virtio-backed volume. If the guest boots from a non-virtio disk,
attach an additional virtio disk with a drive letter (or run the selftest with `--blk-root <path>`).

---

## Host harness (PowerShell + QEMU)

`host-harness/Invoke-AeroVirtioWin7Tests.ps1`:
- Starts a tiny HTTP server on the host (loopback), reachable from the guest as `10.0.2.2`.
- Launches QEMU with:
  - virtio-blk disk (**modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
  - virtio-net NIC (user-mode networking / slirp; **modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
  - virtio-input keyboard + mouse devices (`virtio-keyboard-pci`, `virtio-mouse-pci`; **modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
  - (optional) virtio-input tablet device (`virtio-tablet-pci`):
    - attach-only: `-WithVirtioTablet` / `--with-virtio-tablet`
    - end-to-end tablet report delivery test: `-WithInputTabletEvents` / `-WithTabletEvents` / `--with-input-tablet-events` / `--with-tablet-events`
    (**modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01` in default mode)
  - (optional) virtio-snd device (when enabled via `-WithVirtioSnd` / `--with-virtio-snd`; **modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)

Note: Stock QEMU virtio-input devices report virtio-input `ID_NAME` strings like `QEMU Virtio Keyboard` / `QEMU Virtio Mouse`.
The in-tree Aero virtio-input driver is strict-by-default and expects the Aero contract `ID_NAME` values, so for QEMU-based
testing enable the driver's ID_NAME compatibility mode:

- `HKLM\SYSTEM\CurrentControlSet\Services\aero_virtio_input\Parameters\CompatIdName` = `1` (REG_DWORD)

The provisioning media generator (`host-harness/New-AeroWin7TestImage.ps1`) can bake this in via:

- `-EnableVirtioInputCompatIdName` (alias: `-EnableVirtioInputCompat`)

- COM1 redirected to a host log file
  - Parses the serial log for `AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL` and requires per-test markers for
   virtio-blk + virtio-input + virtio-snd + virtio-snd-capture + virtio-net when RESULT=PASS is seen.
   - When `-WithSndBufferLimits` / `--with-snd-buffer-limits` is enabled, the harness also requires the guest marker
     `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS` (provision the guest with `--test-snd-buffer-limits` or
     env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`).
  - When `-WithInputEvents` (alias: `-WithVirtioInputEvents`) / `--with-input-events` (alias: `--with-virtio-input-events`)
    is enabled, the harness also injects a small keyboard + mouse sequence via QMP (prefers `input-send-event`, with backcompat fallbacks when unavailable) and requires
    `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS`.
    - Note: this requires a guest image provisioned with `--test-input-events` so the guest selftest enables the
      `virtio-input-events` read loop (otherwise the guest reports `...|SKIP|flag_not_set`).
      When `-WithInputEvents` / `--with-input-events` is enabled, that SKIP causes the harness to fail
      (PowerShell: `VIRTIO_INPUT_EVENTS_SKIPPED`; Python: `FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: ...`).
      If the guest reports `...|virtio-input-events|FAIL|...`, the harness fails
      (PowerShell: `VIRTIO_INPUT_EVENTS_FAILED`; Python: `FAIL: VIRTIO_INPUT_EVENTS_FAILED: ...`).
    - Note: if the guest selftest does not emit any `virtio-input-events` marker at all (READY/SKIP/PASS/FAIL) after
      completing `virtio-input`, the harness fails early (PowerShell: `MISSING_VIRTIO_INPUT_EVENTS`; Python:
      `FAIL: MISSING_VIRTIO_INPUT_EVENTS: ...`). Update/re-provision the guest selftest binary.
      If QMP input injection fails, the harness fails (PowerShell: `QMP_INPUT_INJECT_FAILED`; Python:
      `FAIL: QMP_INPUT_INJECT_FAILED: ...`).
    - The harness also emits a host marker for the injection step itself:
      - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback>|kbd_mode=device/broadcast|mouse_mode=device/broadcast`
      - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback|unknown>|reason=...`
      - Note: The harness may retry injection a few times after `virtio-input-events|READY` to reduce timing flakiness.
        In that case you may see multiple `VIRTIO_INPUT_EVENTS_INJECT|PASS` lines (the marker includes `attempt=<n>` and `backend=...`).
    - When `-WithInputWheel` (alias: `-WithVirtioInputWheel`) / `--with-input-wheel`
      (aliases: `--with-virtio-input-wheel`, `--require-virtio-input-wheel`, `--enable-virtio-input-wheel`) is enabled,
      the harness also injects vertical + horizontal scroll wheel events
      and requires `AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS`.
      - Note: this still requires the guest image provisioned with `--test-input-events` (wheel runs as part of the virtio-input-events flow).
    - When `-WithInputEventsExtended` / `--with-input-events-extended` is enabled, the harness injects additional keyboard/mouse inputs and requires:
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS`
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS`
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS`
      This requires provisioning the guest with `--test-input-events-extended` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1`;
      if using `New-AeroWin7TestImage.ps1`, pass `-TestInputEventsExtended` (alias: `-TestInputEventsExtra`)).
    - When `-WithInputMediaKeys` / `--with-input-media-keys` is enabled, the harness injects a deterministic media key sequence and requires:
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS`
      This requires provisioning the guest with `--test-input-media-keys` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_MEDIA_KEYS=1`;
      if using `New-AeroWin7TestImage.ps1`, pass `-TestInputMediaKeys` (alias: `-TestMediaKeys`)).
      - The harness also emits a host marker for the injection step itself:
        - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback>|kbd_mode=device/broadcast`
        - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback|unknown>|reason=...`
    - When `-WithInputLed` / `--with-input-led` is enabled, the harness requires the guest marker:
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS`
      This requires provisioning the guest with `--test-input-led` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_LED=1`;
      if using `New-AeroWin7TestImage.ps1`, pass `-TestInputLed`).
      - This test is guest-driven (HID keyboard LED output reports → virtio-input statusq) and does not require QMP injection.
      - Compatibility: `-WithInputLeds` / `--with-input-leds` instead requires `AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS|writes=<n>`
        (provision the guest with `--test-input-leds` / env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_LEDS=1`; if using
        `New-AeroWin7TestImage.ps1`, pass `-TestInputLeds`).
    - When `-WithInputTabletEvents` (aliases: `-WithVirtioInputTabletEvents`, `-WithTabletEvents`) /
      `--with-input-tablet-events` (aliases: `--with-virtio-input-tablet-events`, `--with-tablet-events`) is enabled,
      the harness attaches `virtio-tablet-pci`, injects a
      deterministic absolute-pointer move + click sequence via QMP (`input-send-event`), and requires
      `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS`.
      - Note: this requires a guest image provisioned with `--test-input-tablet-events` (alias: `--test-tablet-events`) /
        env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1` / `AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1` so the
        guest selftest enables the `virtio-input-tablet-events` read loop (otherwise the guest reports
        `...|SKIP|flag_not_set` and the harness fails).
      - The harness also emits a host marker for the injection step itself:
        - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt=<n>|backend=<qmp_input_send_event>|tablet_mode=device/broadcast`
        - `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=<n>|backend=<qmp_input_send_event|unknown>|reason=...`
  - Exits with `0` on PASS, non-zero on FAIL/timeout.

The harness also sets the PCI **Revision ID** (`x-pci-revision=0x01`) to match the
[`AERO-W7-VIRTIO` v1 contract](../../../docs/windows7-virtio-driver-contract.md). Newer Aero drivers may refuse to bind
if the Revision ID does not match.

To catch QEMU/device-arg misconfiguration early (for example, if your QEMU build ignores `x-pci-revision=0x01` and still
advertises `REV_00`), the harness includes an optional host-side QMP `query-pci` preflight:

- PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -QemuPreflightPci` (alias: `-QmpPreflightPci`)
- Python: `invoke_aero_virtio_win7_tests.py --qemu-preflight-pci` (alias: `--qmp-preflight-pci`)

On success, the harness emits a CI-scrapable host marker:

- `AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=...|vendor=1af4|devices=...`

For Linux/CI environments, `host-harness/invoke_aero_virtio_win7_tests.py` provides the same behavior without requiring PowerShell.

For debugging (or CI log introspection), both harnesses support a dry-run mode that prints the computed QEMU argv
without starting the HTTP/QMP servers or launching QEMU:

- PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -DryRun` (alias: `-PrintQemuArgs`)
- Python: `invoke_aero_virtio_win7_tests.py --dry-run` (aliases: `--print-qemu`, `--print-qemu-cmd`)

This repo also includes an **opt-in** self-hosted GitHub Actions workflow wrapper around the Python harness:

  - [`.github/workflows/win7-virtio-harness.yml`](../../../.github/workflows/win7-virtio-harness.yml)
    - Set `qemu_preflight_pci=true` to enable the optional host-side QMP `query-pci` preflight (validates QEMU-emitted `VEN/DEV/REV`).
  - Use workflow inputs `with_virtio_input_events=true`, `with_virtio_input_wheel=true`, `with_virtio_input_media_keys=true`,
    `with_virtio_input_led=true` (compat: `with_virtio_input_leds=true`), `with_virtio_input_events_extended=true`, and/or
    `with_virtio_input_tablet_events=true` to enable optional end-to-end virtio-input tests.
    (Requires a guest image provisioned with `--test-input-events` for events/wheel, `--test-input-media-keys` for media keys, also
    `--test-input-led` for LED/statusq (compat: `--test-input-leds`), also `--test-input-events-extended` for the extended markers,
    and `--test-input-tablet-events` (alias: `--test-tablet-events`) for tablet.)
  - To require the virtio-snd buffer limits stress test, set `with_virtio_snd=true` and `with_snd_buffer_limits=true` (requires a
    guest image provisioned with `--test-snd-buffer-limits` / env var `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`).

See `host-harness/README.md` for required prerequisites and usage.

---

## Image strategy (no redistribution)

**Do not commit Windows ISOs or disk images.**

You must provide one of:
1) A user-supplied Windows 7 ISO + license key, or
2) An already-installed Win7 disk image (qcow2/raw/vhdx) you own.

The repository includes scripts + documentation to:
- copy/install the Aero virtio drivers
- install `aero-virtio-selftest.exe`
- configure automatic execution on boot (Task Scheduler recommended)

See `host-harness/README.md` for a recommended provisioning approach.

For a standardized QEMU command line to perform an interactive Windows 7 installation from your own ISO (with an attached provisioning ISO), see:
- `host-harness/Start-AeroWin7Installer.ps1`

---

## Extensibility hooks

The guest tool is structured so adding more tests is straightforward:

### virtio-snd
- Enumerate audio render endpoints via MMDevice API and log them (friendly name + device ID).
- Select the virtio-snd endpoint by friendly name substring and/or hardware ID.
- Start a shared-mode WASAPI render stream and play a short deterministic tone (440Hz), with a waveOut fallback.
- Runs when a supported virtio-snd device is detected. Use `--require-snd` / `--test-snd` to fail if missing, or
  `--disable-snd` to force `SKIP`.

### virtio-input
- Enumerate HID devices via SetupAPI/HIDClass.
- Validate virtio-input HID report descriptors correspond to separate keyboard and mouse devices.

When adding tests:
- Emit `AERO_VIRTIO_SELFTEST|TEST|<name>|PASS/FAIL/SKIP|...` lines.
- Keep each test independently pass/fail/skip so the harness can report granular failures.

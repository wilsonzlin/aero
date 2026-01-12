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
    - To enable the optional end-to-end virtio-input event delivery smoke test (HID input reports),
      the guest selftest must be provisioned with `--test-input-events` (or env var
      `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS=1`).
  - has virtio-snd installed if you intend to test audio
    - the guest selftest will exercise virtio-snd playback automatically when a virtio-snd device is present and confirm
      a capture endpoint is registered
    - when the guest is provisioned with `--test-snd-capture`, the selftest also runs a full-duplex regression test
      (`virtio-snd-duplex`) that runs render + capture concurrently
    - use `--disable-snd` to skip virtio-snd testing, or `--test-snd` / `--require-snd` to fail if the device is missing
    - use `--disable-snd-capture` to skip capture-only checks (playback still runs when the device is present);
      do not use this when running the harness with `-WithVirtioSnd` / `--with-virtio-snd` (capture is required)
  - has `aero-virtio-selftest.exe` installed
  - runs the selftest automatically on boot and logs to `COM1`
  - has at least one **mounted/usable virtio-blk volume** (the selftest writes a temporary file to validate disk I/O)

For the in-tree clean-room Aero virtio driver stack, the canonical INF names are:

- `aero_virtio_blk.inf`
- `aero_virtio_net.inf`
- `aero_virtio_input.inf` (optional)
- `aero_virtio_snd.inf` (optional)

## Running tests

Example:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -SerialLogPath ./win7-serial.log `
  -TimeoutSeconds 600
```

For repeatable runs without mutating the base image, use snapshot mode:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -Snapshot `
  -TimeoutSeconds 600
```

### virtio-input event delivery (QMP input injection)

The default virtio-input selftest (`virtio-input`) validates **enumeration + report descriptors** only.
To regression-test **actual input event delivery** (virtio queues → KMDF HID → user-mode `ReadFile`), the guest
includes an optional `virtio-input-events` section that reads real HID input reports.

To enable end-to-end testing:

1. Provision the guest image so the scheduled selftest runs with `--test-input-events`
   (for example via `New-AeroWin7TestImage.ps1 -TestInputEvents`).
2. Run the host harness with `-WithInputEvents` (alias: `-WithVirtioInputEvents`) / `--with-input-events`
   (alias: `--with-virtio-input-events`) so it injects keyboard/mouse events via QMP (`input-send-event`) and
   requires the guest marker:
   `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...`

When enabled, the harness:

1. Waits for the guest readiness marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY`
2. Injects a deterministic input sequence via QMP `input-send-event`:
   - keyboard: `'a'` press + release
   - mouse: relative move + left click
3. Requires the guest marker `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...`

The harness also emits a host-side marker for the injection step itself (useful for debugging flaky setups and for log
scraping in CI):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt=<n>|kbd_mode=device/broadcast|mouse_mode=device/broadcast`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt=<n>|reason=...`

Note: The harness may retry input injection a few times after the guest reports `virtio-input-events|READY` to reduce
timing flakiness (input reports can be dropped if no user-mode read is pending). In that case you may see multiple
`VIRTIO_INPUT_EVENTS_INJECT|PASS` lines (the marker includes `attempt=<n>`).

Note: On some QEMU builds, `input-send-event` may not accept the `device=` routing parameter. In that case the harness
falls back to broadcasting the input events and reports `kbd_mode=broadcast` / `mouse_mode=broadcast` in the marker.

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

Note: If the guest was provisioned without `--test-input-events`, it will emit:
`AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set`.
The host harness only requires `virtio-input-events|PASS` when `-WithInputEvents` / `--with-input-events` is set.

Note: If the guest selftest is too old (or otherwise misconfigured) and does not emit any `virtio-input-events`
marker at all (READY/SKIP/PASS/FAIL) after completing `virtio-input`, the harness fails early with a
failure (PowerShell: `MISSING_VIRTIO_INPUT_EVENTS`). Update/re-provision the guest selftest binary.
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

Note: When `-WithVirtioSnd` / `--with-virtio-snd` is enabled, the host harness expects the guest selftest to run:

- virtio-snd playback (`AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS`)
- virtio-snd capture endpoint checks (`...|virtio-snd-capture|PASS`)
- virtio-snd full-duplex regression (`...|virtio-snd-duplex|PASS`)

The duplex test runs only when the guest selftest is provisioned with `--test-snd-capture` (or equivalent).
If your image was provisioned without capture smoke testing enabled, the guest will emit
`virtio-snd-duplex|SKIP|flag_not_set` and the host harness will fail with a `...DUPLEX_SKIPPED` reason.

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

To attach a virtio-snd device (virtio-sound-pci) during the run, enable it explicitly with `-WithVirtioSnd` / `--with-virtio-snd`.
(Aliases `-EnableVirtioSnd` / `--enable-virtio-snd` are also accepted.)

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
- Logs artifact: `win7-virtio-harness-logs` (serial + harness output + HTTP request log; QEMU stderr sidecar when present)

### Runner setup

On the self-hosted runner you need:

- QEMU available on `PATH` (or pass the absolute path via the workflow input `qemu_system`)
- Python 3 (`python3`)
- a prepared Win7 disk image available at a stable path on the runner (pass via the workflow input `disk_image_path`)

> Note: The harness uses a fixed localhost HTTP port (default `18080`). The workflow enforces
> `concurrency.group: win7-virtio-harness` to prevent concurrent runs from fighting over ports/images.
> If `18080` is already in use on your runner, override it via the workflow input `http_port`.

The workflow can also optionally exercise the end-to-end virtio-input event delivery path (QMP `input-send-event` + guest
HID report verification) by setting the workflow input `with_virtio_input_events=true`.
This requires a guest image provisioned with `--test-input-events` (for example via
`New-AeroWin7TestImage.ps1 -TestInputEvents`) so the guest selftest enables the `virtio-input-events` read loop.

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
- Launches QEMU with:
  - `-chardev file,...` + `-serial chardev:...` (guest COM1 → host log)
  - `virtio-net-pci,disable-legacy=on,x-pci-revision=0x01` with `-netdev user` (modern-only; enumerates as `PCI\VEN_1AF4&DEV_1041&REV_01`)
  - `virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01` + `virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01` (virtio-input; modern-only; enumerates as `PCI\VEN_1AF4&DEV_1052&REV_01`)
  - `-drive if=none,id=drive0` + `virtio-blk-pci,drive=drive0,disable-legacy=on,x-pci-revision=0x01` (modern-only; enumerates as `PCI\VEN_1AF4&DEV_1042&REV_01`)
  - (optional) `virtio-snd` PCI device when `-WithVirtioSnd` / `--with-virtio-snd` is set (`disable-legacy=on,x-pci-revision=0x01`; modern-only; enumerates as `PCI\VEN_1AF4&DEV_1059&REV_01`)
- Watches the serial log for:
  - `AERO_VIRTIO_SELFTEST|RESULT|PASS` / `AERO_VIRTIO_SELFTEST|RESULT|FAIL`
  - When `RESULT|PASS` is seen, the harness also requires that the guest emitted per-test markers for:
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS`
    - (only when `-WithInputEvents` / `--with-input-events` is enabled) `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS` or `...|SKIP` (if `-WithVirtioSnd` / `--with-virtio-snd` is set, it must be `PASS`)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS` or `...|SKIP` (if `-WithVirtioSnd` / `--with-virtio-snd` is set, it must be `PASS`)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS` or `...|SKIP` (if `-WithVirtioSnd` / `--with-virtio-snd` is set, it must be `PASS`)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS`

The Python/PowerShell harnesses also emit an additional host-side marker after the run for log scraping:

```
AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS/FAIL/INFO|large_ok=...|large_bytes=...|large_fnv1a64=...|large_mbps=...|upload_ok=...|upload_bytes=...|upload_mbps=...
```

This mirrors the guest's virtio-net marker fields when present and does not affect overall PASS/FAIL.

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
- virtio-input: `drivers/windows7/virtio-input/inf/aero_virtio_input.inf` (binds `DEV_1052&REV_01`)
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
    "aero_virtio_snd.inf"
  ) `
  -BlkRoot "D:\aero-virtio-selftest\"
```

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

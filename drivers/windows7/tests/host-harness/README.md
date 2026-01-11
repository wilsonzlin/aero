# Host harness (PowerShell) for Win7 virtio selftests

This directory contains the host-side scripts used to run the Windows 7 guest selftests under QEMU and return a deterministic PASS/FAIL exit code.

## Prerequisites

- QEMU (`qemu-system-x86_64` and optionally `qemu-img`)
  - Must support `disable-legacy=on` for modern-only virtio-pci devices
  - Must support `x-pci-revision=0x01` so devices match the Aero contract v1 revision
- PowerShell:
  - Windows PowerShell 5.1 or PowerShell 7+ should work
- A **prepared Windows 7 image** that:
  - has the virtio drivers installed (virtio-blk + virtio-net + virtio-input, modern-only)
  - has `aero-virtio-selftest.exe` installed
  - runs the selftest automatically on boot and logs to `COM1`
  - has at least one **mounted/usable virtio-blk volume** (the selftest writes a temporary file to validate disk I/O)

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

### Optional: attach virtio-snd (audio)

If your test image includes the virtio-snd driver **and is provisioned to run the guest virtio-snd test**
(via `aero-virtio-selftest.exe --test-snd` / `--require-snd`), you can ask the harness to attach a virtio-snd PCI device:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -WithVirtioSnd `
  -TimeoutSeconds 600
```

The harness uses QEMU’s `-audiodev none,...` backend so it remains headless/CI-friendly.

On success, the script returns exit code `0` and prints:

```
PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS
```

On failure/timeout, it returns non-zero and prints the matching failure reason.

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
  --timeout-seconds 600 \
  --with-virtio-snd \
  --snapshot
```

Add `--follow-serial` to stream COM1 serial output while waiting.

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

- Verification requires the **guest virtio-snd selftest** to actually run (use an image provisioned with the virtio-snd
  driver; to enable the guest test section, provision with `--test-snd` / `--require-snd` (for example via
  `New-AeroWin7TestImage.ps1 -RequireSnd`)).
- The harness prints a single-line marker suitable for log scraping:
  `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_WAV|PASS|...` or `...|FAIL|reason=<...>`.

## Running in CI (self-hosted)

This repo includes an **opt-in** GitHub Actions workflow that runs the host harness on a **self-hosted** runner:

- Workflow: [`.github/workflows/win7-virtio-harness.yml`](../../../../.github/workflows/win7-virtio-harness.yml)
- Trigger: `workflow_dispatch` only (no automatic PR runs)

Because Windows images cannot be redistributed, the workflow expects a **pre-provisioned Win7 disk image** to already
exist on the self-hosted runner.

Required workflow inputs:

- `disk_image_path` (required): path on the self-hosted runner to your prepared Win7 image (qcow2 recommended)
- `timeout_seconds`: harness timeout (default `600`)
- `snapshot`: run QEMU with `snapshot=on` so the base image is not modified (recommended)
- `serial_log_path`: where to write COM1 output (default is under `out/win7-virtio-harness/` in the workspace)
- `with_virtio_snd`: attach a virtio-snd device (default `false`)

On completion, the workflow uploads the serial log and harness output as the `win7-virtio-harness` artifact.

## How the harness works

- Starts a tiny HTTP server on `127.0.0.1:<HttpPort>`
  - QEMU slirp/user networking exposes host as `10.0.2.2` inside the guest, so the guest can HTTP GET `http://10.0.2.2:<HttpPort>/aero-virtio-selftest`.
- Launches QEMU with:
  - `-chardev file,...` + `-serial chardev:...` (guest COM1 → host log)
  - `virtio-net-pci,disable-legacy=on,x-pci-revision=0x01` with `-netdev user` (modern-only; enumerates as `PCI\VEN_1AF4&DEV_1041`)
  - `virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01` + `virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01` (virtio-input; modern-only; enumerates as `PCI\VEN_1AF4&DEV_1052`)
  - `-drive if=none,id=drive0` + `virtio-blk-pci,drive=drive0,disable-legacy=on,x-pci-revision=0x01` (modern-only; enumerates as `PCI\VEN_1AF4&DEV_1042`)
  - (optional) `virtio-snd` PCI device when `-WithVirtioSnd` / `--with-virtio-snd` is set (adds `disable-legacy=on` and `x-pci-revision=0x01` when supported)
- Watches the serial log for:
  - `AERO_VIRTIO_SELFTEST|RESULT|PASS` / `AERO_VIRTIO_SELFTEST|RESULT|FAIL`
  - When `RESULT|PASS` is seen, the harness also requires that the guest emitted per-test markers for:
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS` or `...|SKIP`

### Why `x-pci-revision=0x01`?

The Aero Windows 7 virtio device contract encodes the **contract major version** in the PCI
Revision ID (contract v1 = `0x01`).

Some QEMU virtio device types report `REV_00` by default. Once the Aero drivers enforce the
contract Revision ID, they will refuse to bind unless QEMU is told to advertise `REV_01`.

The harness sets `disable-legacy=on` for virtio-net/virtio-blk/virtio-input (and virtio-snd when supported) so QEMU does **not** expose
the legacy I/O-port transport (transitional devices enumerate as `DEV_1000/DEV_1001/DEV_1011`). This matches
[`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md) (`AERO-W7-VIRTIO` v1),
which is modern-only.

#### Verifying what your QEMU build reports (no guest required)

You can probe the PCI IDs (including Revision ID) that your local QEMU build advertises for the harness devices with:

```bash
python3 drivers/windows7/tests/host-harness/probe_qemu_virtio_pci_ids.py --qemu-system qemu-system-x86_64 --mode default
python3 drivers/windows7/tests/host-harness/probe_qemu_virtio_pci_ids.py --qemu-system qemu-system-x86_64 --mode contract-v1
```

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

Note: the harness uses **modern-only** virtio device IDs (`DEV_1041`/`DEV_1042`/`DEV_1052`/`DEV_1059`).
For virtio-blk/virtio-net, use the contract-v1 drivers under `drivers/win7/virtio-blk/` and
`drivers/win7/virtio-net/` (the legacy/transitional packages under `drivers/windows7/virtio/` bind
`DEV_1000`/`DEV_1001` and will not match when `disable-legacy=on`).

For fully repeatable provisioning, pass `-InfAllowList` explicitly:

`New-AeroWin7TestImage.ps1` also supports baking `--blk-root` into the installed scheduled task (useful if the VM boots
from a non-virtio disk but has a separate virtio data volume):

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aerovblk.inf",
    "aerovnet.inf",
    "virtio-input.inf",
    "aero-virtio-snd.inf"
  ) `
  -BlkRoot "D:\aero-virtio-selftest\"
```

By default the guest selftest **skips** the virtio-snd section unless it is enabled via `--test-snd` / `--require-snd`.
To exercise virtio-snd, make sure you:
- include the virtio-snd driver in the drivers directory you provision into the guest,
- provision the scheduled task with `--test-snd` / `--require-snd` (for example via `New-AeroWin7TestImage.ps1 -RequireSnd`), and
- attach a virtio-snd device when running the harness (`-WithVirtioSnd` / `--with-virtio-snd`).

To disable the guest selftest's virtio-snd section even if a device is present (adds `--disable-snd` to the scheduled task):

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aerovblk.inf",
    "aerovnet.inf",
    "virtio-input.inf",
    "aero-virtio-snd.inf"
  ) `
  -DisableSnd
```

If your `-DriversDir` contains duplicate INF basenames, disambiguate by passing a relative path (e.g.
`"win7\\virtio-net\\x64\\aerovnet.inf"` when using `out/packages`). To restore the legacy "install everything" behavior for debugging, pass `-InstallAllInfs`.

### Enabling test-signing mode (unsigned / test-signed drivers)

On Windows 7 x64, kernel drivers must be signed (or the machine must be in test-signing mode).

If your Aero virtio drivers are not yet production-signed, `New-AeroWin7TestImage.ps1` can embed a `bcdedit /set testsigning on`
step into the provisioning script:

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -InfAllowList @(
    "aerovblk.inf",
    "aerovnet.inf",
    "virtio-input.inf",
    "aero-virtio-snd.inf"
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

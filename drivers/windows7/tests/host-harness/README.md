# Host harness (PowerShell) for Win7 virtio selftests

This directory contains the host-side scripts used to run the Windows 7 guest selftests under QEMU and return a deterministic PASS/FAIL exit code.

## Prerequisites

- QEMU (`qemu-system-x86_64` and optionally `qemu-img`)
- PowerShell:
  - Windows PowerShell 5.1 or PowerShell 7+ should work
- A **prepared Windows 7 image** that:
  - has the virtio drivers installed (virtio-blk + virtio-net)
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
  --snapshot
```

Add `--follow-serial` to stream COM1 serial output while waiting.

## How the harness works

- Starts a tiny HTTP server on `127.0.0.1:<HttpPort>`
  - QEMU slirp/user networking exposes host as `10.0.2.2` inside the guest, so the guest can HTTP GET `http://10.0.2.2:<HttpPort>/aero-virtio-selftest`.
- Launches QEMU with:
  - `-chardev file,...` + `-serial chardev:...` (guest COM1 → host log)
  - `virtio-net-pci` with `-netdev user`
  - `-drive if=virtio` for virtio-blk testing
- Watches the serial log for:
  - `AERO_VIRTIO_SELFTEST|RESULT|PASS`
  - `AERO_VIRTIO_SELFTEST|RESULT|FAIL`

## Provisioning an image (recommended approach)

Windows images are **not** distributed in this repo.

The recommended flow:
1. Install Windows 7 normally in QEMU once (using your own ISO + key).
2. Install the Aero virtio drivers (virtio-blk + virtio-net).
3. Copy `aero-virtio-selftest.exe` into the guest.
4. Create a scheduled task to run the selftest at boot (SYSTEM).

The guest-side README includes an example `schtasks /Create ...` command.

If you want to fully automate provisioning, see `New-AeroWin7TestImage.ps1` (template generator / scaffold).

`New-AeroWin7TestImage.ps1` also supports baking `--blk-root` into the installed scheduled task (useful if the VM boots
from a non-virtio disk but has a separate virtio data volume):

```powershell
pwsh ./drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 `
  -SelftestExePath ./aero-virtio-selftest.exe `
  -DriversDir ./drivers-out `
  -BlkRoot "D:\aero-virtio-selftest\"
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

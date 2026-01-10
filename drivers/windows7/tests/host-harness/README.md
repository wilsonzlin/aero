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

## Running tests

Example:

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -SerialLogPath ./win7-serial.log `
  -TimeoutSeconds 600
```

On success, the script returns exit code `0` and prints:

```
PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS
```

On failure/timeout, it returns non-zero and prints the matching failure reason.

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

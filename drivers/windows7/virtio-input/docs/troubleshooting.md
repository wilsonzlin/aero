# Troubleshooting (Win7): virtio-input driver bring-up common failure modes

This page is a quick checklist for the most common “driver won’t install / won’t start / no input” issues when bringing up the **Aero virtio-input** Windows 7 SP1 KMDF HID minidriver.

**Fast sanity checks (guest VM):**

```cmd
:: Test-signing (required for local test certs on Win7 x64)
bcdedit /set testsigning on

:: Service state (created by the INF when installed)
sc query aero_virtio_input

:: Driver binary expected location once loaded
dir %SystemRoot%\System32\drivers\aero_virtio_input.sys

:: (Optional) confirm the package is staged in the DriverStore
dir %SystemRoot%\System32\DriverStore\FileRepository\aero_virtio_input.inf_*
```

If you are using QEMU, keep PS/2 input enabled during install so you do not lose control, then disable it *after* virtio-input works (see “No input events” below).

## Device Manager: Code 28 (“The drivers for this device are not installed”)

**Meaning:** Windows didn’t find a matching INF (or you never installed it).

**Checks:**

1. Confirm the device’s **Hardware Ids** match the INF.
   - Device Manager → device → **Properties** → **Details** → **Hardware Ids**
2. Confirm you are pointing Device Manager at a directory that contains **all** of:
   - `aero_virtio_input.inf`
   - `aero_virtio_input.sys`
   - `aero_virtio_input.cat` (if you are installing a signed package)
3. Confirm architecture matches:
   - Win7 x86 guest → use the x86 driver build.
   - Win7 x64 guest → use the x64 driver build.

**Notes:**

- The in-tree INF is intentionally **revision gated** and **modern-only**. If your device enumerates as `REV_00` or `DEV_1011` (transitional), Windows may never match this INF.
  - See “Code 10 / contract mismatch” for what the driver expects.

## Device Manager: Code 52 (“Windows cannot verify the digital signature”)

**Meaning:** The driver package signature was rejected.

### 1) Test signing mode isn’t enabled (common)

From an elevated Command Prompt:

```cmd
bcdedit /set testsigning on
shutdown /r /t 0
```

After reboot, Windows should display a “Test Mode” watermark on the desktop.

### 2) Certificate not trusted (test cert not installed into the right stores)

For local test signing, install the `.cer` into **Local Machine** stores:

- **Trusted Root Certification Authorities** (Local Computer)
- **Trusted Publishers** (Local Computer)

You can view these via:

```cmd
certlm.msc
```

> The in-tree helper script `scripts/install-test-cert.ps1` installs to these two LocalMachine stores.

### 3) SHA-1 vs SHA-2 (Win7 update dependency)

Windows 7 SP1 can reject SHA-2-signed driver packages unless SHA-2 support updates are installed.

- If your test certificate or signatures are **SHA-256 / SHA-2**, ensure the Win7 guest has SHA-2 support updates installed (commonly referenced as **KB3033929** and/or **KB4474419**).
- If you’re using the repo’s default test certificate workflow, it is typically **SHA-1** for maximum compatibility with stock Win7 SP1.

If you switched cert/signing settings recently, re-generate the catalog (`.cat`) and re-sign the package so the INF+CAT+SYS match.

## Device Manager: Code 10 (“This device cannot start”) / contract mismatch

**Meaning:** The driver was selected/installed, but refused to start the device (often `STATUS_NOT_SUPPORTED`) because the virtio device does not satisfy the **Aero Win7 virtio-input contract v1**.

The most common mismatches are:

### PCI identity expectations

- **Revision ID must be `REV_01`**
  - The driver checks PCI revision `0x01` and refuses to run on `REV_00`.
  - In QEMU, pass `x-pci-revision=0x01` for virtio-input PCI devices.
- **Device ID must be `DEV_1052`** (virtio-input modern/non-transitional)
  - Transitional virtio-input (`DEV_1011`) will not match the in-tree INF by default.

### Virtio PCI (BAR0) layout / modern transport expectations

The driver expects a **modern virtio-pci** device exposing the standard capability-based register layout (common config, notify, ISR, device config). If your device model uses a different BAR layout (or legacy-only I/O ports), the driver may fail very early during transport bring-up and show Code 10.

### virtio-input config selectors that must match

The driver uses virtio-input config space to validate and classify the device:

- `ID_NAME` must be exactly one of:
  - `Aero Virtio Keyboard`
  - `Aero Virtio Mouse`
- `EV_BITS` must be implemented and must advertise the minimum required event types/codes.
  - If `EV_BITS` is missing or empty, the driver will refuse to start.

If you are iterating on a device model, fixing `ID_NAME` and implementing `EV_BITS` is usually the fastest path to getting past Code 10.

## `hidtest` can’t open the device

Some HID stacks/devices do not allow opening the device interface with `GENERIC_WRITE`.

Try the following in the Windows 7 guest:

1. First, verify the device shows up:
   ```bat
   hidtest.exe --list
   ```
2. If an LED write fails, try a different output method:
   ```bat
   hidtest.exe --keyboard --led-hidd 0x02
   hidtest.exe --keyboard --led-ioctl-set-output 0x02
   ```
3. If you only want to read input reports, avoid write access entirely:
   ```bat
   hidtest.exe --keyboard
   hidtest.exe --mouse
   ```

Also confirm you are opening the intended collection (keyboard vs mouse) if multiple HID devices are present.

## No input events (likely still using PS/2)

In virtualized setups it’s easy to accidentally keep using the emulated **PS/2** keyboard/mouse (i8042) even after installing the virtio-input driver.

After you have a known-good virtio-input driver installed, disable i8042 in QEMU to force virtio-only input:

```bash
qemu-system-x86_64 \
  -machine pc,accel=kvm,i8042=off \
  ...
```

If disabling i8042 makes input stop working entirely, the virtio-input stack is still not producing usable events (re-check Code 10 conditions, and verify the device appears under the expected Device Manager categories).

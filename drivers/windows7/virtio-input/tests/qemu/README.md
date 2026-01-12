# QEMU manual test plan: Windows 7 virtio-input (HID) driver

This document describes a repeatable way to manually validate the virtio-input **HID**
driver end-to-end on:

- Windows 7 SP1 x86
- Windows 7 SP1 x64

It uses QEMU to provide virtio-input keyboard/mouse devices, then verifies:

1. The virtio-input HID minidriver binds to the virtio PCI function(s)
2. Windows `hidclass.sys` enumerates HID collections correctly
3. Windows built-in `kbdhid.sys` and `mouhid.sys` attach to the resulting HID keyboard/mouse collections
4. Keyboard/mouse input reports are correct (validated with `hidtest`)

Hardware ID (HWID) references are documented in:

- `drivers/windows7/virtio-input/docs/pci-hwids.md`

## Prerequisites

- QEMU new enough to provide `virtio-keyboard-pci` and `virtio-mouse-pci` devices.
  - For Aero contract v1 driver testing (Revision ID enforcement), QEMU must support `x-pci-revision=0x01`.
- A Windows 7 SP1 VM disk image (x86 or x64).
- A built virtio-input driver package for the target architecture (INF + SYS + CAT).
  - CI output layout: `out/packages/windows7/virtio-input/<arch>/`
  - `<arch>` is `x86` or `x64` (CI naming).
- Test-signing enabled in the guest (or a properly-signed driver package).
- `hidtest.exe` built from:
  - `drivers/windows7/virtio-input/tools/hidtest/`

## QEMU command lines

The examples below are intentionally explicit and can be used as a starting point. Adjust paths, CPU accel, and disk/network options as needed.

Note: QEMU’s `virtio-keyboard-pci` and `virtio-mouse-pci` are separate device frontends. If you
want to mirror the **Aero contract v1** topology (single **multi-function** PCI device with
keyboard on function 0 and mouse on function 1), you can place them on the same slot with explicit
function numbers (`addr=...`) and enable multi-function enumeration on function 0
(`multifunction=on`).

### Windows 7 SP1 x86

Keep the default PS/2 devices enabled during initial driver installation so you do not lose input.

```bash
qemu-system-i386 \
  -machine pc,accel=kvm \
  -m 2048 \
  -cpu qemu32 \
  -drive file=win7-x86.qcow2,if=ide,format=qcow2 \
  -device virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01 \
  -device virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01 \
  -net nic,model=e1000 -net user
```

Optional (place both on slot 0x0A, functions 0 and 1):

```bash
-device virtio-keyboard-pci,addr=0x0a,multifunction=on,disable-legacy=on,x-pci-revision=0x01 \
-device virtio-mouse-pci,addr=0x0a.1,disable-legacy=on,x-pci-revision=0x01
```

### Windows 7 SP1 x64

```bash
qemu-system-x86_64 \
  -machine pc,accel=kvm \
  -m 4096 \
  -cpu qemu64 \
  -drive file=win7-x64.qcow2,if=ide,format=qcow2 \
  -device virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01 \
  -device virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01 \
  -net nic,model=e1000 -net user
```

Optional (place both on slot 0x0A, functions 0 and 1):

```bash
-device virtio-keyboard-pci,addr=0x0a,multifunction=on,disable-legacy=on,x-pci-revision=0x01 \
-device virtio-mouse-pci,addr=0x0a.1,disable-legacy=on,x-pci-revision=0x01
```

### Modern-only vs transitional virtio-input

Virtio-input has two PCI IDs defined in the virtio spec:

- **Modern / non-transitional**: `PCI\VEN_1AF4&DEV_1052`
- **Transitional (legacy+modern)**: `PCI\VEN_1AF4&DEV_1011`

QEMU’s virtio-input PCI devices currently enumerate as **modern/non-transitional**
(`DEV_1052`) even without `disable-legacy=on`. However, you can still include
`disable-legacy=on` to make your intent explicit:

```bash
-device virtio-keyboard-pci,disable-legacy=on \
-device virtio-mouse-pci,disable-legacy=on
```

### Aero contract v1: PCI Revision ID (`REV_01`)

The Aero Windows 7 virtio device contract encodes the **contract major version** in the PCI
Revision ID (contract v1 = `0x01`).

Some QEMU virtio devices report `REV_00` by default. If you are testing drivers that enforce
the Aero contract Revision ID, pass `x-pci-revision=0x01`:

```bash
-device virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01 \
-device virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01
```

### Optional: validate without PS/2 input (post-install)

After the driver is installed and confirmed working, you can ensure you are not accidentally testing the emulated PS/2 devices by disabling the i8042 controller:

```bash
qemu-system-x86_64 \
  -machine pc,accel=kvm,i8042=off \
  ... \
  -device virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01 \
  -device virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01
```

Only do this after you have a known-good virtio-input driver; otherwise you may lose keyboard/mouse access in the guest.

## Verifying HWID

Before installing the driver (or when troubleshooting binding), confirm the device HWID that Windows sees:

1. In the Windows 7 VM, open **Device Manager**.
2. Find the virtio-input device (often under **Other devices** as an unknown “PCI Device” before the driver is installed).
3. Right-click → **Properties** → **Details** tab.
4. In the **Property** dropdown, select **Hardware Ids**.

Expected values include at least:

- `PCI\VEN_1AF4&DEV_1052` (base VEN/DEV form)

The list will also include more specific forms, e.g.:

- `PCI\VEN_1AF4&DEV_1052&REV_01` (when using `x-pci-revision=0x01`)
- `PCI\VEN_1AF4&DEV_1052&SUBSYS_...&REV_01` (depending on the device model)

The Aero Win7 virtio-input INF is intentionally **revision-gated**, so it matches the
`...&REV_01` hardware IDs and does not bind to `REV_00` devices.

## Cross-checking with QEMU monitor (no guest required)

You can validate the PCI ID that QEMU is emitting without booting Windows:

```bash
printf 'info pci\nquit\n' | \
  qemu-system-x86_64 -nodefaults -machine q35 -m 128 -nographic -monitor stdio \
    -device virtio-keyboard-pci
```

Expected `info pci` line (device ID may be shown in lowercase):

```
Keyboard: PCI device 1af4:1052
```

## Driver installation (Windows 7 guest)

1. Boot Windows 7 normally (with PS/2 input still enabled).
2. Enable test signing (Admin CMD):
   ```bat
   bcdedit /set testsigning on
   ```
   Reboot the VM. You should see "Test Mode" on the desktop.
3. Install the virtio-input driver using the **built package directory** for the matching architecture:
   - Open **Device Manager**
    - Find the new/unknown device(s) created by the virtio-input PCI functions
      - Often shows under **Other devices** as an unknown PCI device until the INF is installed.
    - Right click → **Update Driver Software...**
    - **Browse my computer for driver software**
    - Point it to a directory containing `aero_virtio_input.inf` + `aero_virtio_input.sys` (for example: `out\packages\windows7\virtio-input\x64\`)
4. Reboot when prompted.

## Verify the Windows HID stacks attach (`kbdhid.sys` / `mouhid.sys`)

After reboot:

1. Open **Device Manager**.
2. Verify a HID keyboard device exists:
   - Category: **Keyboards**
   - Typical name: **HID Keyboard Device**
   - Properties → **Driver** → **Driver Details**
   - Expected to see at least:
     - `kbdhid.sys`
     - `hidclass.sys`
     - `hidparse.sys`
3. Verify a HID mouse device exists:
   - Category: **Mice and other pointing devices**
   - Typical name: **HID-compliant mouse**
   - Driver Details should include:
     - `mouhid.sys`
     - `hidclass.sys`
     - `hidparse.sys`
4. Confirm input works in the guest desktop (typing, mouse movement/clicks).

## Run `hidtest`

Copy `hidtest.exe` into the guest and run it from an elevated Command Prompt.

1. List devices:
   ```bat
   hidtest.exe --list
   ```
   Look for entries with:
   - Usage `0x0001/0x0006` (GenericDesktop/Keyboard)
   - Usage `0x0001/0x0002` (GenericDesktop/Mouse)

2. Read keyboard reports (prefers a virtio keyboard device when present):
   ```bat
   hidtest.exe --keyboard
   ```
   While it is running, press **F1..F12**. Each function key should appear in the keyboard
   report's key array as HID usage `0x3A..0x45` (F1=`0x3A`, F12=`0x45`).

3. Read mouse reports (prefers a virtio mouse device when present):
   ```bat
   hidtest.exe --mouse
   ```

4. (Optional) send a keyboard LED output report:
   ```bat
   hidtest.exe --keyboard --led 0x02
   ```
   This validates that the device exposes an output report and accepts writes. (In a VM there may not be a physical LED to observe.)

5. (Optional) send a keyboard LED output report via `HidD_SetOutputReport` (exercises `IOCTL_HID_SET_OUTPUT_REPORT`):
   ```bat
   hidtest.exe --keyboard --led-hidd 0x02
   ```

6. (Optional) negative tests (invalid user pointers; should fail cleanly without crashing the guest):
   ```bat
   hidtest.exe --keyboard --ioctl-bad-xfer-packet
   hidtest.exe --keyboard --ioctl-bad-write-report
   hidtest.exe --keyboard --hidd-bad-set-output-report
   ```

## Troubleshooting

### Device Manager shows an error code

- **Code 28**: driver not installed
  - Re-run **Update Driver...** and ensure you pointed to a directory containing the correct `aero_virtio_input.inf` + `aero_virtio_input.sys` for that guest architecture.
- **Code 52**: Windows cannot verify the digital signature
  - Ensure `bcdedit /set testsigning on` was applied and the VM rebooted.
  - Ensure you installed the correct x86 vs x64 build of the driver.
- **Code 10**: device cannot start
  - Confirm the guest is binding the expected hardware ID (see “Verifying HWID”).
  - Confirm the QEMU device type matches what the driver expects (modern/non-transitional vs transitional).
  - This driver is built against the **Aero Win7 virtio contract v1** and uses the virtio-input `ID_NAME`
    config string to classify the device as keyboard vs mouse. If the device model does not report the
    expected names (`"Aero Virtio Keyboard"` / `"Aero Virtio Mouse"`), the driver will refuse to start.

### `hidtest` cannot open the device

- Some HID devices cannot be opened with `GENERIC_WRITE`; try running without `--led`.
- If `--led` fails, try `--led-hidd` (it uses a different HID output path).
- If `hidtest` fails to read:
  - Confirm the device is present and enabled in Device Manager.
  - Try selecting the other collection (keyboard vs mouse).

### Input works in Windows but `hidtest` prints nothing

- Verify you are not testing PS/2 input unintentionally; after the driver works, re-run QEMU with `-machine ... ,i8042=off` to force virtio-only input.

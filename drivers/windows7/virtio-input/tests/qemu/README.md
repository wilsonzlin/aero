# QEMU manual test plan: Windows 7 virtio-input (HID) driver

For the consolidated end-to-end virtio-input validation plan (Rust device model + web runtime + Win7 driver), see:

- [`docs/virtio-input-test-plan.md`](../../../../../docs/virtio-input-test-plan.md)

This document describes a repeatable way to manually validate the virtio-input **HID**
driver end-to-end on:

- Windows 7 SP1 x86
- Windows 7 SP1 x64

It uses QEMU to provide virtio-input keyboard/mouse devices, then verifies:

1. The virtio-input HID minidriver binds to the virtio PCI function(s)
2. Windows `hidclass.sys` enumerates HID collections correctly
3. Windows built-in `kbdhid.sys` and `mouhid.sys` attach to the resulting HID keyboard/mouse collections
4. Keyboard/mouse input reports are correct (validated with `hidtest`)

> Note: The in-tree Win7 virtio-input driver is **strict by default** (Aero contract v1):
>
> - Keyboard/mouse expect the Aero virtio-input `ID_NAME` strings (`"Aero Virtio Keyboard"` / `"Aero Virtio Mouse"`).
> - Tablet/absolute-pointer devices can additionally be accepted (best-effort) via `EV_BITS` inference
>   (`EV_ABS` + `ABS_X`/`ABS_Y`) even if `ID_NAME` is not an Aero string, as long as the PCI subsystem device ID does **not**
>   indicate an Aero contract kind (`0x0010`/`0x0011`/`0x0012`).
>
> If your QEMU virtio-input devices report QEMU `ID_NAME` strings (as stock QEMU often does:
> `"QEMU Virtio Keyboard"` / `"QEMU Virtio Mouse"` / `"QEMU Virtio Tablet"`), enable **compat mode** in the guest
> to allow the driver to bind to QEMU keyboard/mouse devices:
>
> ```bat
> reg add HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters ^
>   /v CompatIdName /t REG_DWORD /d 1 /f
> ```
>
> Then reboot (or disable/enable the device). In compat mode the driver accepts QEMU `ID_NAME`
> strings, relaxes strict `ID_DEVIDS` validation, and can infer device kind from `EV_BITS`.

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

This directory contains helper scripts that wrap the recommended QEMU arguments and always include the
virtio-input **Aero contract v1** flags (`disable-legacy=on,x-pci-revision=0x01`):

- [`run-win7-x86.sh`](./run-win7-x86.sh)
- [`run-win7-x64.sh`](./run-win7-x64.sh)

They print the exact QEMU command line and then `exec` QEMU.

### Quick start (recommended)

```bash
# x86 guest
./run-win7-x86.sh /path/to/win7-x86.qcow2

# x64 guest
./run-win7-x64.sh /path/to/win7-x64.qcow2
```

Options:

- `--multifunction` — place keyboard + mouse on the same PCI slot (`00:0a.0` + `00:0a.1`) to mirror the
  Aero contract topology.
- `--i8042-off` — disable the emulated PS/2 controller (`-machine ...,i8042=off`). Only use this after the
  virtio-input driver is installed and confirmed working, otherwise you may lose input.
- `--vectors N` (alias: `--msix-vectors N`) — request an MSI-X table size from QEMU
  (`-device virtio-*-pci,...,vectors=N`). Requires QEMU support for the `vectors` property (QEMU will fail to
  start if unsupported). Windows may still grant fewer messages; drivers fall back.

Passing extra QEMU args:

The scripts accept additional QEMU arguments after the disk image path. For clarity you can insert an
optional `--` separator:

```bash
./run-win7-x64.sh /path/to/win7-x64.qcow2 -- -smp 2 -display gtk
```

Environment overrides:

- `QEMU_BIN=...` — override the QEMU binary (defaults: `qemu-system-i386` / `qemu-system-x86_64`).
- `QEMU_ACCEL=kvm|tcg|...` — override `-machine ...,accel=...` (defaults to `kvm` when available).
- `QEMU_DISK_FORMAT=qcow2|vpc|raw|...` — override disk format detection (helpful if the file extension is
  ambiguous).

### Equivalent explicit command lines

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

Equivalent helper-script flag:

```bash
./run-win7-x86.sh --multifunction /path/to/win7-x86.qcow2
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

Equivalent helper-script flag:

```bash
./run-win7-x64.sh --multifunction /path/to/win7-x64.qcow2
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

Equivalent helper-script flag:

```bash
./run-win7-x64.sh --i8042-off /path/to/win7-x64.qcow2
```

Equivalent for x86:

```bash
./run-win7-x86.sh --i8042-off /path/to/win7-x86.qcow2
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

- `PCI\VEN_1AF4&DEV_1052&SUBSYS_...&REV_01` (Aero contract v1 subsystem IDs)

The in-tree Aero Win7 virtio-input INFs are intentionally **revision-gated** (Aero contract v1, `REV_01`).

- Keyboard/mouse: `aero_virtio_input.inf`
  - Contract keyboard HWID: `...&SUBSYS_00101AF4&REV_01` (Device Manager name: **Aero VirtIO Keyboard**).
  - Contract mouse HWID: `...&SUBSYS_00111AF4&REV_01` (Device Manager name: **Aero VirtIO Mouse**).
  - Strict generic fallback (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01` (**Aero VirtIO Input Device**).
- Tablet/absolute pointer: `aero_virtio_tablet.inf`
  - Contract tablet HWID: `...&SUBSYS_00121AF4&REV_01` (Aero contract tablet).
  - This HWID is more specific than the generic fallback, so it wins when it matches (i.e. when both packages are installed).
  - If the tablet subsystem ID is missing (or `aero_virtio_tablet.inf` is not installed), the device may bind via the generic fallback
    in `aero_virtio_input.inf` and appear as **Aero VirtIO Input Device**.
- Optional legacy filename alias (disabled by default): `virtio-input.inf.disabled` → rename to `virtio-input.inf` to enable
  - Intended for compatibility with workflows/tools that still reference `virtio-input.inf`.
  - Filename-only alias: from the first section header (`[Version]`) onward, it is expected to be byte-identical to
    `aero_virtio_input.inf`
    (banner/comments may differ; see `drivers/windows7/virtio-input/scripts/check-inf-alias.py`).
  - Enabling the alias does **not** change HWID matching behavior (it matches the same HWIDs as `aero_virtio_input.inf`).
  - Do **not** ship/install both basenames at once: choose **either** `aero_virtio_input.inf` **or** `virtio-input.inf`.

If your device is `REV_01` but does not expose the Aero subsystem IDs, `aero_virtio_input.inf` can still bind via the strict
generic fallback model line (`PCI\VEN_1AF4&DEV_1052&REV_01`); in that case it will show up as **Aero VirtIO Input Device**.
If you expect distinct keyboard/mouse names, ensure the subsystem IDs are present (`SUBSYS_0010` / `SUBSYS_0011`).

If the device reports `REV_00`, Windows will not bind (the INFs are revision-gated). Ensure `x-pci-revision=0x01` is set.

If you want tablet devices to bind with the tablet Device Manager name, ensure `aero_virtio_tablet.inf` is installed as well
(it is more specific than the generic fallback match, so it wins when both packages are present and it matches).

Avoid shipping/installing both `aero_virtio_input.inf` and `virtio-input.inf` at the same time: the alias overlaps the canonical
HWID set (including the strict generic fallback), which can lead to duplicate DriverStore entries and confusing PnP driver selection.

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

Or run the helper script (exits non-zero if the expected `1af4:1052` ID is not found):

```bash
bash ./drivers/windows7/virtio-input/tests/qemu/info-pci.sh
```

To use a non-default QEMU binary:

```bash
QEMU_BIN=/path/to/qemu-system-x86_64 bash ./drivers/windows7/virtio-input/tests/qemu/info-pci.sh
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
     - Point it to a directory containing `aero_virtio_input.inf` + `aero_virtio_input.sys` (and `aero_virtio_tablet.inf` for tablet/absolute-pointer devices) (for example: `out\packages\windows7\virtio-input\x64\`)
4. Reboot when prompted.

## Verify the Windows HID stacks attach (`kbdhid.sys` / `mouhid.sys`)

After reboot:

1. Open **Device Manager**.
2. Under **Human Interface Devices**, the virtio-input PCI functions should appear with distinct names when the Aero subsystem IDs are present:
   - **Aero VirtIO Keyboard**
   - **Aero VirtIO Mouse**
3. Verify a HID keyboard device exists:
   - Category: **Keyboards**
   - Typical name: **HID Keyboard Device**
   - Properties → **Driver** → **Driver Details**
   - Expected to see at least:
     - `kbdhid.sys`
     - `hidclass.sys`
     - `hidparse.sys`
4. Verify a HID mouse device exists:
   - Category: **Mice and other pointing devices**
   - Typical name: **HID-compliant mouse**
   - Driver Details should include:
     - `mouhid.sys`
     - `hidclass.sys`
     - `hidparse.sys`
5. Confirm input works in the guest desktop (typing, mouse movement/clicks).

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
   # 0x1F sets all 5 HID boot keyboard LED bits (Num/Caps/Scroll/Compose/Kana).
   hidtest.exe --keyboard --led 0x1F
   ```
    This validates that the device exposes an output report and accepts writes. (In a VM there may not be a physical LED to observe.)
    For the Aero contract v1 requirement that the device consumes/completes all virtio-input `statusq` buffers, also check the driver
    counters (see below): `LedWritesSubmitted`, `StatusQSubmits`, and `StatusQCompletions` should advance and `StatusQCompletions` should
    catch up to `StatusQSubmits` (outstanding count should not grow without bound).

   Or, cycle LEDs through a short sequence to exercise the write path (Num/Caps/Scroll/Compose/Kana, then `0x1F`):
   ```bat
   hidtest.exe --keyboard --led-cycle
   ```

5. (Optional) send a keyboard LED output report via `HidD_SetOutputReport` (exercises `IOCTL_HID_SET_OUTPUT_REPORT`):
   ```bat
   hidtest.exe --keyboard --led-hidd 0x1F
   ```

6. (Optional) send a keyboard LED output report via `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)`:
   ```bat
   hidtest.exe --keyboard --led-ioctl-set-output 0x1F
   ```

   To stress the LED/statusq write path (backpressure/coalescing), run:
   ```bat
   hidtest.exe --keyboard --led-spam 10000
   ```
   By default, `--led-spam` alternates `0` and `0x1F` (all 5 defined HID boot keyboard LED bits).
   Override the "on" value by combining with `--led 0xMASK` (or `--led-hidd` / `--led-ioctl-set-output`).

7. (Optional) query/reset driver diagnostics counters:
   ```bat
   hidtest.exe --counters
   hidtest.exe --counters --json
   hidtest.exe --reset-counters
   hidtest.exe --reset-counters --json

   REM Reset and immediately verify that monotonic counters are cleared:
   hidtest.exe --reset-counters --counters
   hidtest.exe --reset-counters --counters --json
   hidtest.exe --reset-counters --counters-json
   ```
     Note: `--reset-counters` requires opening the HID interface with write access; if it fails, rerun elevated.
     For how to interpret the counters output (normal increments vs drops/overruns), see:
     - [`tools/hidtest/README.md` → Counters interpretation](../../tools/hidtest/README.md#counters-interpretation)

      Quick interpretation (after you generate some input / LED writes):

      - Normal: `VirtioEvents` and `IoctlHidReadReport` increase; `ReadReportPended` and `ReadReportCompleted` increase and stay close; depth gauges like `PendingRingDepth` stay low.
      - Bad: `PendingRingDrops` / `ReportRingDrops` / `VirtioEventDrops` increasing indicates dropped input; `ReportRingOverruns` / `VirtioEventOverruns` should remain `0` (non-zero indicates oversized events/reports).
      - LED/statusq (contract): after running `--keyboard --led ...`, `LedWritesSubmitted` / `StatusQSubmits` should increase and `StatusQCompletions` should eventually match `StatusQSubmits` (no wedge).

8. (Optional) query driver state / interrupt mode diagnostics:
   ```bat
   hidtest.exe --keyboard --state
   hidtest.exe --keyboard --state --json
   hidtest.exe --keyboard --state-json
   hidtest.exe --keyboard --interrupt-info
   hidtest.exe --keyboard --interrupt-info --json
   hidtest.exe --keyboard --interrupt-info-json

   REM Probe short-buffer negotiation (expect ERROR_INSUFFICIENT_BUFFER):
   hidtest.exe --ioctl-query-interrupt-info-short
   ```

9. (Optional) negative tests (invalid user pointers; should fail cleanly without crashing the guest):
   ```bat
   hidtest.exe --keyboard --ioctl-bad-xfer-packet
   hidtest.exe --keyboard --ioctl-bad-write-report
   hidtest.exe --keyboard --ioctl-bad-read-xfer-packet
   hidtest.exe --keyboard --ioctl-bad-read-report
   hidtest.exe --keyboard --ioctl-bad-set-output-xfer-packet
   hidtest.exe --keyboard --ioctl-bad-set-output-report
   hidtest.exe --keyboard --ioctl-bad-get-report-descriptor
   hidtest.exe --keyboard --ioctl-bad-get-collection-descriptor
   hidtest.exe --keyboard --ioctl-bad-get-device-descriptor
   hidtest.exe --keyboard --ioctl-bad-get-string
   hidtest.exe --keyboard --ioctl-bad-get-indexed-string
   hidtest.exe --keyboard --ioctl-bad-get-string-out
   hidtest.exe --keyboard --ioctl-bad-get-indexed-string-out
   hidtest.exe --keyboard --hidd-bad-set-output-report
   ```

## Regression test: "stuck keys" on power transition / hot-unplug

The driver emits an all-zero input report when transitioning away from a running device
(D0Exit, surprise removal, HID deactivate) so Windows releases any latched key state.

### D0Exit (sleep / resume)

1. Open Notepad.
2. Hold a key down (e.g. `A`) so autorepeat is visible.
3. Trigger a sleep/resume transition:
   - In the guest: **Start → Shut down → Sleep**, then resume the VM.
4. Verify the key is **not** still logically pressed after resume (no continued autorepeat).

### SurpriseRemoval (QEMU hot-unplug)

To exercise `EvtDeviceSurpriseRemoval`, start QEMU with a monitor and explicit device IDs:

```bash
qemu-system-x86_64 \
  ... \
  -monitor stdio \
  -device virtio-keyboard-pci,id=vkbd,disable-legacy=on,x-pci-revision=0x01 \
  -device virtio-mouse-pci,id=vmouse,disable-legacy=on,x-pci-revision=0x01
```

Then:

1. In the guest, hold a key down (autorepeat in Notepad is an easy visual).
2. In the QEMU monitor, remove the device:
   - `device_del vkbd`
3. Verify the key does **not** remain logically pressed after removal.

## Troubleshooting

For the common Win7 bring-up failure modes (Device Manager error codes, signature/test-signing issues, contract mismatches, `hidtest` access problems, and PS/2 vs virtio input confusion), see:

- [`drivers/windows7/virtio-input/docs/troubleshooting.md`](../../docs/troubleshooting.md)

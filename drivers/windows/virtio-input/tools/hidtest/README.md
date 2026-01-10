# hidtest (Win7)

Minimal user-mode HID probe utility for validating the `virtio-input` HID minidriver without full emulator integration.

## What it does

- Enumerates present HID device interfaces (SetupDi APIs, `GUID_DEVINTERFACE_HID`).
- Opens a handle to a selected interface.
- Calls:
  - `HidD_GetAttributes` (VID/PID)
  - `HidD_GetPreparsedData` + `HidP_GetCaps` (report sizes / usage)
  - `IOCTL_HID_GET_REPORT_DESCRIPTOR` (descriptor length sanity check)
- Reads input reports via `ReadFile` in a loop and prints raw bytes + best-effort decoding for:
  - virtio-input keyboard report (`ReportID=1`)
  - virtio-input mouse report (`ReportID=2`)
- Optionally writes a keyboard LED output report (`ReportID=1`) via `WriteFile` to exercise `IOCTL_HID_WRITE_REPORT`.

## Build (MSVC)

From a Visual Studio / Windows SDK command prompt:

```bat
cl /nologo /W4 /D_CRT_SECURE_NO_WARNINGS main.c /link setupapi.lib hid.lib
```

## Build (MinGW-w64)

```sh
gcc -municode -Wall -Wextra -O2 -o hidtest.exe main.c -lsetupapi -lhid
```

## Usage

List all HID interfaces:

```bat
hidtest.exe --list
```

Open the virtio keyboard collection by default and read reports:

```bat
hidtest.exe
```

Force selecting the mouse collection:

```bat
hidtest.exe --mouse
```

If multiple mice are present, `--mouse` prefers a virtio interface (VID `0x1AF4`) when available.

Write keyboard LEDs (NumLock|CapsLock|ScrollLock):

```bat
hidtest.exe --led 0x07
```

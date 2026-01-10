# hidtest (Windows 7)

Small user-mode HID verification tool for validating that the virtio-input HID minidriver produces correct keyboard and mouse reports.

This program does **not** depend on any virtio driver sources; it talks to HID devices via the Windows HID APIs.

## Build

`hidtest` is a Win32 console application (C++) and only uses Windows SDK headers/libraries:

- SetupAPI (`setupapi.lib`)
- HID (`hid.lib`)

From a Visual Studio **Developer Command Prompt** (x86 or x64):

```bat
cd drivers\windows7\virtio-input\tools\hidtest
cl /nologo /W4 /EHsc /DUNICODE /D_UNICODE hidtest.cpp /link setupapi.lib hid.lib
```

This produces `hidtest.exe` in the current directory.

## Usage

### List HID devices

```bat
hidtest.exe list
```

This enumerates present HID device interfaces via `GUID_DEVINTERFACE_HID` and prints:

- Device path
- VID/PID (if available)
- Usage page / usage
- Input/Output/Feature report lengths
- Report descriptor length

### Listen for input reports (keyboard/mouse)

```bat
hidtest.exe listen <index>
```

Press keys / move the mouse and the tool will print decoded events.

For a keyboard collection, events include:

- Modifier transitions (`LCTRL`, `LSHIFT`, ...)
- Key pressed/released transitions (HID usage IDs; common keys are named)

For a mouse collection, events include:

- Button transitions
- `buttons` bitmask + `x/y` relative motion + `wheel` (if present)

Stop with `Ctrl+C`.

### Send a keyboard LED output report (Num/Caps/Scroll)

```bat
hidtest.exe setleds <index> <mask>
```

`mask` bits (standard boot keyboard LED layout):

- `0x01` = NumLock
- `0x02` = CapsLock
- `0x04` = ScrollLock

Example:

```bat
hidtest.exe setleds 3 0x02
```

If the selected HID device does not expose an output report, or cannot be opened with `GENERIC_WRITE`, the tool prints an error.


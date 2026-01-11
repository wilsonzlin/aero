# WebUSB constraints for USB passthrough (Chromium)

This document describes what **WebUSB can and cannot do** in Chromium-based browsers, and how those constraints shape Aero’s planned **“non-HID USB passthrough”** feature.

The important takeaway is that WebUSB is **not a general-purpose “attach any USB device to the VM”** mechanism. It is usable primarily for **vendor-specific bulk/interrupt devices** that can be driven via **WinUSB/libusb** without the host OS binding a native class driver.

In practice, WebUSB failures are dominated by two constraints:

1. **Browser restrictions**: some USB interface classes are treated as “protected” and cannot be accessed via WebUSB.
2. **Host OS driver / permissions**: even when `navigator.usb` exists, `open()` / `claimInterface()` can fail if the OS driver binding or permissions are incorrect (especially on Windows).

---

## Troubleshooting (common reasons WebUSB fails)

If WebUSB calls like `requestDevice()`, `device.open()`, or `device.claimInterface()` fail with an opaque `DOMException`:

- **Secure context required:** WebUSB requires `https://` or `http://localhost` (`isSecureContext === true`).
- **User gesture required:** `navigator.usb.requestDevice()` must be triggered by a user gesture (e.g. a button click).
  - Call `requestDevice()` directly from the gesture handler; if you `await` before calling it, the user gesture can be lost.
  - User activation does **not** propagate across `postMessage()` to workers, so do the chooser step on the main thread.
- **Protected interface classes:** WebUSB cannot access some interface classes (HID, mass storage, audio/video, etc.). Prefer a vendor-specific interface (class `0xFF`) or a more appropriate Web API (e.g. WebHID/WebSerial).
- **Windows (WinUSB):** WebUSB typically requires the relevant interface to be bound to **WinUSB**.
  - For development: tools like **Zadig** can install WinUSB for a specific VID/PID/interface.
  - For production devices: ship **Microsoft OS 2.0 descriptors** / WinUSB Compatible ID descriptors so Windows binds WinUSB automatically.
- **Linux (udev / kernel driver):**
  - Ensure your user has permission to access the device (via `udev` rules).
  - Ensure no kernel driver is attached to the interface; a bound kernel driver can prevent `claimInterface()`.

---

## 1) Chromium “protected interface classes”

Chromium maintains a list of **protected USB interface classes**. Interfaces in these classes are treated as **security-sensitive** and cannot be requested/claimed by WebUSB.

### Protected classes (Aero-relevant subset)

| `bInterfaceClass` (hex) | USB-IF class name | Practical impact for WebUSB |
|---:|---|---|
| `0x01` | Audio | Blocked (no USB audio passthrough/streaming via WebUSB) |
| `0x03` | HID (Human Interface Device) | Blocked (keyboards, mice, many game controllers) |
| `0x08` | Mass Storage | Blocked (flash drives, external HDDs/SSDs) |
| `0x09` | Hub | Blocked |
| `0x0B` | Smart Card | Blocked |
| `0x0E` | Video | Blocked (USB webcams/capture devices) |
| `0x10` | Audio/Video | Blocked |
| `0xE0` | Wireless Controller | Blocked (e.g. Bluetooth HCI adapters) |

> Note: Chromium’s protected list is maintained in Chromium source and may evolve. The table above captures the classes that matter most for Aero’s “USB passthrough” planning.

### What “protected” means in practice

- **Devices with only protected interfaces won’t appear** in the `navigator.usb.requestDevice()` chooser at all.
- **Composite devices can still be requestable** *if they contain at least one non-protected interface*.
  - Example: a device exposing `HID (0x03)` + `Vendor Specific (0xFF)` may appear in the chooser because of the vendor-specific interface.
  - However, **protected interfaces remain unclaimable**: attempts to `claimInterface()` on a protected interface will fail, so only the non-protected portion of the device is usable via WebUSB.

This matters for Aero because a “passthrough” implementation can only forward traffic for interfaces that WebUSB can actually claim.

---

## 2) Transfer-type limitations (no practical USB audio/video streaming)

WebUSB exposes these transfer types:

- **Control transfers** (`controlTransferIn/Out`)
- **Bulk transfers** (`transferIn/Out` on bulk endpoints)
- **Interrupt transfers** (`transferIn/Out` on interrupt endpoints)

**Isochronous transfers are not generally available/stable in WebUSB** across browsers/platforms, which makes true passthrough of:

- USB Audio (typically isochronous)
- USB Video / UVC cameras (typically isochronous)

impractical via WebUSB.

If Aero ever targets isochronous support, it should be treated as **experimental** and gated behind explicit “this may not work” UX (Chromium flags/origin trials may be involved depending on the state of the platform).

---

## 3) Host OS driver friction (the #1 reason “it works on my machine” fails)

Even when an interface class is not protected, WebUSB still needs the host OS to allow the browser process to open and claim that interface.

### Windows: WinUSB is required per-interface

On Windows, WebUSB can only reliably talk to interfaces bound to **WinUSB** (or another libusb-compatible driver stack).

**Symptoms when WinUSB is not installed for the interface:**

- Device shows in the chooser, but `device.open()` or `claimInterface()` fails.
- Errors tend to surface as `NetworkError`/`NotFoundError`/`SecurityError` depending on the failure point.

**Strategy options:**

1. **Best (device/firmware controlled): ship WinUSB binding via Microsoft OS 2.0 descriptors**
   - Many vendor devices can advertise **MS OS 2.0 descriptors** (WCID) so Windows automatically associates the interface with WinUSB.
   - This is the lowest-friction path for end users and is the only approach that scales for production.
2. **Fallback (user installs a driver): use Zadig to bind WinUSB**
   - Users can use **Zadig** to replace the driver for a specific interface with WinUSB.
   - This typically requires admin rights and can break vendor software that expects the original driver.
   - For composite devices, users must select the correct interface (not the whole device) where Zadig exposes per-interface entries.

**Aero guidance:** for any “USB passthrough” UX on Windows, assume that **WinUSB installation is a prerequisite** and design the onboarding/troubleshooting flow accordingly.

### Linux: udev permissions + kernel driver detachment

On Linux, WebUSB access usually fails for one of two reasons:

1. **Permissions:** the browser process cannot open `/dev/bus/usb/...`
   - Fix via a udev rule granting access to the relevant devices (vendor/product IDs).
   - Common patterns include `TAG+="uaccess"` (logind-managed access) or assigning to a group like `plugdev`.
2. **Kernel driver bound to the interface**
   - If a kernel driver already claimed the interface, the browser may fail to claim it.
   - Vendor-specific interfaces (`0xFF`) are most likely to work because they often have no in-kernel driver.

**Example udev rule (template):**

```udev
# /etc/udev/rules.d/99-aero-webusb.rules
SUBSYSTEM=="usb", ATTR{idVendor}=="1234", ATTR{idProduct}=="5678", TAG+="uaccess"
```

After adding a rule: `sudo udevadm control --reload-rules && sudo udevadm trigger` (or replug the device).

### macOS: generally OK for vendor-specific, but system drivers still win

macOS does not have a WinUSB-style driver install step, but the same core rule applies:

- If the OS has a system driver bound to the interface, WebUSB cannot “steal” it reliably.
- Vendor-specific interfaces are the most feasible.

---

## 4) Feasibility matrix for Aero use-cases

The table below translates WebUSB constraints into Aero product guidance.

| Aero use-case | Typical interface class | WebUSB feasibility | Notes / alternatives |
|---|---:|---|---|
| **Vendor-specific “bulk device passthrough”** (custom hardware, firmware tools, dongles) | `0xFF` (Vendor Specific) | **Works in principle** | Needs bulk/interrupt endpoints. On Windows requires WinUSB. This is the primary viable target for Aero’s “non-HID USB passthrough”. |
| Serial adapters / microcontrollers presenting as COM ports | `0x02/0x0A` (CDC ACM) | **Usually not via WebUSB** | Often bound to OS serial drivers. Prefer **WebSerial** as a separate integration path. |
| Keyboards / mice / most game controllers | `0x03` (HID) | **Not possible via WebUSB alone** | Chromium protects HID interfaces. Use **Pointer Lock + keyboard events**, **Gamepad API**, or (for some devices) **WebHID** as separate paths. |
| USB flash drives / external storage | `0x08` (Mass Storage) | **Not possible via WebUSB alone** | Protected class + OS driver binding. Use **File System Access API** / upload flows instead of block-device passthrough. |
| USB audio interfaces / headsets | `0x01` (Audio) | **Not possible via WebUSB alone** | Protected class + isochronous. Use **Web Audio** / OS audio routing. |
| Webcams / capture devices | `0x0E` (Video) | **Not possible via WebUSB alone** | Protected class + isochronous. Use **`getUserMedia()`** / MediaDevices. |
| Smart card readers | `0x0B` (Smart Card) | **Not possible via WebUSB alone** | Protected class. Consider domain-specific flows (e.g. WebAuthn) rather than VM passthrough. |

---

## 5) “User steps” checklist (what Aero’s UI/UX must assume)

For any WebUSB-backed feature:

- Page must be in a **secure context**: `https://` or `http://localhost`
- **User gesture required**: `navigator.usb.requestDevice(...)` must run in a click/tap handler
- **Chromium-based browser required** (Chrome / Edge). Firefox and Safari do not provide WebUSB.
- Device must expose at least one **non-protected interface**; otherwise it will not show up in the chooser.
- **Windows:** the interface must be bound to **WinUSB** (MS OS 2.0 descriptors preferred; Zadig as a manual fallback).
- **Linux:** expect udev permissions work (and possible kernel driver detachment failures).
- **Isochronous (if ever targeted):** expect to require experimental flags and treat as non-production.

---

## 6) Implications for Aero “non-HID USB passthrough”

When we say “USB passthrough” in Aero, what is realistically achievable in the browser is:

- **Forwarding USB control/bulk/interrupt transfers** for a **vendor-specific interface** that WebUSB can claim.

We should avoid promising:

- HID passthrough (keyboards/mice/controllers)
- Mass storage passthrough (flash drives)
- USB audio/video streaming devices
- Smart card readers

Those need separate, purpose-built integrations (virtio input, file import/export, `getUserMedia`, etc.) rather than raw USB forwarding.

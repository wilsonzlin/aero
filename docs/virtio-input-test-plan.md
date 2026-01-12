# Virtio-input end-to-end test plan (device model + Win7 driver + web runtime)

This is the single “do these steps” plan for validating **virtio-input** (keyboard + mouse) end-to-end:

1. Rust/device-model conformance (host-side)
2. Windows 7 driver unit tests (host-side, portable)
3. Windows 7 driver bring-up under QEMU (manual)
4. Windows 7 automated host harness (QEMU + guest selftest)
5. Web runtime validation (browser → input routing → virtio-input)

Authoritative interoperability contract (device model ↔ Win7 drivers):

- **[`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md)** (`AERO-W7-VIRTIO` v1)

Overview (device model behavior and motivation):

- [`docs/virtio-input.md`](./virtio-input.md)

If a test fails, treat the contract as the source of truth; fix code or bump the contract version.

> **Windows images are not distributed.** This repo does not include proprietary Windows 7 images/ISOs.
> For QEMU/host-harness testing you must supply your own Win7 media or a locally prepared image.
> See [`docs/FIXTURES.md`](./FIXTURES.md) and [`docs/13-legal-considerations.md`](./13-legal-considerations.md).

---

## 1) Rust / device-model tests (host-side)

### 1.1 Run the virtio-input-focused tests (recommended)

From the repo root:

```bash
cargo test -p aero-virtio --locked --test virtio_input
```

Recommended (enforces repo resource limits via `safe-run.sh`):

```bash
./scripts/safe-run.sh cargo test -p aero-virtio --locked --test virtio_input
```

If your checkout does not preserve executable bits for `scripts/*.sh` (or you see `Permission denied`), run:

```bash
bash ./scripts/safe-run.sh cargo test -p aero-virtio --locked --test virtio_input
```

Alternative (name-filter based; useful when you don’t remember the test binary name; may match 0 tests):

```bash
# Required by this test plan (or equivalent).
# NOTE: `cargo test -- <pattern>` is a name filter. Always confirm the output says
# `running N tests` with N > 0; if you see `running 0 tests`, use `--test virtio_input`.
./scripts/safe-run.sh cargo test -p aero-virtio --locked -- tests::virtio_input

# Practical equivalent in this repo (matches the `virtio_input` test binary name):
./scripts/safe-run.sh cargo test -p aero-virtio --locked -- virtio_input

# Without safe-run.sh (no timeout / mem limit):
cargo test -p aero-virtio --locked -- tests::virtio_input
cargo test -p aero-virtio --locked -- virtio_input
```

Tip: if you see `running 0 tests`, prefer the explicit integration test invocation:

```bash
./scripts/safe-run.sh cargo test -p aero-virtio --locked --test virtio_input
```

Primary coverage lives in:

- [`crates/aero-virtio/tests/virtio_input.rs`](../crates/aero-virtio/tests/virtio_input.rs)

### 1.2 Run contract-level virtio PCI checks that virtio-input depends on

These are not “virtio-input only”, but they lock down the **shared** virtio-pci contract that the Win7 driver stack depends on.

```bash
cargo test -p aero-virtio --locked --test win7_contract_queue_sizes
cargo test -p aero-virtio --locked --test win7_contract_ring_features
cargo test -p aero-virtio --locked --test win7_contract_dma_64bit
cargo test -p aero-virtio --locked --test pci_profile
cargo test -p aero-devices --locked --test pci_virtio_input_multifunction
```

Recommended (enforces repo resource limits via `safe-run.sh`):

```bash
./scripts/safe-run.sh cargo test -p aero-virtio --locked --test win7_contract_queue_sizes
./scripts/safe-run.sh cargo test -p aero-virtio --locked --test win7_contract_ring_features
./scripts/safe-run.sh cargo test -p aero-virtio --locked --test win7_contract_dma_64bit
./scripts/safe-run.sh cargo test -p aero-virtio --locked --test pci_profile
./scripts/safe-run.sh cargo test -p aero-devices --locked --test pci_virtio_input_multifunction
```

### 1.3 What to expect (key invariants)

These expectations should match **exactly** what is specified in:
[`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md).

**PCI / virtio-pci modern layout**

- **BAR0 size** is **`0x4000`** bytes and is 64-bit MMIO (contract §1.2).
- PCI config space exposes a valid capability list (status bit 4 + cap ptr at `0x34`), containing the required virtio vendor caps:
  - `COMMON_CFG`, `NOTIFY_CFG`, `ISR_CFG`, `DEVICE_CFG` (contract §1.3).
- Aero contract v1 uses a fixed BAR0 capability layout (contract §1.4):
  - `COMMON_CFG` @ `0x0000`
  - `NOTIFY_CFG` @ `0x1000` (`notify_off_multiplier == 4`)
  - `ISR_CFG` @ `0x2000`
  - `DEVICE_CFG` @ `0x3000`

**virtio-input specifics**

- Device is exposed as a **single multi-function PCI device** (keyboard fn0 + mouse fn1) (contract §3.3):
  - Vendor/Device: `1AF4:1052` (`VIRTIO_ID_INPUT`)
  - Revision ID: `0x01` (`REV_01`)
  - Keyboard:
    - function **0**
    - subsystem id `0x0010`
    - `header_type = 0x80` (multifunction bit set)
  - Mouse:
    - function **1**
    - subsystem id `0x0011`
- Each function exposes exactly **2 split virtqueues** (contract §3.3.2):
  - `eventq` (queue 0): **64**
  - `statusq` (queue 1): **64**
- `eventq` buffers complete with **`used.len = 8`** and contain exactly one `virtio_input_event` (contract §3.3.5).
- `statusq` buffers are always consumed/completed (contents may be ignored) (contract §3.3.5, statusq behavior).
- Device config selector behavior matches virtio-input spec + Aero requirements (contract §3.3.4):
  - `ID_NAME` returns:
    - `"Aero Virtio Keyboard"` / `"Aero Virtio Mouse"`
  - `ID_DEVIDS` returns BUS_VIRTUAL + virtio vendor + product id
  - `EV_BITS` bitmaps include required types/codes (keyboard vs mouse differ).
    - Keyboard `EV_BITS` includes: `EV_SYN`, `EV_KEY`, `EV_LED` and `LED_NUML`/`LED_CAPSL`/`LED_SCROLLL`.
    - Mouse `EV_BITS` includes: `EV_SYN`, `EV_KEY`, `EV_REL`.

---

## 2) Windows 7 driver unit tests (portable host-side)

The Win7 virtio-input driver contains a portable translator (`virtio_input_event` → HID reports) that can be tested on any host without the WDK.

Source and test:

- Translator: [`drivers/windows7/virtio-input/src/hid_translate.c`](../drivers/windows7/virtio-input/src/hid_translate.c)
- Test: [`drivers/windows7/virtio-input/tests/hid_translate_test.c`](../drivers/windows7/virtio-input/tests/hid_translate_test.c)

See also: [`drivers/windows7/virtio-input/tests/README.md`](../drivers/windows7/virtio-input/tests/README.md)

### 2.1 Build + run (gcc / clang)

From the repo root:

```bash
cd drivers/windows7/virtio-input/tests

# gcc
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/hid_translate_test \
  hid_translate_test.c ../src/hid_translate.c && /tmp/hid_translate_test

# clang (equivalent)
clang -std=c11 -Wall -Wextra -Werror \
  -o /tmp/hid_translate_test \
  hid_translate_test.c ../src/hid_translate.c && /tmp/hid_translate_test
```

Expected output:

```text
hid_translate_test: ok
```

### 2.2 Explicit mapping check: F1..F12, NumLock, ScrollLock

The translator **must** map Linux `KEY_F1..KEY_F12` (virtio-input EV_KEY codes) to the correct HID keyboard usages (`0x3A..0x45`).

It must also map the lock keys used by Windows LED state:

- `KEY_NUMLOCK` → HID usage `0x53`
- `KEY_SCROLLLOCK` → HID usage `0x47`

This is required by the contract (virtio-input keyboard “minimum required supported key codes” includes **F1..F12**; contract §3.3.5).

The host-side unit test asserts these mappings. If this fails, fix the mapping in:

- `drivers/windows7/virtio-input/src/hid_translate.c`

---

## 3) Windows 7 driver QEMU manual test (shortest path)

Full reference:

- [`drivers/windows7/virtio-input/tests/qemu/README.md`](../drivers/windows7/virtio-input/tests/qemu/README.md)

### 3.0 Build/package/sign the driver (host)

QEMU bring-up requires an installable driver package directory containing:

- `aero_virtio_input.inf`
- `aero_virtio_input.sys`
- `aero_virtio_input.cat` (recommended; required for signature verification)

See the canonical driver README for the full build + signing workflow and CI output paths:

- [`drivers/windows7/virtio-input/README.md`](../drivers/windows7/virtio-input/README.md)

If you built via CI scripts, the packaged outputs are typically staged under:

- `out/packages/windows7/virtio-input/x86/`
- `out/packages/windows7/virtio-input/x64/`

### 3.1 Boot QEMU with virtio-input devices

Example (x64), keeping PS/2 enabled during installation so you don’t lose input:

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

Notes:

- `x-pci-revision=0x01` is required for the **Aero Win7 contract v1** (`REV_01`) drivers to bind.

### 3.2 Install the test certificate + driver in the Win7 guest

Inside the guest:

1. Enable test signing (Admin CMD), then reboot:
   ```bat
   bcdedit /set testsigning on
   ```
2. Install the driver signing certificate used by your build into:
   - **Trusted Root Certification Authorities**
   - **Trusted Publishers**

   See `drivers/windows7/virtio-input/README.md` for the in-tree test-signing workflow (make-cert → install-test-cert → make-cat → sign-driver).
3. Install the driver via **Device Manager**:
   - Find the virtio-input PCI device(s) (often show as unknown before binding)
   - Update Driver → Have Disk… → point at the directory containing:
      - `aero_virtio_input.inf`
      - `aero_virtio_input.sys`
      - `aero_virtio_input.cat`

### 3.3 Verify HID keyboard + mouse enumeration

In Device Manager after install/reboot:

- **Keyboards** contains a **HID Keyboard Device** (driver stack includes `kbdhid.sys`, `hidclass.sys`).
- **Mice and other pointing devices** contains a **HID-compliant mouse** (driver stack includes `mouhid.sys`, `hidclass.sys`).

(Optional but recommended) Run `hidtest.exe` as described in the QEMU README to validate raw input reports.

- Tool source/build instructions: [`drivers/windows7/virtio-input/tools/hidtest/README.md`](../drivers/windows7/virtio-input/tools/hidtest/README.md)

After the driver is installed and confirmed working, you can optionally disable PS/2 in QEMU (`-machine ...,i8042=off`) to ensure you are not accidentally testing the emulated PS/2 devices. Only do this once you have a known-good virtio-input driver; otherwise you may lose input in the guest.

### 3.4 Expected pass/fail signals (Windows 7)

These are common “first look” signals when validating the driver end-to-end:

- **Before driver install (expected):**
  - Device Manager shows the virtio-input PCI function(s) under **Other devices** (often as “PCI Device”).
  - Typical state: **Code 28** (“drivers for this device are not installed”).
- **After successful install (expected):**
  - The virtio-input PCI function(s) bind to `aero_virtio_input.sys` and appear under **Human Interface Devices** (`HIDClass`).
  - A HID keyboard and HID mouse are present and use the in-box HID stacks:
    - Keyboard: **Keyboards → HID Keyboard Device** (`kbdhid.sys`, `hidclass.sys`, `hidparse.sys`)
    - Mouse: **Mice and other pointing devices → HID-compliant mouse** (`mouhid.sys`, `hidclass.sys`, `hidparse.sys`)
- **Common failures:**
  - **Code 52**: Windows cannot verify the driver signature (test signing/cert install issue).
  - **Code 10**: device cannot start (often contract mismatch: wrong `REV`, wrong virtio-pci caps/layout, or wrong virtio-input `ID_NAME` strings).

For detailed troubleshooting (including QEMU-specific notes), see:

- [`drivers/windows7/virtio-input/tests/qemu/README.md`](../drivers/windows7/virtio-input/tests/qemu/README.md)

---

## 4) Windows 7 automated host harness (QEMU + guest selftest)

Full reference:

- [`drivers/windows7/tests/host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md)

### 4.1 Basic invocation (PowerShell)

```powershell
pwsh ./drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1 `
  -QemuSystem qemu-system-x86_64 `
  -DiskImagePath ./win7-aero-tests.qcow2 `
  -SerialLogPath ./win7-serial.log `
  -Snapshot `
  -TimeoutSeconds 600
```

### 4.2 Basic invocation (Python, Linux-friendly)

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --serial-log ./win7-serial.log \
  --timeout-seconds 600 \
  --snapshot
```

Success looks like:

- Harness exit code `0`
- Serial log contains `AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS`

---

## 5) Web runtime validation (browser → virtio-input routing)

Goal: validate that browser keyboard/mouse events are routed through the correct virtual device:

- **before** the Win7 virtio-input driver sets `DRIVER_OK`: PS/2 or USB fallback
- **after** `DRIVER_OK`: virtio-input

### 5.1 Bring up the web runtime

From the repo root:

```bash
cargo xtask web dev
```

Equivalent:

```bash
npm run dev
```

Open the printed URL with a verbose log level (example):

```text
http://localhost:5173/?log=debug
```

Note: most multi-worker test/debug flows (SharedArrayBuffer, ring buffers, etc.) require `crossOriginIsolated` to be true (COOP/COEP headers).

### 5.2 Validate virtio-input PCI exposure (IDs / caps / BAR0) in the browser runtime

Once virtio-input is wired into the web runtime PCI bus, validate the device is exposed exactly as required by:

- `docs/windows7-virtio-driver-contract.md` (AERO-W7-VIRTIO v1), especially:
  - §1.3 PCI caps
  - §1.4 fixed BAR0 layout
  - §3.3 virtio-input

#### What to check (contract v1)

- Two PCI functions with **Vendor/Device** `1AF4:1052`:
  - function 0: keyboard (`SUBSYS 1AF4:0010`, `header_type = 0x80` multifunction)
  - function 1: mouse (`SUBSYS 1AF4:0011`)
- **Revision ID** `0x01` (`REV_01`) on both functions.
- **BAR0**: 64-bit MMIO, size `0x4000` bytes (via standard PCI BAR sizing probe).
- **Capability list** is present (PCI Status bit 4 set; cap pointer at `0x34`) and contains the required virtio vendor-specific capabilities:
  - `cfg_type = 1` COMMON
  - `cfg_type = 2` NOTIFY (must include `notify_off_multiplier = 4`)
  - `cfg_type = 3` ISR
  - `cfg_type = 4` DEVICE

#### How to check (practical approach)

Use the same “CPU ↔ IO worker” technique used by the existing PCI tests to read config space via PCI config mechanism #1 (ports `0xCF8`/`0xCFC`):

- [`tests/e2e/io_worker_i8042.spec.ts`](../tests/e2e/io_worker_i8042.spec.ts) (see “PCI config + BAR-backed MMIO dispatch”)

At a high level:

1. From a CPU-context (or a small debug worker), write `0xCF8` to select a B/D/F + config register dword:

   ```text
   0x8000_0000 | (bus<<16) | (device<<11) | (function<<8) | (reg & 0xFC)
   ```

2. Read `0xCFC..0xCFF` to get the config dword/word/byte.
3. Scan bus 0 for `vendor_id == 0x1AF4` and `device_id == 0x1052`.
4. For BAR0 sizing:
   - write `0xFFFF_FFFF` to BAR0 low dword (and high dword for 64-bit BARs), then read back the mask and compute size.
   - expected size: `0x4000`.
5. Walk the PCI capability list (starting at config offset `0x34`) and confirm virtio vendor caps and their BAR/offset layout.
6. If you need to read actual virtio MMIO registers via BAR0 (e.g. to check `device_status` / `DRIVER_OK`), ensure PCI **memory space decoding** is enabled (PCI command bit1 = `0x2`).
   - The end-to-end PCI test in [`tests/e2e/io_worker_i8042.spec.ts`](../tests/e2e/io_worker_i8042.spec.ts) includes an example of enabling mem decoding before issuing BAR-backed MMIO reads/writes.

Expected signal:

- **Pass:** the virtio-input keyboard and mouse functions enumerate and match the contract values above.
- **Fail:** any drift here usually means the Win7 driver won’t bind (INF is revision-gated) or won’t start (cap parsing/layout checks fail).

### 5.3 Boot a Win7 image with the virtio-input driver installed

1. Boot a Windows 7 image that already has the Aero virtio-input driver installed and working (see §3).
2. In the Windows guest, confirm the virtio-input devices enumerate (Device Manager):
   - HID keyboard and HID mouse present (or at least the virtio-input PCI functions are present and the driver service is started).

### 5.4 Verify routing switch happens at `DRIVER_OK`

The intended auto-routing policy is implemented by the input capture pipeline:

- [`crates/emulator/src/in_capture.rs`](../crates/emulator/src/in_capture.rs) (`InputRoutingPolicy::Auto`)
- It prefers virtio-input only when `virtio.keyboard.driver_ok()` / `virtio.mouse.driver_ok()` becomes true.

Validation steps:

1. While the guest is still booting (or before the virtio-input driver is installed), verify keyboard/mouse still work via the fallback path (PS/2 or USB).
2. After the driver is installed and the guest is fully booted, verify:
   - the driver reaches `DRIVER_OK` (virtio status bit 2)
   - keyboard/mouse input continues working
3. Confirm that new input is being sent via virtio-input (not the fallback path).

If you need an explicit “device-ready” signal, `DRIVER_OK` is the virtio status bit `0x04` in the virtio common config `device_status` register (AERO-W7-VIRTIO v1 uses the common config at BAR0 + `0x0000`, and `device_status` at offset `0x14` within that common config block).

### 5.5 Recommended debug signal (when validating routing)

When debugging routing issues, add a one-line log/trace when the virtio-input driver becomes ready:

- log when `VirtioInputDevice::driver_ok()` transitions from false → true
  - see [`crates/emulator/src/io/virtio/devices/input.rs`](../crates/emulator/src/io/virtio/devices/input.rs) (`driver_ok()`)

And/or log which backend is selected in `InputRoutingPolicy::Auto`:

- see the selection points in `crates/emulator/src/in_capture.rs` (keyboard/mouse Auto branches).

This makes it trivial to confirm “we switched to virtio-input only after DRIVER_OK”.

---

## Success criteria (summary)

You should be able to validate virtio-input without reading implementation details:

- Rust tests pass (virtio-input + contract-level virtio-pci invariants).
- Win7 driver translator unit test passes, including explicit **F1..F12** mapping.
- Win7 driver binds and enumerates HID keyboard + mouse under QEMU.
- Win7 host harness reports `virtio-input|PASS`.
- Web runtime routes input to virtio-input only after `DRIVER_OK`, with a clear debug signal when it flips.

# PCI Device Compatibility (Windows 7 Driver Binding)

Windows (and Linux) bind drivers primarily based on PCI **Vendor ID / Device ID** and the **Class Code (base / sub / prog-if)**, and in some cases also require a sane PCI **BAR layout** and/or specific capabilities. If these fields are inconsistent, guests can fail to attach in-box drivers and instead show “Unknown device”, breaking boot or core functionality.

This document defines Aero’s canonical PCI identity + wiring for I/O devices, with a focus on stable Windows 7 binding.

The canonical **paravirtual** device identities are encoded in `crates/devices/src/pci/profile.rs`, and validated by `crates/devices/tests/pci_profile.rs`.

Note: the canonical `aero_machine::Machine` supports **two mutually-exclusive** display configurations:

- `MachineConfig::enable_aerogpu=true`: expose the **AeroGPU PCI identity** at `00:07.0`
  (`A3A0:0001`) with the canonical BAR layout (BAR0 regs + BAR1 VRAM aperture). This is the
  canonical Windows driver binding target (`PCI\VEN_A3A0&DEV_0001`). In `aero_machine` today:

  - BAR1 is backed by a dedicated VRAM buffer for legacy VGA/VBE boot display compatibility and
    implements permissive legacy VGA decode (VGA port I/O + VRAM-backed `0xA0000..0xBFFFF` window;
    see `docs/16-aerogpu-vga-vesa-compat.md`).
  - Note: the in-tree Win7 AeroGPU driver treats the adapter as system-memory-backed (no dedicated
    WDDM VRAM segment). BAR1 is outside the WDDM memory model (see
    `docs/graphics/win7-wddm11-aerogpu-driver.md`).
  - BAR0 implements a minimal MMIO surface sufficient for the in-tree Win7 KMD to initialize:
    - ring/fence transport (submission decode/capture + fence-page/IRQ plumbing; default bring-up
      behavior can complete fences without executing the command stream; browser/WASM runtimes can
      enable an out-of-process “submission bridge” via `Machine::aerogpu_drain_submissions` +
      `Machine::aerogpu_complete_fence`; native builds can install an in-process backend such as the
      feature-gated headless wgpu backend via `Machine::aerogpu_set_backend_wgpu`), and
    - scanout0 register storage + vblank counters/IRQ semantics for `WaitForVerticalBlankEvent`
      pacing (see `drivers/aerogpu/protocol/vblank.md`).
- `MachineConfig::enable_vga=true`: expose the standalone legacy VGA/VBE implementation
  (`aero_gpu_vga`) for BIOS/bootloader VGA/VBE compatibility.
  - When `enable_pc_platform=false`, the machine maps the VBE LFB MMIO aperture directly at the
    configured LFB base (historically defaulting to `0xE000_0000` / `aero_gpu_vga::SVGA_LFB_BASE`).
  - When `enable_pc_platform=true`, the machine exposes a minimal Bochs/QEMU-compatible “Standard
    VGA” PCI stub (`aero_devices::pci::profile::VGA_TRANSITIONAL_STUB`, `1234:1111` at `00:0c.0`)
    and routes the VBE LFB through its BAR0 inside the PCI MMIO window (BAR base assigned by BIOS
    POST / the PCI allocator).
  - This path is not part of the long-term Windows paravirtual device contract.

## Canonical PCI layout (bus/dev/fn)

We assume a single PCI bus (`bus 0`) with stable device numbers. Not all devices must be present in every VM configuration, but when enabled they should retain their canonical BDF for predictable guest enumeration.

| BDF      | Device | Vendor:Device | Class (base/sub/progif) | INTx pin | Notes |
|----------|--------|---------------|--------------------------|----------|-------|
| 00:01.0  | ISA    | 8086:7000     | 06/01/00                 | -        | PIIX3-compatible ISA bridge (function 0 of a multi-function slot; `header_type=0x80` so guests discover 00:01.1/00:01.2) |
| 00:01.1  | IDE    | 8086:7010     | 01/01/8A                 | INTA     | PIIX3-compatible PCI IDE (legacy compatibility mode, bus mastering DMA). Note: ATA/ATAPI completion interrupts are legacy ISA IRQ14/IRQ15 (see below + `docs/05-storage-topology-win7.md`). |
| 00:01.2  | USB1   | 8086:7020     | 0C/03/00                 | INTA     | UHCI (USB 1.1) |
| 00:02.0  | SATA   | 8086:2922     | 01/06/01                 | INTA     | AHCI (SATA) |
| 00:03.0  | NVMe   | 1B36:0010     | 01/08/02                 | INTA     | NVMe controller (optional) |
| 00:04.0  | Audio  | 8086:2668     | 04/03/00                 | INTA     | Intel HD Audio (HDA) controller |
| 00:05.0  | NIC    | 8086:100E     | 02/00/00                 | INTA     | Intel E1000 (82540EM) |
| 00:06.0  | NIC    | 10EC:8139     | 02/00/00                 | INTA     | RTL8139 (alternate NIC option) |
| 00:07.0  | GPU    | A3A0:0001     | 03/00/00                 | INTA     | AeroGPU display controller (WDDM). **Canonical BDF + VID/DID contract** for Windows driver binding (`PCI\VEN_A3A0&DEV_0001`). Do not assign any other device to `00:07.0`. See `docs/abi/aerogpu-pci-identity.md`. Canonical PCI profile defines BAR0 (64KiB regs) + BAR1 (prefetchable VRAM aperture) per `docs/16-aerogpu-vga-vesa-compat.md`. |
| 00:08.0  | vNIC   | 1AF4:1041     | 02/00/00                 | INTA     | virtio-net (Aero Win7 contract v1: modern-only, `REV_01`; upstream transitional = 1AF4:1000) |
| 00:09.0  | vBlk   | 1AF4:1042     | 01/00/00                 | INTA     | virtio-blk (Aero Win7 contract v1: modern-only, `REV_01`; upstream transitional = 1AF4:1001) |
| 00:0A.0  | vInput | 1AF4:1052     | 09/80/00                 | INTA     | virtio-input keyboard (Aero Win7 contract v1: `SUBSYS_00101AF4`, `REV_01`, `header_type=0x80` for multi-function discovery) |
| 00:0A.1  | vInput | 1AF4:1052     | 09/80/00                 | INTA     | virtio-input mouse (Aero Win7 contract v1: `SUBSYS_00111AF4`, `REV_01`) |
| 00:0B.0  | vSnd   | 1AF4:1059     | 04/01/00                 | INTA     | virtio-snd (Aero Win7 contract v1: modern-only, `REV_01`) |
| 00:0c.0  | VGA (stub) | 1234:1111 | 03/00/00 | - | Bochs/QEMU “Standard VGA” PCI stub identity (see `aero_devices::pci::profile::VGA_TRANSITIONAL_STUB`). Exposed only for the standalone legacy VGA/VBE boot-display path when `enable_vga=true` (and `enable_aerogpu=false`) with the PC platform enabled (routes the VBE LFB through PCI BAR0). Must be absent when `enable_aerogpu=true`. |
| 00:0d.0  | USB3   | 1B36:000D     | 0C/03/30                 | INTA     | xHCI (USB 3.x) controller (QEMU xHCI identity). Wired in the web runtime when the WASM build exports `XhciControllerBridge` (optional/experimental; Windows 7 has no in-box xHCI driver). See [`docs/usb-xhci.md`](./usb-xhci.md). |
| 00:12.0  | USB2   | 8086:293A     | 0C/03/20                 | INTA     | EHCI (USB 2.0) controller (ICH9-family identity; Windows 7 in-box `usbehci.sys`). See [`docs/usb-ehci.md`](./usb-ehci.md). |

### Notes on display (AeroGPU vs VGA/VBE boot display)

- The canonical AeroGPU Windows 7 driver binds to `PCI\VEN_A3A0&DEV_0001`. See
  [`abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md).
- With `MachineConfig::enable_aerogpu=true`, the machine exposes the AeroGPU PCI identity at
  `00:07.0` (`A3A0:0001`) for driver binding. In the intended AeroGPU-owned VGA/VBE boot display
  path (see [`16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md)), firmware derives
  the VBE mode-info `PhysBasePtr` from AeroGPU BAR1: `PhysBasePtr = BAR1_BASE + 0x40000`
  (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`; see `crates/aero-machine/src/lib.rs::VBE_LFB_OFFSET`).
- With `MachineConfig::enable_vga=true` (and `enable_aerogpu=false`), boot display is provided by
  the standalone `aero_gpu_vga` VGA/VBE device model. Firmware reports the configured VBE LFB base
  address (historically defaulting to `0xE000_0000`).
  - When `enable_pc_platform=true`, this base is the BAR0 base of the “Standard VGA” PCI stub
    (`00:0c.0`), assigned by BIOS POST / the PCI allocator.
  - When `enable_pc_platform=false`, the machine maps the LFB MMIO aperture directly at the
    configured physical address.
- This `enable_vga` VGA/VBE path is a stepping stone and does **not** implement the full AeroGPU
  WDDM MMIO/ring protocol described by
  [`16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md).

### Notes on virtio IDs (transitional vs modern)

Virtio uses vendor ID `0x1AF4`.

Aero’s canonical profile (and the Windows 7 virtio device contract, `AERO-W7-VIRTIO` v1) uses the virtio 1.0+
**modern** virtio-pci device ID space (`0x1040 + device_id`) and a modern-only transport (PCI capabilities + MMIO).

The **transitional** IDs below are listed only for upstream/historical context; Aero contract v1 does not require and
may not expose transitional IDs.

| Virtio function | Transitional ID | Modern ID |
|-----------------|-----------------|-----------|
| net             | 1AF4:1000       | 1AF4:1041 |
| blk             | 1AF4:1001       | 1AF4:1042 |
| input           | 1AF4:1011       | 1AF4:1052 |

Note: virtio-snd is treated as **modern-only** in `AERO-W7-VIRTIO` v1; do not rely on any legacy/transitional ID space for driver binding.

## IRQ routing (INTx → PIRQ → PIC/APIC)

PCI INTx interrupts are level-triggered and are routed by the chipset via “PIRQ” lines (A–D).

Note: some “legacy” devices (notably PIIX3 IDE) use ISA IRQs (IRQ14/IRQ15) for their data-plane
interrupts even though they are exposed as PCI functions and have PCI INTx fields in config space.
Do not assume that every PCI function’s interrupts are delivered via the PIRQ→GSI mapping.

### 1) INTx swizzle (root bus)

For a device on the root bus, the PIRQ line is selected using the standard PCI swizzle:

```
PIRQ = (INTx + device_number) mod 4
```

Where `INTA=0, INTB=1, INTC=2, INTD=3`.

In the canonical layout above, devices use `INTA`, so the effective PIRQ line is `device_number mod 4`.

### 2) PIRQ → PIC IRQ (8259)

For a typical PC-compatible mapping (and the default used by Aero’s PCI INTx router), map PIRQ lines to IRQs 10–13:

| PIRQ | PIC IRQ |
|------|---------|
| A    | 10      |
| B    | 11      |
| C    | 12      |
| D    | 13      |

### 3) PIRQ → IOAPIC GSI

For APIC mode, route the same PIRQ lines to IOAPIC GSIs `10..13`:

| PIRQ | IOAPIC GSI |
|------|------------|
| A    | 10         |
| B    | 11         |
| C    | 12         |
| D    | 13         |

## PCI config requirements (what must stay stable)

For Windows 7 and Linux to bind drivers predictably:

1. **Vendor ID / Device ID** must match the expected device model.
2. **Class/Subclass/ProgIF** must match:
    - IDE: `01/01/*` (PIIX3 uses `prog-if=0x8A` for legacy-compat + bus-master IDE)
    - AHCI: `01/06/01`
    - NVMe: `01/08/02`
    - HDA: `04/03/00`
    - UHCI: `0C/03/00`
    - EHCI: `0C/03/20`
    - xHCI: `0C/03/30`
3. **Header type** must be `0x00` (type-0 endpoint), except when a device intentionally exposes
     multiple functions on the same slot:
    - PIIX3 (function 0 at `00:01.0`) must set `header_type = 0x80` so guests enumerate the IDE
      and UHCI functions at `00:01.1` and `00:01.2`.
   - `virtio-input` keyboard (function 0) must set `header_type = 0x80` (multi-function bit) so
      guests enumerate the paired mouse function (function 1).
4. **BAR types and sizes** must be correct:
    - Example: AHCI’s ABAR must be MMIO and large enough for the implemented port set (Aero uses 8KiB
      via `aero_devices::pci::profile::AHCI_ABAR_SIZE`).
    - Example: HDA MMIO must be 16KiB (`0x4000`) per spec.
    - Example: AeroGPU’s canonical PCI profile defines BAR0 (64KiB regs) and BAR1 (prefetchable VRAM aperture)
      for legacy VGA/VBE compatibility (`docs/16-aerogpu-vga-vesa-compat.md`).
5. **Virtio PCI capabilities** must be present and internally consistent for modern drivers:
    - Virtio vendor-specific capabilities for Common/ISR/Device/Notify regions
    - MSI/MSI-X capabilities where supported by the platform implementation

## Debugging

The unit tests include a “PCI dump” string generator (`aero_devices::pci::profile::pci_dump`) to help spot regressions in IDs/class codes/IRQ mapping when adding new devices.

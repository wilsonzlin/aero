# PCI Device Compatibility (Windows 7 Driver Binding)

Windows (and Linux) bind drivers primarily based on PCI **Vendor ID / Device ID** and the **Class Code (base / sub / prog-if)**, and in some cases also require a sane PCI **BAR layout** and/or specific capabilities. If these fields are inconsistent, guests can fail to attach in-box drivers and instead show “Unknown device”, breaking boot or core functionality.

This document defines Aero’s canonical PCI identity + wiring for I/O devices, with a focus on stable Windows 7 binding.

The canonical values are encoded in `crates/devices/src/pci/profile.rs`, and validated by `crates/devices/tests/pci_profile.rs`.

## Canonical PCI layout (bus/dev/fn)

We assume a single PCI bus (`bus 0`) with stable device numbers. Not all devices must be present in every VM configuration, but when enabled they should retain their canonical BDF for predictable guest enumeration.

| BDF      | Device | Vendor:Device | Class (base/sub/progif) | INTx pin | Notes |
|----------|--------|---------------|--------------------------|----------|-------|
| 00:01.1  | IDE    | 8086:7010     | 01/01/8A                 | INTA     | PIIX3-compatible PCI IDE (legacy compatibility mode, bus mastering DMA) |
| 00:01.2  | USB1   | 8086:7020     | 0C/03/00                 | INTA     | UHCI (USB 1.1) |
| 00:02.0  | SATA   | 8086:2922     | 01/06/01                 | INTA     | AHCI (SATA) |
| 00:03.0  | NVMe   | 1B36:0010     | 01/08/02                 | INTA     | NVMe controller (optional) |
| 00:04.0  | Audio  | 8086:2668     | 04/03/00                 | INTA     | Intel HD Audio (HDA) controller |
| 00:05.0  | NIC    | 8086:100E     | 02/00/00                 | INTA     | Intel E1000 (82540EM) |
| 00:06.0  | NIC    | 10EC:8139     | 02/00/00                 | INTA     | RTL8139 (alternate NIC option) |
| 00:08.0  | vNIC   | 1AF4:1041     | 02/00/00                 | INTA     | virtio-net (modern ID; transitional = 1AF4:1000) |
| 00:09.0  | vBlk   | 1AF4:1042     | 01/00/00                 | INTA     | virtio-blk (modern ID; transitional = 1AF4:1001) |
| 00:0A.0  | vInput | 1AF4:1052     | 09/80/00                 | INTA     | virtio-input (modern ID) |
| 00:0B.0  | vSnd   | 1AF4:1059     | 04/01/00                 | INTA     | virtio-snd (modern ID; generic PCI audio class) |

### Notes on virtio IDs (transitional vs modern)

Virtio uses vendor ID `0x1AF4`.

Aero’s canonical profile uses **modern** virtio PCI IDs (`0x1040 + device_type`), but also documents the **transitional** IDs used by older stacks.

| Virtio function | Transitional ID | Modern ID |
|-----------------|-----------------|-----------|
| net             | 1AF4:1000       | 1AF4:1041 |
| blk             | 1AF4:1001       | 1AF4:1042 |
| input           | (modern-only)   | 1AF4:1052 |
| snd             | (modern-only)   | 1AF4:1059 |

## IRQ routing (INTx → PIRQ → PIC/APIC)

PCI INTx interrupts are level-triggered and are routed by the chipset via “PIRQ” lines (A–D).

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
3. **Header type** must be `0x00` (type-0 endpoint) for these devices.
4. **BAR types and sizes** must be correct:
   - Example: AHCI’s ABAR must be MMIO and large enough for the implemented port set (Aero uses 8KiB).
   - Example: HDA MMIO must be 16KiB (`0x4000`) per spec.
5. **Virtio PCI capabilities** must be present and internally consistent for modern drivers:
   - Virtio vendor-specific capabilities for Common/ISR/Device/Notify regions
   - MSI/MSI-X capabilities where supported by the platform implementation

## Debugging

The unit tests include a “PCI dump” string generator (`aero_devices::pci::profile::pci_dump`) to help spot regressions in IDs/class codes/IRQ mapping when adding new devices.

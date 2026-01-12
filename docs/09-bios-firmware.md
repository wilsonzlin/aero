# 09 - BIOS/UEFI & Firmware

## Overview

Windows 7 can boot from both legacy BIOS and UEFI. We implement a custom BIOS that performs POST, initializes devices, and boots the Windows Boot Manager.

> Implementation note: the current legacy BIOS implementation lives in `crates/firmware::bios`.
> It uses a lightweight "HLT-in-ROM-stub hypercall" dispatch model for INT services (documented in
> `crates/firmware/README.md`).
>
> The BIOS code is written directly against the canonical CPU state (`aero_cpu_core::state::CpuState`)
> and guest physical memory bus (`memory::MemoryBus`), with a small set of firmware-local traits for
> ROM mapping, A20 gating, and sector-based block devices. In the canonical VM stack,
> `crates/aero-machine` provides the concrete implementations of those traits and dispatches BIOS
> interrupt hypercalls.
>
> Storage trait note: BIOS uses a small `firmware::bios::BlockDevice` interface for INT 13h, while
> PCI storage controllers use `aero_storage::VirtualDisk`. `crates/aero-machine::SharedDisk` bridges
> those. See [`docs/20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).
>
> For how Tier-0 surfaces BIOS interrupt stubs (and how the embedding is expected to dispatch them),
> see [`docs/02-cpu-emulation.md`](./02-cpu-emulation.md).

---

## Boot Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Boot Sequence                                 │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  CPU Reset Vector (0xFFFFFFF0)                                  │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  BIOS POST (Power-On Self-Test)                          │    │
│  │    - Memory detection and test                           │    │
│  │    - CPU identification                                  │    │
│  │    - Device enumeration (PCI, USB)                       │    │
│  │    - Initialize interrupt vectors                        │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  ACPI Table Setup                                        │    │
│  │    - RSDP, RSDT/XSDT                                     │    │
│  │    - FADT, MADT, HPET                                    │    │
│  │    - DSDT (AML bytecode)                                 │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Boot Device Selection                                   │    │
│  │    - Read MBR/GPT from boot device                       │    │
│  │    - Load boot sector to 0x7C00                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Windows Boot Manager (bootmgr)                          │    │
│  │    - Protected mode transition                           │    │
│  │    - Load winload.exe                                    │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Windows Kernel (ntoskrnl.exe)                           │    │
│  │    - Long mode transition                                │    │
│  │    - Driver initialization                               │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## SMP Boot (BSP + APs)

For multi-core guests, the emulator must model the x86 SMP bring-up sequence:

- **BSP (CPU0)** starts executing at the reset vector (`0xFFFF_FFF0` → `F000:FFF0`).
- **APs (CPU1..N-1)** do *not* start executing at reset; they begin in a *wait-for-SIPI* state.
- The BSP brings up APs via **local APIC IPIs** (writes to the ICR register):
  - **INIT IPI** (level assert): resets the AP and transitions it to wait-for-SIPI.
  - **SIPI (Startup IPI)**: releases the AP and starts it at `vector << 12` in real mode.

### Trampoline Region

The BIOS (or OS) places a small real-mode trampoline in low memory at a SIPI-addressable page
(below 1MiB, 4KiB aligned). A typical placement is:

- **Physical:** `0x8000`
- **SIPI vector:** `0x08`

The trampoline's job is to:

1. Set up a temporary real-mode environment (segments + stack).
2. Load a GDT and enable protected mode.
3. Enable paging and switch to long mode.
4. Jump to a shared **AP entry point** provided by firmware/OS.
5. Signal "CPU online" to the BSP (e.g. via an atomic flag in shared memory).

## BIOS Implementation

### BIOS Memory Map

```rust
// BIOS ROM layout
pub const BIOS_BASE: u64 = 0x000F_0000;
pub const BIOS_ALIAS_BASE: u64 = 0xFFFF_0000;
pub const BIOS_SIZE: usize = 0x10000;  // 64KB

// Key entry points
pub const RESET_VECTOR_OFFSET: u64 = 0xFFF0; // F000:FFF0
pub const RESET_VECTOR_PHYS: u64 = BIOS_BASE + RESET_VECTOR_OFFSET;
pub const RESET_VECTOR_ALIAS_PHYS: u64 = BIOS_ALIAS_BASE + RESET_VECTOR_OFFSET; // 0xFFFF_FFF0
pub const INT_TABLE: u64 = 0x0000_0000;     // Interrupt vector table
pub const BDA_BASE: u64 = 0x0000_0400;      // BIOS Data Area
pub const EBDA_BASE: u64 = 0x0009_F000;     // Extended BIOS Data Area
pub const EBDA_SIZE: usize = 0x1000;        // 4KB

// PCIe ECAM ("MMCONFIG") window (ACPI MCFG)
pub const PCIE_ECAM_BASE: u64 = 0xB000_0000;
pub const PCIE_ECAM_SIZE: u64 = 0x1000_0000; // 256MiB (buses 0..=255)

#[repr(C)]
pub struct BiosDataArea {
    com_ports: [u16; 4],           // 0x400-0x407
    lpt_ports: [u16; 3],           // 0x408-0x40D
    ebda_segment: u16,             // 0x40E
    equipment_flags: u16,          // 0x410
    reserved1: u8,                 // 0x412
    memory_size_kb: u16,           // 0x413-0x414
    reserved2: [u8; 2],            // 0x415-0x416
    keyboard_flags: [u8; 2],       // 0x417-0x418
    keyboard_buffer: [u8; 32],     // 0x41E-0x43D
    // ... more fields
}
```

### PCIe ECAM (MMCONFIG) + E820 hole handling

To support PCIe-friendly PCI configuration space access (required by many Windows drivers), the
platform maps a 256MiB ECAM window at `0xB000_0000..0xC000_0000` and publishes it via ACPI `MCFG`.

The BIOS reserves the ECAM window **and** the rest of the below-4 GiB PCI/MMIO hole
(`0xC000_0000..0x1_0000_0000`) in its E820 map (type 2) and treats `0xB000_0000`
(`aero_pc_constants::PCIE_ECAM_BASE`) as the end of low RAM. Any configured guest RAM above that
point is remapped above 4 GiB so the guest does not silently lose memory.

### POST Implementation

```rust
pub struct Bios {
    rom: Vec<u8>,
    bda: BiosDataArea,
    acpi_tables: AcpiTables,
    pci_devices: Vec<PciDevice>,
}

impl Bios {
    pub fn post(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        // 1. Disable interrupts
        cpu.rflags &= !FLAG_IF;
        
        // 2. Set up initial segments
        cpu.cs.selector = 0xF000;
        cpu.cs.base = 0x000F_0000;
        cpu.ds.selector = 0x0000;
        cpu.ds.base = 0x0000_0000;
        cpu.ss.selector = 0x0000;
        cpu.ss.base = 0x0000_0000;
        cpu.esp = 0x7C00;
        
        // 3. Memory detection
        let memory_size = self.detect_memory(memory);
        self.bda.memory_size_kb = (memory_size / 1024).min(640) as u16;
        
        // 4. Initialize interrupt vector table
        self.setup_ivt(memory);
        
        // 5. Initialize BIOS Data Area
        self.setup_bda(memory);
        
        // 6. Initialize PCI devices
        self.enumerate_pci(memory);
        
        // 7. Initialize video (VGA)
        self.init_video(memory);
        
        // 8. Initialize keyboard controller
        self.init_keyboard();
        
        // 9. Set up ACPI tables
        self.setup_acpi_tables(memory);
        
        // 10. Enable A20 line
        self.enable_a20(memory);
        
        // 11. Enable interrupts
        cpu.rflags |= FLAG_IF;
        
        // 12. Boot
        self.boot(cpu, memory);
    }
    
    fn setup_ivt(&self, memory: &mut MemoryBus) {
        // Set up interrupt vector table at 0x0000
        for i in 0..256 {
            let vector_addr = (i * 4) as u64;
            let handler_addr = self.get_int_handler(i as u8);
            
            // Each vector is segment:offset (4 bytes)
            memory.write_u16(vector_addr, handler_addr as u16);      // Offset
            memory.write_u16(vector_addr + 2, 0xF000);               // Segment
        }
    }
    
    fn get_int_handler(&self, int_num: u8) -> u16 {
        // Return offset within BIOS segment for interrupt handler
        match int_num {
            0x00 => 0xE000,  // Divide error
            0x08 => 0xE100,  // Timer interrupt (IRQ0)
            0x09 => 0xE200,  // Keyboard interrupt (IRQ1)
            0x10 => 0xE300,  // Video services
            0x13 => 0xE400,  // Disk services
            0x14 => 0xE500,  // Serial services
            0x15 => 0xE600,  // System services (includes memory map)
            0x16 => 0xE700,  // Keyboard services
            0x19 => 0xE800,  // Boot loader
            0x1A => 0xE900,  // Time services
            _ => 0xEF00,     // Default handler (IRET)
        }
    }
}
```

### BIOS Interrupt Services

Windows 7 boot and setup rely on **VBE (VESA BIOS Extensions)** for early boot graphics, before any vendor WDDM driver is loaded. INT 10h must therefore implement both:

- legacy VGA text services (e.g. AH=0x0E TTY write), and
- VBE services (AX=4Fxx) to set a linear framebuffer mode (see [AeroGPU Legacy VGA/VBE Compatibility](./16-aerogpu-vga-vesa-compat.md)).

```rust
impl Bios {
    pub fn handle_int10(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        // Video services
        let function = (cpu.rax >> 8) as u8;
        
        match function {
            0x00 => {
                // Set video mode
                let mode = cpu.rax as u8;
                self.set_video_mode(mode, memory);
            }
            0x4F => {
                // VBE (VESA BIOS Extensions)
                self.handle_int10_vbe(cpu, memory);
            }
            0x01 => {
                // Set cursor shape
                let start = (cpu.rcx >> 8) as u8;
                let end = cpu.rcx as u8;
                // ... set cursor
            }
            0x02 => {
                // Set cursor position
                let page = (cpu.rbx >> 8) as u8;
                let row = (cpu.rdx >> 8) as u8;
                let col = cpu.rdx as u8;
                self.set_cursor_pos(page, row, col, memory);
            }
            0x0E => {
                // TTY write
                let char = cpu.rax as u8;
                self.write_char(char, memory);
            }
            0x0F => {
                // Get video mode
                cpu.rax = (self.current_video_mode() as u64) | (80 << 8);
                cpu.rbx = 0;  // Page 0
            }
            _ => {
                log::debug!("Unhandled INT 10h function: {:02x}", function);
            }
        }
    }

    fn handle_int10_vbe(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        // VBE entry point: AX=4Fxx, subfunction in AL.
        //
        // Minimal set required for Windows boot:
        //  - 4F00h: Get Controller Info
        //  - 4F01h: Get Mode Info
        //  - 4F02h: Set Mode (bit 14 = LFB)
        //  - 4F03h: Get Current Mode
        let sub = cpu.rax as u8; // AL

        match sub {
            0x00 => {
                // ES:DI -> VbeInfoBlock (512 bytes)
                let dst = ((cpu.es.selector as u64) << 4) + (cpu.rdi & 0xFFFF);
                self.write_vbe_controller_info(dst, memory);
                cpu.rax = 0x004F; // success (AL=4Fh, AH=00h)
                cpu.rflags &= !FLAG_CF;
            }
            0x01 => {
                // CX = mode, ES:DI -> ModeInfoBlock (256 bytes)
                let mode = cpu.rcx as u16;
                let dst = ((cpu.es.selector as u64) << 4) + (cpu.rdi & 0xFFFF);
                if self.write_vbe_mode_info(mode, dst, memory).is_ok() {
                    cpu.rax = 0x004F;
                    cpu.rflags &= !FLAG_CF;
                } else {
                    cpu.rax = 0x014F;
                    cpu.rflags |= FLAG_CF;
                }
            }
            0x02 => {
                // BX = mode (bit 14 enables LFB). Return status in AX.
                let mode = cpu.rbx as u16;
                if self.set_vbe_mode(mode, memory).is_ok() {
                    cpu.rax = 0x004F;
                    cpu.rflags &= !FLAG_CF;
                } else {
                    cpu.rax = 0x014F;
                    cpu.rflags |= FLAG_CF;
                }
            }
            0x03 => {
                // Get current mode -> BX
                cpu.rbx = self.current_vbe_mode() as u64;
                cpu.rax = 0x004F;
                cpu.rflags &= !FLAG_CF;
            }
            _ => {
                cpu.rax = 0x014F; // function unsupported
                cpu.rflags |= FLAG_CF;
            }
        }
    }
    
    pub fn handle_int13(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        // Disk services
        let function = (cpu.rax >> 8) as u8;
        let drive = cpu.rdx as u8;
        
        match function {
            0x00 => {
                // Reset disk
                cpu.rax &= !0xFF00;  // Clear AH (status)
                cpu.rflags &= !FLAG_CF;  // Success
            }
            0x02 => {
                // Read sectors
                let count = cpu.rax as u8;
                let cylinder = ((cpu.rcx >> 8) as u16) | (((cpu.rcx as u16) & 0xC0) << 2);
                let head = (cpu.rdx >> 8) as u8;
                let sector = (cpu.rcx & 0x3F) as u8;
                let buffer = ((cpu.es.selector as u64) << 4) + (cpu.rbx & 0xFFFF);
                
                match self.read_disk_sectors(drive, cylinder, head, sector, count, buffer, memory) {
                    Ok(()) => {
                        cpu.rax = (cpu.rax & 0xFF) | 0x0000;  // AH = 0 (success)
                        cpu.rflags &= !FLAG_CF;
                    }
                    Err(status) => {
                        cpu.rax = (cpu.rax & 0xFF) | ((status as u64) << 8);
                        cpu.rflags |= FLAG_CF;
                    }
                }
            }
            0x08 => {
                // Get drive parameters
                if let Some(params) = self.get_drive_params(drive) {
                    cpu.rax &= !0xFF00;
                    cpu.rbx = params.drive_type as u64;
                    cpu.rcx = ((params.cylinders - 1) & 0xFF) as u64
                        | (((params.cylinders - 1) >> 2) & 0xC0) as u64
                        | ((params.sectors & 0x3F) << 8) as u64;
                    cpu.rdx = ((params.heads - 1) << 8) as u64 | params.drives as u64;
                    cpu.rflags &= !FLAG_CF;
                } else {
                    cpu.rax |= 0x0700;  // AH = 7 (error)
                    cpu.rflags |= FLAG_CF;
                }
            }
            0x15 => {
                // Get disk type
                if drive < 0x80 {
                    cpu.rax = 0;  // No floppy
                } else {
                    cpu.rax = 0x0300;  // Hard disk
                }
            }
            0x41 => {
                // Check extensions
                if cpu.rbx == 0x55AA {
                    cpu.rax = 0x3000;  // Version 3.0
                    cpu.rbx = 0xAA55;
                    cpu.rcx = 0x0007;  // Extended functions supported
                    cpu.rflags &= !FLAG_CF;
                } else {
                    cpu.rflags |= FLAG_CF;
                }
            }
            0x42 => {
                // Extended read
                let packet_addr = ((cpu.ds.selector as u64) << 4) + (cpu.rsi & 0xFFFF);
                let packet = self.read_disk_address_packet(packet_addr, memory);
                
                match self.extended_read(drive, &packet, memory) {
                    Ok(()) => {
                        cpu.rax &= !0xFF00;
                        cpu.rflags &= !FLAG_CF;
                    }
                    Err(status) => {
                        cpu.rax = (cpu.rax & 0xFF) | ((status as u64) << 8);
                        cpu.rflags |= FLAG_CF;
                    }
                }
            }
            _ => {
                log::debug!("Unhandled INT 13h function: {:02x}", function);
                cpu.rflags |= FLAG_CF;
            }
        }
    }
    
    pub fn handle_int15(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        // System services
        let function = (cpu.rax >> 8) as u8;
        
        match (function, cpu.rax as u16) {
            (0xE8, 0xE820) => {
                // Get memory map (E820)
                let continuation = cpu.rbx as u32;
                let buffer = ((cpu.es.selector as u64) << 4) + (cpu.rdi & 0xFFFF);
                
                match self.get_e820_entry(continuation, buffer, memory) {
                    Some(next_continuation) => {
                        cpu.rbx = next_continuation as u64;
                        cpu.rcx = 20;  // Entry size
                        cpu.rax = 0x534D4150;  // "SMAP"
                        cpu.rflags &= !FLAG_CF;
                    }
                    None => {
                        cpu.rflags |= FLAG_CF;
                    }
                }
            }
            (0x88, _) => {
                // Get extended memory size (old method)
                let extended_kb = (self.total_memory_bytes / 1024).saturating_sub(1024);
                cpu.rax = extended_kb.min(0xFFFF) as u64;
                cpu.rflags &= !FLAG_CF;
            }
            (0xC0, _) => {
                // Get system configuration
                cpu.rflags |= FLAG_CF;  // Not supported
            }
            _ => {
                log::debug!("Unhandled INT 15h function: {:04x}", cpu.rax as u16);
                cpu.rflags |= FLAG_CF;
            }
        }
    }
}
```

---

## ACPI Tables

### ACPI Table Structure

```rust
pub struct AcpiTables {
    rsdp: Rsdp,
    rsdt: Rsdt,
    xsdt: Xsdt,
    fadt: Fadt,
    madt: Madt,
    hpet: Hpet,
    dsdt: Vec<u8>,  // AML bytecode
}

#[repr(C, packed)]
pub struct Rsdp {
    signature: [u8; 8],      // "RSD PTR "
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    // ACPI 2.0+ fields
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
pub struct AcpiHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

impl AcpiTables {
    pub fn build(&mut self, config: &SystemConfig) -> u64 {
        let base = ACPI_TABLE_BASE;
        let mut offset = 0u64;
        
        // Build DSDT first (referenced by FADT)
        let dsdt_addr = base + offset;
        self.build_dsdt();
        offset += self.dsdt.len() as u64;
        offset = align_up(offset, 16);
        
        // Build FADT
        let fadt_addr = base + offset;
        self.build_fadt(dsdt_addr);
        offset += std::mem::size_of::<Fadt>() as u64;
        offset = align_up(offset, 16);
        
        // Build MADT
        let madt_addr = base + offset;
        self.build_madt(config);
        offset += self.madt_size() as u64;
        offset = align_up(offset, 16);
        
        // Build HPET
        let hpet_addr = base + offset;
        self.build_hpet();
        offset += std::mem::size_of::<Hpet>() as u64;
        offset = align_up(offset, 16);
        
        // Build RSDT/XSDT
        let rsdt_addr = base + offset;
        self.build_rsdt(&[fadt_addr, madt_addr, hpet_addr]);
        offset += self.rsdt_size() as u64;
        offset = align_up(offset, 16);
        
        let xsdt_addr = base + offset;
        self.build_xsdt(&[fadt_addr, madt_addr, hpet_addr]);
        offset += self.xsdt_size() as u64;
        offset = align_up(offset, 16);
        
        // Build RSDP (goes in EBDA)
        self.build_rsdp(rsdt_addr, xsdt_addr);
        
        EBDA_BASE + RSDP_OFFSET
    }
    
    fn build_madt(&mut self, config: &SystemConfig) {
        let mut madt = Vec::new();
        
        // Header
        let header = AcpiHeader {
            signature: *b"APIC",
            length: 0,  // Filled in later
            revision: 3,
            checksum: 0,
            oem_id: *b"Aero  ",
            oem_table_id: *b"AeroMADT",
            oem_revision: 1,
            creator_id: u32::from_le_bytes(*b"Aero"),
            creator_revision: 1,
        };
        madt.extend_from_slice(bytes_of(&header));
        
        // Local APIC address
        madt.extend_from_slice(&LOCAL_APIC_BASE.to_le_bytes());
        
        // Flags
        madt.extend_from_slice(&1u32.to_le_bytes());  // PCAT_COMPAT
        
        // Processor Local APIC entries
        for cpu_id in 0..config.cpu_count {
            madt.push(0);  // Type: Processor Local APIC
            madt.push(8);  // Length
            madt.push(cpu_id as u8);  // ACPI Processor ID
            madt.push(cpu_id as u8);  // APIC ID
            madt.extend_from_slice(&1u32.to_le_bytes());  // Flags: Enabled
        }
        
        // I/O APIC entry
        madt.push(1);  // Type: I/O APIC
        madt.push(12);  // Length
        madt.push(0);  // I/O APIC ID
        madt.push(0);  // Reserved
        madt.extend_from_slice(&IO_APIC_BASE.to_le_bytes());
        madt.extend_from_slice(&0u32.to_le_bytes());  // Global System Interrupt Base
        
        // Interrupt Source Override entries (ISA IRQs)
        // IRQ0 -> GSI2 (timer)
        self.add_iso(&mut madt, 0, 0, 2, 0);
        // IRQ9 -> GSI9 (ACPI SCI)
        // Flags use the MPS INTI encoding: active-low + level-triggered (0b11, 0b11).
        self.add_iso(&mut madt, 0, 9, 9, 0x000F);
        
        // Update length and checksum
        let len = madt.len() as u32;
        madt[4..8].copy_from_slice(&len.to_le_bytes());
        let checksum = self.calculate_checksum(&madt);
        madt[9] = checksum;
        
        self.madt_data = madt;
    }
}
```

### ACPI Power Management (PM1/GPE/SCI/Reset)

Windows 7 uses **fixed-feature ACPI power management** for clean shutdown/reboot:

- **ACPI enable handshake:** OS writes `ACPI_ENABLE` to the `FADT.SMI_CMD` port and expects firmware to set `PM1a_CNT.SCI_EN`.
- **Shutdown (S5):** OS reads `\_S5` from DSDT, then writes `SLP_TYP` + `SLP_EN` to `PM1a_CNT`.
- **Reboot:** OS writes the `FADT.ResetValue` to the `FADT.ResetReg` (if `RESET_REG_SUP` is set).
- **SCI (System Control Interrupt):** Level-triggered interrupt (usually **IRQ9**) asserted when a PM/GPE event is pending and enabled.

#### Recommended minimal register layout (PC-compatible)

Use a simple fixed I/O layout and make FADT/DSDT/device-model agree (the defaults below match `aero-acpi::AcpiConfig`):

| Block | I/O Port | Length | Notes |
|------|----------|--------|------|
| SMI_CMD | `0x00B2` | 1 | Write `ACPI_ENABLE=0xA0` to set `SCI_EN`; `ACPI_DISABLE=0xA1` to clear |
| PM1a_EVT | `0x0400` | 4 | 16-bit `PM1_STS` + 16-bit `PM1_EN` |
| PM1a_CNT | `0x0404` | 2 | `SCI_EN`, `SLP_TYP` (bits 12:10), `SLP_EN` (bit 13) |
| PM_TMR | `0x0408` | 4 | 3.579545MHz free-running counter (low 24 bits used if `TMR_VAL_EXT=0`) |
| GPE0 | `0x0420` | 8 | Status (4 bytes) + Enable (4 bytes) |
| ResetReg | `0x0CF9` | 1 | `ResetValue = 0x06` triggers in-place VM reset |
| SCI IRQ | `IRQ9` | - | Route through PIC or I/O APIC consistently |

#### FADT fields that must be consistent

- `SCI_INT = 9` (or chosen SCI IRQ)
- `SMI_CMD`, `ACPI_ENABLE`, `ACPI_DISABLE`
- `PM1a_EVT_BLK / PM1_EVT_LEN`
- `PM1a_CNT_BLK / PM1_CNT_LEN`
- `PM_TMR_BLK / PM_TMR_LEN` (set to 0 if unimplemented)
- `GPE0_BLK / GPE0_BLK_LEN`
- `RESET_REG_SUP` flag and `ResetReg/ResetValue` if reboot is supported

#### DSDT objects required for shutdown

At minimum, include the sleep-type package that matches the `SLP_TYP` value the PM device expects:

```asl
/*
 * `_PIC` must switch the platform interrupt router between legacy PIC and APIC
 * mode. Many ACPI/APIC OSes (including Windows) rely on `_PIC(1)` to program
 * the chipset IMCR (ports 0x22/0x23); if the OS never writes those ports
 * directly, leaving `_PIC` as a no-op can result in lost interrupts.
 */
OperationRegion (IMCR, SystemIO, 0x22, 0x02)
Field (IMCR, ByteAcc, NoLock, Preserve) { IMCS, 8, IMCD, 8 }
Method (_PIC, 1) { Store (0x70, IMCS); And (Arg0, One, IMCD) }

Name (_S5, Package () { 0x05, 0x05 }) // required for soft-off
```

#### SCI delivery rule (minimal)

SCI should be asserted when:

```
PM1_CNT.SCI_EN == 1 && (
    (PM1_STS & PM1_EN) != 0 ||
    (GPE0_STS & GPE0_EN) != 0
)
```

Clearing the last pending enabled bit (write-1-to-clear in `PM1_STS` / `GPE_STS`) must deassert SCI.

---

## E820 map + PCI holes + high memory

The BIOS reports guest physical memory via **INT 15h, EAX=E820h**. For the canonical PC machine we
follow a Q35-style layout that reserves PCI configuration/MMIO windows below 4 GiB.

**Source of truth:**

- E820 construction: `crates/firmware/src/bios/interrupts.rs::build_e820_map`
- ECAM constants shared with the platform/MMIO wiring: `crates/aero-pc-constants/src/lib.rs`

### Constants (PC/Q35)

- `aero_pc_constants::PCIE_ECAM_BASE = 0xB000_0000`
- `PCIE_ECAM_SIZE = 0x1000_0000` (256 MiB)
- PCI/MMIO hole starts at `0xC000_0000` and extends to `0x1_0000_0000` (4 GiB)

### Layout diagram (when `total_ram > PCIE_ECAM_BASE`)

```
guest physical address space

0x0000_0000 --------------------------------------------------------------+
        low RAM (usable)                                                  |
0xB000_0000 --------------------------------------------------------------+
        PCIe ECAM / MMCONFIG (reserved, 256MiB)                           |
0xC000_0000 --------------------------------------------------------------+
        PCI/MMIO hole (reserved; PCI BARs, LAPIC/IOAPIC/HPET MMIO, etc.)   |
0x1_0000_0000 ------------------------------------------------------------+
        high RAM remap (usable): length = total_ram - 0xB000_0000         |
0x1_0000_0000 + (total_ram - 0xB000_0000) --------------------------------+
```

When the configured RAM exceeds `0xB000_0000`, the BIOS **clamps low RAM** to end at
`PCIE_ECAM_BASE`, reserves the ECAM + PCI/MMIO windows in E820, and **remaps the remainder above
4 GiB starting at `0x1_0000_0000`** so the guest doesn’t silently lose memory.

This implies the emulator must support a **non-contiguous** guest-physical RAM layout; addresses in
the reserved holes must not be backed by RAM (and if not handled by an MMIO device, should behave
like an open bus read).

Implementation note: the canonical hole-aware RAM backend is `memory::MappedGuestMemory`
(`crates/memory/src/mapped.rs`). The PC platform memory buses wrap large RAM configurations using
this mapping (see `crates/platform/src/memory.rs::MemoryBus::wrap_pc_high_memory` and
`crates/aero-machine/src/lib.rs::SystemMemory::new`).

```rust
#[derive(Clone, Copy)]
#[repr(C)]
pub struct E820Entry {
    base: u64,
    length: u64,
    region_type: u32,
    extended_attributes: u32,
}

pub const E820_RAM: u32 = 1;
pub const E820_RESERVED: u32 = 2;
pub const E820_ACPI: u32 = 3;
pub const E820_NVS: u32 = 4;
pub const E820_UNUSABLE: u32 = 5;

impl Bios {
    fn build_e820_map(&self, total_memory: u64) -> Vec<E820Entry> {
        // Pseudo-code for illustration; see `crates/firmware/src/bios/interrupts.rs::build_e820_map`
        // for the exact implementation (including ACPI reserved splits).
        // Reserve a 256MiB PCIe ECAM ("MMCONFIG") window for PCI config space access.
        const PCIE_ECAM_BASE: u64 = 0xB000_0000;
        const PCIE_ECAM_SIZE: u64 = 0x1000_0000;
        // PCI/MMIO window below 4GiB (BAR allocations + APIC/HPET MMIO, etc).
        const PCI_HOLE_START: u64 = 0xC000_0000;
        const PCI_HOLE_END: u64 = 0x1_0000_0000;

        let low_ram_end = total_memory.min(PCIE_ECAM_BASE);

        let mut map = vec![
            // Conventional memory (0 - 0x9F000 = 636KiB)
            E820Entry {
                base: 0x0000_0000,
                length: 0x0009_F000,
                region_type: E820_RAM,
                extended_attributes: 1,
            },
            // EBDA (at 0x9F000 = 636KiB)
            E820Entry {
                base: 0x0009_F000,
                length: 0x0000_1000,
                region_type: E820_RESERVED,
                extended_attributes: 1,
            },
            // Video memory and ROM (0xA0000 - 1MiB)
            E820Entry {
                base: 0x000A_0000,
                length: 0x0006_0000,
                region_type: E820_RESERVED,
                extended_attributes: 1,
            },
            // Extended memory (1MB - below 4GB hole)
            E820Entry {
                base: 0x0010_0000,
                length: low_ram_end.saturating_sub(0x0010_0000),
                region_type: E820_RAM,
                extended_attributes: 1,
            },
            // ACPI tables
            E820Entry {
                base: ACPI_TABLE_BASE,
                length: ACPI_TABLE_SIZE,
                region_type: E820_ACPI,
                extended_attributes: 1,
            },
            // ACPI NVS
            E820Entry {
                base: ACPI_NVS_BASE,
                length: ACPI_NVS_SIZE,
                region_type: E820_NVS,
                extended_attributes: 1,
            },
        ];

        if total_memory > PCIE_ECAM_BASE {
            // PCIe ECAM / MMCONFIG window.
            map.push(E820Entry {
                base: PCIE_ECAM_BASE,
                length: PCIE_ECAM_SIZE,
                region_type: E820_RESERVED,
                extended_attributes: 1,
            });

            // Remaining PCI/MMIO window below 4GiB.
            map.push(E820Entry {
                base: PCI_HOLE_START,
                length: PCI_HOLE_END - PCI_HOLE_START,
                region_type: E820_RESERVED,
                extended_attributes: 1,
            });

            // Remap RAM above 4GiB so the guest doesn't lose memory due to the ECAM hole.
            map.push(E820Entry {
                base: PCI_HOLE_END,
                length: total_memory.saturating_sub(PCIE_ECAM_BASE),
                region_type: E820_RAM,
                extended_attributes: 1,
            });
        }

        map
    }
}
```

---

## PCI Enumeration

```rust
impl Bios {
    fn enumerate_pci(&mut self, memory: &mut MemoryBus) {
        // Scan all buses, devices, and functions
        for bus in 0..256u16 {
            for device in 0..32u8 {
                for function in 0..8u8 {
                    let vendor_id = self.pci_read_config(bus as u8, device, function, 0) as u16;
                    
                    if vendor_id == 0xFFFF {
                        continue;  // No device
                    }
                    
                     let device_id = (self.pci_read_config(bus as u8, device, function, 0) >> 16) as u16;
                     let class_code = self.pci_read_config(bus as u8, device, function, 8);
                     let intr = self.pci_read_config(bus as u8, device, function, 0x3C);
                     let interrupt_pin = ((intr >> 8) & 0xFF) as u8; // 1=INTA#, 2=INTB#, ...
                     let irq = self.assign_irq(device, interrupt_pin);
                     
                     // Write back the Interrupt Line register (0x3C) so the guest sees the routing.
                     self.pci_write_config_byte(bus as u8, device, function, 0x3C, irq);
                     
                     let pci_device = PciDevice {
                         bus: bus as u8,
                         device,
                         function,
                         vendor_id,
                         device_id,
                         class_code: (class_code >> 8) & 0xFFFFFF,
                         subsystem_id: 0,
                        irq,
                     };
                    
                    self.pci_devices.push(pci_device);
                    
                    // Check if multi-function device
                    if function == 0 {
                        let header_type = (self.pci_read_config(bus as u8, device, 0, 0xC) >> 16) as u8;
                        if header_type & 0x80 == 0 {
                            break;  // Single function device
                        }
                    }
                }
            }
        }
    }
    
    fn assign_irq(&self, device: u8, interrupt_pin: u8) -> u8 {
        // QEMU-style INTx swizzle:
        //   PIRQ = (pin + device) mod 4
        // and map PIRQ[A-D] -> IRQ/GSI 10-13 (IOAPIC input pins in APIC mode, ISA IRQs in PIC mode).
        if interrupt_pin == 0 {
            return 0xFF; // No INTx pin.
        }
        let pin = (interrupt_pin - 1) as usize; // 0=INTA#, 1=INTB#, ...
        let pirq = (pin + device as usize) & 3;
        [10, 11, 12, 13][pirq]
    }
}
```

> ACPI note: the DSDT `_PRT` for `\_SB.PCI0` must report the same GSI numbers (10-13) for each device/pin swizzled onto PIRQA-D. No MADT Interrupt Source Override (ISO) entries are needed for these lines since they are not remapped ISA IRQs.
>
> With the swizzle `PIRQ = (pin + device) mod 4` and `PIRQ[A-D] -> GSI[10-13]`, the first four PCI device numbers map like this (then repeat every 4 devices):
>
> | PCI device | INTA# | INTB# | INTC# | INTD# |
> | ---------- | ----- | ----- | ----- | ----- |
> | 0          | 10    | 11    | 12    | 13    |
> | 1          | 11    | 12    | 13    | 10    |
> | 2          | 12    | 13    | 10    | 11    |
> | 3          | 13    | 10    | 11    | 12    |

---

## HPET (High Precision Event Timer)

```rust
pub struct HpetDevice {
    // Configuration registers
    general_capabilities: u64,
    general_configuration: u64,
    general_interrupt_status: u64,
    main_counter: u64,
    
    // Timer comparators
    timers: [HpetTimer; 3],
    
    // Clock frequency
    period_femtoseconds: u64,
}

impl HpetDevice {
    pub fn new() -> Self {
        Self {
            general_capabilities: 
                (2 << 8) |      // NUM_TIM_CAP: 3 timers
                (1 << 13) |     // COUNT_SIZE_CAP: 64-bit
                (0x10DE << 16), // VENDOR_ID
            general_configuration: 0,
            general_interrupt_status: 0,
            main_counter: 0,
            timers: [HpetTimer::default(); 3],
            period_femtoseconds: 100_000_000,  // 10ns = 100MHz
        }
    }
    
    pub fn read(&self, offset: u64) -> u64 {
        match offset {
            0x000 => self.general_capabilities | (self.period_femtoseconds << 32),
            0x010 => self.general_configuration,
            0x020 => self.general_interrupt_status,
            0x0F0 => self.main_counter,
            0x100..=0x1FF => self.read_timer((offset - 0x100) / 0x20, offset % 0x20),
            _ => 0,
        }
    }
    
    pub fn write(&mut self, offset: u64, value: u64) {
        match offset {
            0x010 => {
                let was_enabled = self.general_configuration & 1 != 0;
                self.general_configuration = value;
                
                if !was_enabled && value & 1 != 0 {
                    // Timer just enabled
                }
            }
            0x020 => {
                // Clear interrupt status (write 1 to clear)
                self.general_interrupt_status &= !value;
            }
            0x0F0 => {
                if self.general_configuration & 1 == 0 {
                    self.main_counter = value;
                }
            }
            0x100..=0x1FF => self.write_timer((offset - 0x100) / 0x20, offset % 0x20, value),
            _ => {}
        }
    }
    
    pub fn tick(&mut self, nanoseconds: u64) -> Option<u8> {
        if self.general_configuration & 1 == 0 {
            return None;  // Timer disabled
        }
        
        let ticks = nanoseconds * 1_000_000 / self.period_femtoseconds;
        self.main_counter = self.main_counter.wrapping_add(ticks);
        
        // Check timer comparators
        for (i, timer) in self.timers.iter_mut().enumerate() {
            if timer.check_and_fire(self.main_counter) {
                return Some(timer.irq);
            }
        }
        
        None
    }
}
```

---

## Boot Sequence

```rust
impl Bios {
    fn boot(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        // Find boot device
        let boot_device = self.find_boot_device();
        
        // Read MBR (first sector)
        let mut mbr = [0u8; 512];
        self.read_sector(boot_device, 0, &mut mbr);
        
        // Verify boot signature
        if mbr[510] != 0x55 || mbr[511] != 0xAA {
            panic!("Invalid boot signature");
        }
        
        // Copy MBR to 0x7C00
        memory.write_bytes(0x7C00, &mbr);
        
        // Set up registers for boot
        cpu.rax = 0;
        cpu.rbx = 0;
        cpu.rcx = 0;
        cpu.rdx = boot_device as u64;  // Boot drive in DL
        cpu.rsi = 0;
        cpu.rdi = 0;
        cpu.rbp = 0;
        cpu.rsp = 0x7C00;
        
        // Jump to boot sector
        cpu.cs.selector = 0x0000;
        cpu.cs.base = 0x0000_0000;
        cpu.ds.selector = 0x0000;
        cpu.es.selector = 0x0000;
        cpu.rip = 0x7C00;
    }
}
```

---

## Next Steps

- See [Performance Optimization](./10-performance-optimization.md) for boot time improvements
- See [Task Breakdown](./15-agent-task-breakdown.md) for firmware tasks

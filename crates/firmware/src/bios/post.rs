use std::sync::Arc;

use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_IF};

use super::{
    ivt, pci::PciConfigSpace, rom, set_real_mode_seg, Bios, BiosBus, BiosMemoryBus, BlockDevice,
    BIOS_ALIAS_BASE, BIOS_BASE, BIOS_SEGMENT, EBDA_BASE,
};
use crate::smbios::{SmbiosConfig, SmbiosTables};

impl Bios {
    pub(super) fn post_impl(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        pci: Option<&mut dyn PciConfigSpace>,
    ) {
        // Reset transient POST state.
        self.e820_map.clear();
        self.pci_devices.clear();
        self.rsdp_addr = None;
        self.acpi_reclaimable = None;
        self.acpi_nvs = None;
        self.smbios_eps_addr = None;
        self.last_int13_status = 0;
        self.tty_output.clear();

        // 0) Install ROM stubs (read-only).
        //
        // The BIOS ROM is mapped twice:
        // - `BIOS_BASE` (the conventional `F0000..=FFFFF` real-mode window)
        // - `BIOS_ALIAS_BASE` (the 32-bit reset-vector alias at `FFFF_0000..=FFFF_FFFF`)
        //
        // Note: `BIOS_ALIAS_BASE` is outside typical guest RAM. Bus implementations that only
        // model RAM may need to treat ROM mappings as sparse.
        let rom_image: Arc<[u8]> = rom::build_bios_rom().into();
        bus.map_rom(BIOS_BASE, rom_image.clone());
        bus.map_rom(BIOS_ALIAS_BASE, rom_image);

        // 1) Real-mode CPU init: interrupts disabled during POST.
        cpu.mode = CpuMode::Real;
        cpu.halted = false;
        cpu.clear_pending_bios_int();
        cpu.set_rflags(0);
        cpu.a20_enabled = bus.a20_enabled();

        set_real_mode_seg(&mut cpu.segments.cs, BIOS_SEGMENT);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        set_real_mode_seg(&mut cpu.segments.es, 0);
        set_real_mode_seg(&mut cpu.segments.ss, 0);
        cpu.gpr[gpr::RSP] = 0x7C00;
        cpu.set_rip(super::RESET_VECTOR_OFFSET); // conventional reset vector within F000 segment

        // 2) BDA/EBDA: reserve a 4KiB EBDA page below 1MiB and advertise base memory size.
        ivt::init_bda(bus, self.config.boot_drive);
        self.init(bus);
        // Initialize VGA text mode state (mode 03h) so software querying BDA/INT 10h gets sane
        // defaults without needing to explicitly set a mode first.
        self.video
            .vga
            .set_text_mode_03h(&mut BiosMemoryBus::new(bus), true);
        self.video_mode = 0x03;

        // 3) Interrupt Vector Table.
        ivt::init_ivt(bus);

        // 4) SMBIOS: publish the SMBIOS EPS in the EBDA so Windows can discover it.
        //
        // Keep the EPS within the first 1KiB of EBDA (per spec) while avoiding the RSDP slot.
        let smbios_cfg = SmbiosConfig {
            ram_bytes: self.config.memory_size_bytes,
            cpu_count: self.config.cpu_count.max(1),
            uuid_seed: 0,
            eps_addr: Some((EBDA_BASE + 0x200) as u32),
            table_addr: Some((EBDA_BASE + 0x400) as u32),
        };
        let mut smbios_bus = BiosMemoryBus::new(bus);
        self.smbios_eps_addr = Some(SmbiosTables::build_and_write(&smbios_cfg, &mut smbios_bus));

        // 5) Enable A20 (fast A20 path; the bus owns the gating behaviour).
        //
        // This must happen before writing any firmware tables above 1MiB (ACPI reclaimable blobs).
        bus.set_a20_enabled(true);
        cpu.a20_enabled = bus.a20_enabled();

        // 6) Optional PCI enumeration + deterministic IRQ routing (must match ACPI `_PRT`).
        if let Some(pci) = pci {
            self.enumerate_pci(pci);
        }

        // 7) ACPI tables (generated via `aero-acpi`).
        if self.config.enable_acpi {
            if let Some(info) = self.acpi_builder.build_and_write(
                bus,
                self.config.memory_size_bytes,
                self.config.cpu_count,
                self.config.pirq_to_gsi,
                self.config.acpi_placement,
            ) {
                self.rsdp_addr = Some(info.rsdp_addr);
                self.acpi_reclaimable = Some(info.reclaimable);
                self.acpi_nvs = Some(info.nvs);
            }
        }

        // 8) Re-enable interrupts after POST.
        cpu.rflags |= RFLAGS_IF;

        // 9) Boot: load sector 0 to 0x7C00 and jump.
        if let Err(msg) = self.boot(cpu, bus, disk) {
            self.bios_panic(cpu, bus, msg);
        }
    }

    fn boot(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
    ) -> Result<(), &'static str> {
        let mut sector = [0u8; 512];
        disk.read_sector(0, &mut sector)
            .map_err(|_| "Disk read error")?;

        if sector[510] != 0x55 || sector[511] != 0xAA {
            return Err("Invalid boot signature");
        }

        bus.write_physical(0x7C00, &sector);

        // Register setup per BIOS conventions.
        cpu.gpr[gpr::RAX] = 0;
        cpu.gpr[gpr::RBX] = 0;
        cpu.gpr[gpr::RCX] = 0;
        cpu.gpr[gpr::RDX] = self.config.boot_drive as u64; // DL
        cpu.gpr[gpr::RSI] = 0;
        cpu.gpr[gpr::RDI] = 0;
        cpu.gpr[gpr::RBP] = 0;
        cpu.gpr[gpr::RSP] = 0x7C00;

        set_real_mode_seg(&mut cpu.segments.cs, 0x0000);
        set_real_mode_seg(&mut cpu.segments.ds, 0x0000);
        set_real_mode_seg(&mut cpu.segments.es, 0x0000);
        set_real_mode_seg(&mut cpu.segments.ss, 0x0000);
        cpu.set_rip(0x7C00);

        Ok(())
    }

    fn bios_panic(&mut self, cpu: &mut CpuState, _bus: &mut dyn BiosBus, msg: &'static str) {
        // The minimal implementation keeps this simple: record the message in the TTY buffer
        // and halt the CPU. A future VGA implementation can render this to 0xB8000.
        self.tty_output.extend_from_slice(msg.as_bytes());
        self.tty_output.push(b'\n');
        cpu.halted = true;
    }
}

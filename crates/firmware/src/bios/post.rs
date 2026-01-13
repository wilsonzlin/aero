use std::sync::Arc;

use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_IF};

use super::{
    eltorito, ivt, pci::PciConfigSpace, rom, set_real_mode_seg, Bios, BiosBus, BiosMemoryBus,
    BlockDevice, ElToritoBootInfo, ElToritoBootMediaType, BIOS_ALIAS_BASE, BIOS_BASE, BIOS_SEGMENT,
    EBDA_BASE,
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
        self.el_torito_boot_info = None;
        self.unhandled_interrupt_log_count = 0;
        self.clear_tty_output();

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
            uuid_seed: self.config.smbios_uuid_seed,
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
            match self.acpi_builder.build_and_write(
                bus,
                self.config.memory_size_bytes,
                self.config.cpu_count,
                self.config.pirq_to_gsi,
                self.config.acpi_placement,
            ) {
                Ok(info) => {
                    self.rsdp_addr = Some(info.rsdp_addr);
                    self.acpi_reclaimable = Some(info.reclaimable);
                    self.acpi_nvs = Some(info.nvs);
                }
                Err(err) => {
                    let msg = format!("BIOS: ACPI build failed: {err}");
                    self.bios_diag(bus, &msg);
                }
            };
        }

        // 8) Re-enable interrupts after POST.
        cpu.rflags |= RFLAGS_IF;

        // 9) Boot: load sector 0 to 0x7C00 and jump.
        if let Err(msg) = self.boot(cpu, bus, disk) {
            self.bios_panic(cpu, bus, msg);
        }
    }

    /// Load the configured boot device into memory and initialize the real-mode CPU state
    /// (registers, data segments, stack) to match common BIOS boot conventions.
    ///
    /// This helper is shared by:
    /// - POST boot (direct jump), and
    /// - INT 19h bootstrap reload (via a synthetic IRET frame).
    ///
    /// Returns the boot entry point as a real-mode `CS:IP` pair.
    pub(super) fn boot_from_configured_device(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
    ) -> Result<(u16, u16), &'static str> {
        let boot_drive = self.config.boot_drive;

        let (entry_cs, entry_ip) = if (0xE0..=0xEF).contains(&boot_drive) {
            self.load_eltorito_cd_boot_image(bus, disk)?
        } else {
            self.load_mbr_boot_sector(bus, disk)?
        };

        // Register setup per BIOS conventions.
        cpu.gpr[gpr::RAX] = 0;
        cpu.gpr[gpr::RBX] = 0;
        cpu.gpr[gpr::RCX] = 0;
        cpu.gpr[gpr::RDX] = boot_drive as u64; // DL
        cpu.gpr[gpr::RSI] = 0;
        cpu.gpr[gpr::RDI] = 0;
        cpu.gpr[gpr::RBP] = 0;

        // Use a clean 0000:7C00 stack, matching typical BIOS boot handoff.
        cpu.gpr[gpr::RSP] = 0x7C00;
        set_real_mode_seg(&mut cpu.segments.ds, 0x0000);
        set_real_mode_seg(&mut cpu.segments.es, 0x0000);
        set_real_mode_seg(&mut cpu.segments.ss, 0x0000);

        Ok((entry_cs, entry_ip))
    }

    fn boot(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
    ) -> Result<(), &'static str> {
        let (entry_cs, entry_ip) = self.boot_from_configured_device(cpu, bus, disk)?;

        // Transfer control to the loaded boot image.
        set_real_mode_seg(&mut cpu.segments.cs, entry_cs);
        cpu.set_rip(entry_ip as u64);

        Ok(())
    }

    fn load_mbr_boot_sector(
        &self,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
    ) -> Result<(u16, u16), &'static str> {
        let mut sector = [0u8; 512];
        disk.read_sector(0, &mut sector)
            .map_err(|_| "Disk read error")?;

        if sector[510] != 0x55 || sector[511] != 0xAA {
            return Err("Invalid boot signature");
        }

        bus.write_physical(0x7C00, &sector);
        Ok((0x0000, 0x7C00))
    }

    fn load_eltorito_cd_boot_image(
        &mut self,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
    ) -> Result<(u16, u16), &'static str> {
        let parsed = eltorito::parse_boot_image(disk)?;
        let entry = parsed.image;
        let start_lba = u64::from(entry.load_rba)
            .checked_mul(4)
            .ok_or("El Torito boot image load past end-of-image")?;
        let count = u64::from(entry.sector_count);
        let dst = (u64::from(entry.load_segment)) << 4;
        for i in 0..count {
            let mut buf = [0u8; 512];
            disk.read_sector(start_lba + i, &mut buf)
                .map_err(|_| "Disk read error")?;
            bus.write_physical(dst + i * 512, &buf);
        }

        // Cache boot metadata for INT 13h AH=4Bh ("El Torito disk emulation services").
        //
        // This is commonly used by CD boot loaders such as ISOLINUX, even when running in
        // "no emulation" mode.
        self.el_torito_boot_info = Some(ElToritoBootInfo {
            media_type: ElToritoBootMediaType::NoEmulation,
            boot_drive: self.config.boot_drive,
            controller_index: 0,
            boot_catalog_lba: Some(entry.boot_catalog_lba),
            boot_image_lba: Some(entry.load_rba),
            load_segment: Some(entry.load_segment),
            sector_count: Some(entry.sector_count),
        });

        Ok((entry.load_segment, 0x0000))
    }

    fn bios_diag(&mut self, bus: &mut dyn BiosBus, msg: &str) {
        // Record the message in the TTY buffer for programmatic inspection.
        self.push_tty_bytes(msg.as_bytes());
        let needs_newline = !msg.as_bytes().last().is_some_and(|b| *b == b'\n');
        if needs_newline {
            self.push_tty_byte(b'\n');
        }

        // Best-effort: also render to the VGA text buffer so the reason is visible on the guest
        // display when POST fails before a bootloader takes over.
        //
        // Use the INT 10h teletype helper so we update both the on-screen text window and the
        // BIOS Data Area cursor state. Avoid propagating panics from odd VGA/BDA state or unusual
        // memory bus implementations; a panic during POST would be worse than missing output.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut mem = BiosMemoryBus::new(bus);
            for &ch in msg.as_bytes() {
                self.video.vga.teletype_output(&mut mem, 0, ch, 0x07);
            }
            if needs_newline {
                self.video.vga.teletype_output(&mut mem, 0, b'\n', 0x07);
            }
        }));
    }

    pub(super) fn bios_panic(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        msg: &'static str,
    ) {
        // Record the message in the TTY buffer for programmatic inspection.
        self.push_tty_bytes(msg.as_bytes());
        let needs_newline = !msg.as_bytes().last().is_some_and(|b| *b == b'\n');
        if needs_newline {
            self.push_tty_byte(b'\n');
        }

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Best-effort: render directly to the legacy VGA text buffer so real users see boot
            // failures even when no serial console is attached.
            super::render_message_to_vga_text_line0(bus, msg);
        }));
        cpu.halted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::{BiosConfig, InMemoryDisk, TestMemory};

    #[test]
    fn post_panic_renders_message_to_vga_text_buffer() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);

        // Invalid boot signature should trigger a BIOS panic.
        let bad_sector = [0u8; 512];
        let mut disk = InMemoryDisk::from_boot_sector(bad_sector);

        bios.post(&mut cpu, &mut mem, &mut disk);

        assert!(cpu.halted);

        let msg = b"Invalid boot signature";
        assert!(bios
            .tty_output()
            .windows(msg.len())
            .any(|window| window == msg));

        // VGA text mode is word-packed: [char, attr] pairs. Scan only char bytes.
        let vga = mem.read_bytes(0xB8000, 0x8000);
        let chars: Vec<u8> = vga.iter().step_by(2).copied().collect();
        assert!(chars
            .windows(msg.len())
            .any(|window| window == msg));
    }
}

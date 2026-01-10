use machine::{CpuState, FLAG_ALWAYS_ON, FLAG_IF};

use super::{acpi, ivt, rom, seg, Bios, BiosBus, BIOS_BASE, BIOS_SEGMENT};

impl Bios {
    pub(super) fn post_impl(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn machine::BlockDevice,
    ) {
        // 0) Install ROM stubs (read-only).
        let rom_image = rom::build_bios_rom();
        bus.map_rom(BIOS_BASE, &rom_image);

        // 1) Real-mode CPU init: interrupts disabled during POST.
        cpu.halted = false;
        cpu.pending_bios_int = None;
        cpu.rflags = FLAG_ALWAYS_ON;
        cpu.rflags &= !FLAG_IF;

        cpu.cs = seg(BIOS_SEGMENT);
        cpu.ds = seg(0);
        cpu.es = seg(0);
        cpu.ss = seg(0);
        cpu.rsp = 0x7C00;
        cpu.rip = 0xFFF0; // conventional reset vector within F000 segment

        // 2) BDA/EBDA: reserve a 4KiB EBDA page below 1MiB and advertise base memory size.
        ivt::init_bda(bus);

        // 3) Interrupt Vector Table.
        ivt::init_ivt(bus);

        // 4) ACPI tables (builder call-out + fixed placement contract).
        self.rsdp_addr = self.acpi_builder.build(bus, acpi::default_placement());

        // 5) Enable A20 (fast A20 path; the bus owns the gating behaviour).
        bus.set_a20_enabled(true);

        // 6) Re-enable interrupts after POST.
        cpu.rflags |= FLAG_IF;

        // 7) Boot: load sector 0 to 0x7C00 and jump.
        if let Err(msg) = self.boot(cpu, bus, disk) {
            self.bios_panic(cpu, bus, msg);
        }
    }

    fn boot(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn machine::BlockDevice,
    ) -> Result<(), &'static str> {
        let mut sector = [0u8; 512];
        disk.read_sector(0, &mut sector)
            .map_err(|_| "Disk read error")?;

        if sector[510] != 0x55 || sector[511] != 0xAA {
            return Err("Invalid boot signature");
        }

        bus.write_physical(0x7C00, &sector);

        // Register setup per BIOS conventions.
        cpu.rax = 0;
        cpu.rbx = 0;
        cpu.rcx = 0;
        cpu.rdx = self.config.boot_drive as u64; // DL
        cpu.rsi = 0;
        cpu.rdi = 0;
        cpu.rbp = 0;
        cpu.rsp = 0x7C00;

        cpu.cs = seg(0x0000);
        cpu.ds = seg(0x0000);
        cpu.es = seg(0x0000);
        cpu.ss = seg(0x0000);
        cpu.rip = 0x7C00;

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

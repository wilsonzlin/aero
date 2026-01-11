use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit, StepExit};
use aero_cpu_core::mem::CpuBus as CoreCpuBus;
use aero_cpu_core::state::{
    gpr, CpuMode as CoreCpuMode, CpuState as CoreCpuState, Segment as CoreSegment, FLAG_CF, FLAG_ZF,
};
use firmware::bda::BiosDataArea;
use firmware::bios::{
    build_bios_rom, Bios, BiosBus, BiosConfig, BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR,
    BIOS_ALIAS_BASE, BIOS_BASE, BIOS_SEGMENT, RESET_VECTOR_OFFSET, TICKS_PER_DAY,
};
use firmware::rtc::{CmosRtc, DateTime};
use machine::{
    A20Gate, BlockDevice, CpuState as MachineCpuState, FirmwareMemory, MemoryAccess,
    PhysicalMemory, Segment as MachineSegment,
};
use std::time::Duration;

const TEST_MEM_SIZE_BYTES: u64 = 16 * 1024 * 1024;
const TEST_MEM_SIZE: usize = TEST_MEM_SIZE_BYTES as usize;

fn test_bios_config() -> BiosConfig {
    BiosConfig {
        memory_size_bytes: TEST_MEM_SIZE_BYTES,
        boot_drive: 0x80,
        ..Default::default()
    }
}

fn boot_sector_with(bytes: &[u8]) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let len = bytes.len().min(510);
    sector[..len].copy_from_slice(&bytes[..len]);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn set_real_mode_seg(seg: &mut CoreSegment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

fn core_reset_state() -> CoreCpuState {
    let mut state = CoreCpuState::new(CoreCpuMode::Real);
    // Hardware reset: CS:IP = F000:FFF0 (physical 0xFFFF_FFF0 via the BIOS alias mapping).
    state.segments.cs.selector = BIOS_SEGMENT;
    state.segments.cs.base = BIOS_ALIAS_BASE;
    state.segments.cs.limit = 0xFFFF;
    state.segments.cs.access = 0;
    state.set_rip(RESET_VECTOR_OFFSET);
    set_real_mode_seg(&mut state.segments.ds, 0);
    set_real_mode_seg(&mut state.segments.es, 0);
    set_real_mode_seg(&mut state.segments.ss, 0);
    set_real_mode_seg(&mut state.segments.fs, 0);
    set_real_mode_seg(&mut state.segments.gs, 0);
    state
}

fn sync_machine_to_core(machine: &MachineCpuState, core: &mut CoreCpuState) {
    core.mode = CoreCpuMode::Real;
    core.halted = machine.halted;
    core.clear_pending_bios_int();

    core.gpr[gpr::RAX] = machine.rax;
    core.gpr[gpr::RCX] = machine.rcx;
    core.gpr[gpr::RDX] = machine.rdx;
    core.gpr[gpr::RBX] = machine.rbx;
    core.gpr[gpr::RSP] = machine.rsp;
    core.gpr[gpr::RBP] = machine.rbp;
    core.gpr[gpr::RSI] = machine.rsi;
    core.gpr[gpr::RDI] = machine.rdi;
    core.gpr[gpr::R8] = 0;
    core.gpr[gpr::R9] = 0;
    core.gpr[gpr::R10] = 0;
    core.gpr[gpr::R11] = 0;
    core.gpr[gpr::R12] = 0;
    core.gpr[gpr::R13] = 0;
    core.gpr[gpr::R14] = 0;
    core.gpr[gpr::R15] = 0;

    core.set_rip(machine.rip);
    core.set_rflags(machine.rflags);

    set_real_mode_seg(&mut core.segments.cs, machine.cs.selector);
    set_real_mode_seg(&mut core.segments.ds, machine.ds.selector);
    set_real_mode_seg(&mut core.segments.es, machine.es.selector);
    set_real_mode_seg(&mut core.segments.ss, machine.ss.selector);
}

fn sync_core_to_machine(core: &CoreCpuState, machine: &mut MachineCpuState) {
    machine.rax = core.gpr[gpr::RAX];
    machine.rbx = core.gpr[gpr::RBX];
    machine.rcx = core.gpr[gpr::RCX];
    machine.rdx = core.gpr[gpr::RDX];
    machine.rsi = core.gpr[gpr::RSI];
    machine.rdi = core.gpr[gpr::RDI];
    machine.rbp = core.gpr[gpr::RBP];
    machine.rsp = core.gpr[gpr::RSP];
    machine.rip = core.rip();
    machine.rflags = core.rflags();

    machine.cs = MachineSegment {
        selector: core.segments.cs.selector,
    };
    machine.ds = MachineSegment {
        selector: core.segments.ds.selector,
    };
    machine.es = MachineSegment {
        selector: core.segments.es.selector,
    };
    machine.ss = MachineSegment {
        selector: core.segments.ss.selector,
    };

    machine.pending_bios_int = None;
    machine.halted = core.halted;
}

struct BiosTestMemory {
    inner: PhysicalMemory,
}

impl BiosTestMemory {
    fn new(size: usize) -> Self {
        Self {
            inner: PhysicalMemory::new(size),
        }
    }
}

impl A20Gate for BiosTestMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.inner.set_a20_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.inner.a20_enabled()
    }
}

impl FirmwareMemory for BiosTestMemory {
    fn map_rom(&mut self, base: u64, rom: &[u8]) {
        self.inner.map_rom(base, rom);
    }
}

impl MemoryAccess for BiosTestMemory {
    fn read_u8(&self, addr: u64) -> u8 {
        self.inner.read_u8(addr)
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        self.inner.write_u8(addr, val)
    }

    fn fetch_code(&self, addr: u64, len: usize) -> &[u8] {
        self.inner.fetch_code(addr, len)
    }
}

impl CoreCpuBus for BiosTestMemory {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception> {
        Ok(MemoryAccess::read_u8(self, vaddr))
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, aero_cpu_core::Exception> {
        Ok(MemoryAccess::read_u16(self, vaddr))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, aero_cpu_core::Exception> {
        Ok(MemoryAccess::read_u32(self, vaddr))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, aero_cpu_core::Exception> {
        Ok(MemoryAccess::read_u64(self, vaddr))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, aero_cpu_core::Exception> {
        Ok(MemoryAccess::read_u128(self, vaddr))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception> {
        MemoryAccess::write_u8(self, vaddr, val);
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), aero_cpu_core::Exception> {
        MemoryAccess::write_u16(self, vaddr, val);
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), aero_cpu_core::Exception> {
        MemoryAccess::write_u32(self, vaddr, val);
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), aero_cpu_core::Exception> {
        MemoryAccess::write_u64(self, vaddr, val);
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), aero_cpu_core::Exception> {
        MemoryAccess::write_u128(self, vaddr, val);
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = MemoryAccess::read_u8(self, vaddr + i as u64);
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, aero_cpu_core::Exception> {
        Ok(0)
    }

    fn io_write(
        &mut self,
        _port: u16,
        _size: u32,
        _val: u64,
    ) -> Result<(), aero_cpu_core::Exception> {
        Ok(())
    }
}

struct CoreVm<D: BlockDevice> {
    cpu: CoreCpuState,
    mem: BiosTestMemory,
    bios: Bios,
    disk: D,
    assist: AssistContext,
}

impl<D: BlockDevice> CoreVm<D> {
    fn new(mem_size: usize, bios: Bios, disk: D) -> Self {
        Self {
            cpu: CoreCpuState::new(CoreCpuMode::Real),
            mem: BiosTestMemory::new(mem_size),
            bios,
            disk,
            assist: AssistContext::default(),
        }
    }

    fn reset(&mut self) {
        let rom = build_bios_rom();
        // Map ROM at both the conventional real-mode window and the top-of-4GiB reset alias.
        self.mem.map_rom(BIOS_BASE, &rom);
        self.mem.map_rom(BIOS_ALIAS_BASE, &rom);
        // Keep A20 enabled while executing from the 0xFFFF_0000 alias mapping.
        self.mem.set_a20_enabled(true);

        // Start from the architectural reset vector (alias at 0xFFFF_FFF0) and run
        // until the ROM fallback stub halts. The real BIOS POST is performed in host
        // code, so this just validates that ROM + reset mapping is wired correctly.
        self.cpu = core_reset_state();
        let mut executed = 0u64;
        while executed < 32 {
            let res = run_batch_with_assists(&mut self.assist, &mut self.cpu, &mut self.mem, 1);
            executed += res.executed;
            match res.exit {
                BatchExit::Completed | BatchExit::Branch => continue,
                BatchExit::Halted => break,
                BatchExit::BiosInterrupt(vector) => {
                    panic!("unexpected BIOS interrupt during reset: {vector:#x}")
                }
                BatchExit::Assist(r) => panic!("unexpected unhandled assist during reset: {r:?}"),
                BatchExit::Exception(e) => panic!("unexpected exception during reset: {e:?}"),
            }
        }

        // BIOS POST + boot sector load (host implementation).
        let mut machine_cpu = MachineCpuState::default();
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.post(&mut machine_cpu, bus, &mut self.disk);

        sync_machine_to_core(&machine_cpu, &mut self.cpu);
        self.cpu.halted = false;
    }

    fn step(&mut self) -> StepExit {
        let res = run_batch_with_assists(&mut self.assist, &mut self.cpu, &mut self.mem, 1);
        let exit = match res.exit {
            BatchExit::Completed => StepExit::Continue,
            BatchExit::Branch => StepExit::Branch,
            BatchExit::Halted => StepExit::Halted,
            BatchExit::BiosInterrupt(vector) => StepExit::BiosInterrupt(vector),
            BatchExit::Assist(r) => StepExit::Assist(r),
            BatchExit::Exception(e) => panic!("cpu exception: {e:?}"),
        };

        if let StepExit::BiosInterrupt(vector) = exit {
            let mut machine_cpu = MachineCpuState::default();
            sync_core_to_machine(&self.cpu, &mut machine_cpu);
            let bus: &mut dyn BiosBus = &mut self.mem;
            self.bios
                .dispatch_interrupt(vector, &mut machine_cpu, bus, &mut self.disk);
            sync_machine_to_core(&machine_cpu, &mut self.cpu);
        }

        exit
    }
}

#[test]
fn aero_cpu_core_vm_resets_to_0x7c00_with_dl_set() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[0x90, 0x90, 0x90]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    assert_eq!(vm.cpu.segments.cs.selector, 0x0000);
    assert_eq!(vm.cpu.rip(), 0x7C00);
    assert_eq!(vm.cpu.gpr[gpr::RSP] as u16, 0x7C00);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u8, 0x80);

    assert!(matches!(vm.step(), StepExit::Continue)); // first NOP
    assert_eq!(vm.cpu.rip(), 0x7C01);
    assert!(matches!(vm.step(), StepExit::Continue)); // second NOP
    assert_eq!(vm.cpu.rip(), 0x7C02);
}

#[test]
fn aero_cpu_core_int10_tty_hypercall_roundtrip() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 10h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x10, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0E00 | (b'A' as u64);

    assert!(matches!(vm.step(), StepExit::Branch)); // INT
    assert_eq!(vm.cpu.segments.cs.selector, BIOS_SEGMENT);

    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x10))); // HLT in ROM stub
    assert!(matches!(vm.step(), StepExit::Branch)); // IRET
    assert_eq!(vm.cpu.segments.cs.selector, 0x0000);
    assert_eq!(vm.cpu.rip(), 0x7C02);
    assert!(matches!(vm.step(), StepExit::Halted)); // final HLT

    assert_eq!(vm.bios.tty_output(), b"A");
    assert_eq!(vm.mem.read_u8(0xB8000), b'A');
    assert_eq!(vm.mem.read_u8(0xB8001), 0x07);
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&vm.mem), (0, 1));
}

#[test]
fn aero_cpu_core_int13_chs_read_reads_second_sector_into_memory() {
    let bios = Bios::new(test_bios_config());

    // Two sectors: boot sector + one data sector.
    let mut disk_bytes = vec![0u8; 2 * 512];
    disk_bytes[510] = 0x55;
    disk_bytes[511] = 0xAA;
    disk_bytes[512] = 0x42;
    let disk = machine::InMemoryDisk::new(disk_bytes);

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // CHS read 1 sector from cylinder 0, head 0, sector 2 into 0x0000:0x0500.
    vm.cpu.gpr[gpr::RAX] = 0x0201;
    vm.cpu.gpr[gpr::RCX] = 0x0002; // CH=0, CL=2
    vm.cpu.gpr[gpr::RDX] = 0x0080; // DH=0, DL=0x80
    set_real_mode_seg(&mut vm.cpu.segments.es, 0x0000);
    vm.cpu.gpr[gpr::RBX] = 0x0500;

    assert!(matches!(vm.step(), StepExit::Branch)); // INT
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch)); // IRET
    assert!(matches!(vm.step(), StepExit::Halted));

    assert_eq!(vm.mem.read_u8(0x0500), 0x42);
    assert!(!vm.cpu.get_flag(FLAG_CF));
}

#[test]
fn aero_cpu_core_int13_extensions_check_reports_edd30() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // INT 13h AH=41h extensions check (requires BX=55AAh, DL>=80h).
    vm.cpu.gpr[gpr::RAX] = 0x4100;
    vm.cpu.gpr[gpr::RBX] = 0x55AA;
    vm.cpu.gpr[gpr::RDX] = 0x0080;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0x30);
    assert_eq!(vm.cpu.gpr[gpr::RBX] as u16, 0xAA55);
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0x0005);
}

#[test]
fn aero_cpu_core_int13_get_drive_parameters_returns_fixed_geometry() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // INT 13h AH=08h get drive parameters (minimal fixed geometry).
    vm.cpu.gpr[gpr::RAX] = 0x0800;
    vm.cpu.gpr[gpr::RDX] = 0x0080;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0xFFFF);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u16, 0x0F01);
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);
}

#[test]
fn aero_cpu_core_int13_ext_read_reads_lba_into_memory() {
    let bios = Bios::new(test_bios_config());

    let boot_sector = boot_sector_with(&[]);
    let mut disk_bytes = vec![0u8; 512 * 4];
    disk_bytes[..512].copy_from_slice(&boot_sector);
    disk_bytes[512..1024].fill(0xAA);
    let disk = machine::InMemoryDisk::new(disk_bytes);

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // Disk Address Packet at 0000:0500.
    let dap = 0x0500u64;
    MemoryAccess::write_u8(&mut vm.mem, dap + 0, 0x10);
    MemoryAccess::write_u8(&mut vm.mem, dap + 1, 0x00);
    MemoryAccess::write_u16(&mut vm.mem, dap + 2, 1); // count
    MemoryAccess::write_u16(&mut vm.mem, dap + 4, 0x1000); // offset
    MemoryAccess::write_u16(&mut vm.mem, dap + 6, 0x0000); // segment
    MemoryAccess::write_u64(&mut vm.mem, dap + 8, 1); // LBA

    vm.cpu.gpr[gpr::RAX] = 0x4200; // AH=42h
    vm.cpu.gpr[gpr::RDX] = 0x0080; // DL=0x80
    vm.cpu.gpr[gpr::RSI] = 0x0500;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);

    let mut buf = vec![0u8; 512];
    vm.mem.read_physical(0x1000, &mut buf);
    assert_eq!(buf, vec![0xAA; 512]);
}

#[test]
fn aero_cpu_core_int13_ext_get_drive_params_reports_sector_count() {
    let bios = Bios::new(test_bios_config());

    let boot_sector = boot_sector_with(&[]);
    let mut disk_bytes = vec![0u8; 512 * 8];
    disk_bytes[..512].copy_from_slice(&boot_sector);
    let sectors = (disk_bytes.len() / 512) as u64;
    let disk = machine::InMemoryDisk::new(disk_bytes);

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // Drive parameter table at 0000:0600, with a caller-supplied buffer size.
    MemoryAccess::write_u16(&mut vm.mem, 0x0600, 0x1E);

    vm.cpu.gpr[gpr::RAX] = 0x4800; // AH=48h
    vm.cpu.gpr[gpr::RDX] = 0x0080; // DL=0x80
    vm.cpu.gpr[gpr::RSI] = 0x0600;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);
    assert_eq!(vm.mem.read_u64(0x0600 + 16), sectors);
    assert_eq!(vm.mem.read_u16(0x0600 + 24), 512);
}

#[test]
fn aero_cpu_core_int13_chs_read_rejects_buffers_crossing_64k_boundary() {
    let bios = Bios::new(test_bios_config());

    // Three sectors so CHS read of sectors 2+3 is in-range.
    let mut disk_bytes = vec![0u8; 3 * 512];
    disk_bytes[510] = 0x55;
    disk_bytes[511] = 0xAA;
    let disk = machine::InMemoryDisk::new(disk_bytes);

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // CHS read 2 sectors from cylinder 0, head 0, sector 2 into 0x0000:0xFF00.
    // This crosses a 64KiB physical boundary and should return AH=09h.
    vm.cpu.gpr[gpr::RAX] = 0x0202;
    vm.cpu.gpr[gpr::RCX] = 0x0002; // CH=0, CL=2
    vm.cpu.gpr[gpr::RDX] = 0x0080; // DH=0, DL=0x80
    set_real_mode_seg(&mut vm.cpu.segments.es, 0x0000);
    vm.cpu.gpr[gpr::RBX] = 0xFF00;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x0900);
}

#[test]
fn aero_cpu_core_int13_out_of_range_read_sets_status_and_int13_status_reports_it() {
    let bios = Bios::new(test_bios_config());
    // Only one sector so a CHS read of sector 2 (LBA 1) is out-of-range.
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 13h (read); INT 13h (get status); HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x13, 0xCD, 0x13, 0xF4]);

    // CHS read 1 sector from cylinder 0, head 0, sector 2 into 0x0000:0x0500.
    vm.cpu.gpr[gpr::RAX] = 0x0201;
    vm.cpu.gpr[gpr::RCX] = 0x0002; // CH=0, CL=2
    vm.cpu.gpr[gpr::RDX] = 0x0080; // DH=0, DL=0x80
    set_real_mode_seg(&mut vm.cpu.segments.es, 0x0000);
    vm.cpu.gpr[gpr::RBX] = 0x0500;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x0400);

    // INT 13h AH=01h get status of last disk operation.
    vm.cpu.gpr[gpr::RAX] = 0x0100;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x13)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x0400);
}

#[test]
fn aero_cpu_core_int15_a20_toggle_is_observable_in_memory_bus() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    assert!(vm.mem.a20_enabled(), "POST should enable A20");

    // Program: INT 15h; INT 15h; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x15, 0xCD, 0x15, 0xF4]);

    // Disable A20: AX=2400h.
    vm.cpu.gpr[gpr::RAX] = 0x2400;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x15)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(!vm.mem.a20_enabled(), "INT 15h AX=2400h should disable A20");
    assert_eq!((vm.cpu.gpr[gpr::RAX] >> 8) as u8, 0); // AH=0 on success
    assert!(!vm.cpu.get_flag(FLAG_CF));

    // Query A20: AX=2402h (returns AL=0 disabled / AL=1 enabled).
    vm.cpu.gpr[gpr::RAX] = 0x2402;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x15)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u8, 0);
    assert!(!vm.cpu.get_flag(FLAG_CF));

    assert!(matches!(vm.step(), StepExit::Halted));
}

#[test]
fn aero_cpu_core_int15_e820_returns_first_entry() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 15h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x15, 0xF4]);

    // E820 query for first entry into ES:DI=0x0000:0x0600.
    vm.cpu.gpr[gpr::RAX] = 0xE820;
    vm.cpu.gpr[gpr::RDX] = 0x534D_4150; // 'SMAP'
    vm.cpu.gpr[gpr::RBX] = 0;
    vm.cpu.gpr[gpr::RCX] = 24;
    set_real_mode_seg(&mut vm.cpu.segments.es, 0x0000);
    vm.cpu.gpr[gpr::RDI] = 0x0600;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x15)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u32, 0x534D_4150);
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u32, 24);
    assert_eq!(vm.cpu.gpr[gpr::RBX] as u32, 1);

    let base = vm.mem.read_u64(0x0600);
    let length = vm.mem.read_u64(0x0608);
    let kind = vm.mem.read_u32(0x0610);
    let attrs = vm.mem.read_u32(0x0614);
    assert_eq!(base, 0);
    assert_ne!(length, 0);
    assert_eq!(kind, 1);
    assert_eq!(attrs, 1);
}

#[test]
fn aero_cpu_core_int15_a20_support_returns_int15_bitmask() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 15h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x15, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x2403; // Get A20 support

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x15)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);
    assert_eq!(vm.cpu.gpr[gpr::RBX] as u16, 0x0007);
}

#[test]
fn aero_cpu_core_int15_ah88_returns_extended_memory_kb_above_1m() {
    let mem_bytes = 32 * 1024 * 1024;
    let cfg = BiosConfig {
        enable_acpi: false,
        memory_size_bytes: mem_bytes as u64,
        ..test_bios_config()
    };
    let bios = Bios::new(cfg);
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(mem_bytes, bios, disk);
    vm.reset();

    // Program: INT 15h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x15, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x8800;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x15)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x7C00);
}

#[test]
fn aero_cpu_core_int15_e801_returns_kb_and_64k_blocks_from_e820_map() {
    let mem_bytes = 32 * 1024 * 1024;
    let cfg = BiosConfig {
        enable_acpi: false,
        memory_size_bytes: mem_bytes as u64,
        ..test_bios_config()
    };
    let bios = Bios::new(cfg);
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(mem_bytes, bios, disk);
    vm.reset();

    // Program: INT 15h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x15, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0xE801;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x15)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x3C00);
    assert_eq!(vm.cpu.gpr[gpr::RBX] as u16, 0x0100);
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0x3C00);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u16, 0x0100);
}

#[test]
fn aero_cpu_core_int16_read_key_returns_scancode_and_ascii() {
    let mut bios = Bios::new(test_bios_config());
    bios.push_key(0x2C5A); // scan=0x2C, ascii='Z'
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 16h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x16, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0000;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x16)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x2C5A);
    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert!(!vm.cpu.get_flag(FLAG_ZF));
}

#[test]
fn aero_cpu_core_int16_check_key_does_not_consume_and_sets_zf_correctly() {
    let mut bios = Bios::new(test_bios_config());
    bios.push_key(0x2C5A); // scan=0x2C, ascii='Z'
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 16h; INT 16h; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x16, 0xCD, 0x16, 0xF4]);

    // AH=01h Check for keystroke (does not consume).
    vm.cpu.gpr[gpr::RAX] = 0x0100;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x16)));
    assert!(matches!(vm.step(), StepExit::Branch));

    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x2C5A);
    assert!(!vm.cpu.get_flag(FLAG_ZF));
    assert!(!vm.cpu.get_flag(FLAG_CF));

    // AH=00h Read keystroke (consumes).
    vm.cpu.gpr[gpr::RAX] = 0x0000;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x16)));
    assert!(matches!(vm.step(), StepExit::Branch));

    assert_eq!(vm.cpu.gpr[gpr::RAX] as u16, 0x2C5A);
    assert!(!vm.cpu.get_flag(FLAG_ZF));
    assert!(!vm.cpu.get_flag(FLAG_CF));

    assert!(matches!(vm.step(), StepExit::Halted));
}

#[test]
fn aero_cpu_core_int16_check_key_sets_zf_when_queue_empty() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 16h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x16, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0100;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x16)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(vm.cpu.get_flag(FLAG_ZF));
    assert!(!vm.cpu.get_flag(FLAG_CF));
}

#[test]
fn aero_cpu_core_int1a_get_system_time_returns_bda_ticks() {
    let bios = Bios::new_with_rtc(
        test_bios_config(),
        CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)),
    );
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // BDA tick count is initialized during POST.
    assert_eq!(vm.mem.read_u32(BDA_TICK_COUNT_ADDR), 0);
    assert_eq!(vm.mem.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0000; // AH=00h Get System Time

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    let ticks =
        (((vm.cpu.gpr[gpr::RCX] & 0xFFFF) as u32) << 16) | ((vm.cpu.gpr[gpr::RDX] & 0xFFFF) as u32);
    assert_eq!(ticks, vm.mem.read_u32(BDA_TICK_COUNT_ADDR));
    assert_eq!((vm.cpu.gpr[gpr::RAX] & 0xFF) as u8, 0);
    assert!(!vm.cpu.get_flag(FLAG_CF));
}

#[test]
fn aero_cpu_core_int1a_midnight_flag_is_reported_and_cleared() {
    let bios = Bios::new_with_rtc(
        test_bios_config(),
        CmosRtc::new(DateTime::new(2026, 1, 1, 23, 59, 59)),
    );
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Cross midnight by advancing 2 seconds.
    vm.bios.advance_time(&mut vm.mem, Duration::from_secs(2));
    assert_eq!(vm.mem.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 1);

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0000; // AH=00h Get System Time

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    let ticks =
        (((vm.cpu.gpr[gpr::RCX] & 0xFFFF) as u32) << 16) | ((vm.cpu.gpr[gpr::RDX] & 0xFFFF) as u32);
    assert_eq!(ticks, vm.mem.read_u32(BDA_TICK_COUNT_ADDR));
    assert_eq!(ticks, (u64::from(TICKS_PER_DAY) * 1 / 86_400) as u32);
    assert_eq!((vm.cpu.gpr[gpr::RAX] & 0xFF) as u8, 1);
    assert_eq!(vm.mem.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);
    assert!(!vm.cpu.get_flag(FLAG_CF));
}

#[test]
fn aero_cpu_core_int1a_read_rtc_time_returns_bcd_time_fields() {
    let bios = Bios::new_with_rtc(
        test_bios_config(),
        CmosRtc::new(DateTime::new(2026, 12, 31, 12, 34, 56)),
    );
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0200; // AH=02h Read RTC time

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0x1234);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u16, 0x5600);
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);
}

#[test]
fn aero_cpu_core_int1a_set_rtc_time_updates_rtc_and_bda_tick_count() {
    let bios = Bios::new_with_rtc(
        test_bios_config(),
        CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)),
    );
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 1Ah; INT 1Ah; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x1A, 0xCD, 0x1A, 0xF4]);

    // AH=03h Set RTC time.
    vm.cpu.gpr[gpr::RAX] = 0x0300;
    vm.cpu.gpr[gpr::RCX] = 0x0102; // 01:02
    vm.cpu.gpr[gpr::RDX] = 0x0300; // 03 + DST=0

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);

    let seconds = 1u64 * 3600 + 2u64 * 60 + 3u64;
    let expected_ticks = (u64::from(TICKS_PER_DAY) * seconds / 86_400) as u32;
    assert_eq!(vm.mem.read_u32(BDA_TICK_COUNT_ADDR), expected_ticks);

    // AH=02h Read RTC time.
    vm.cpu.gpr[gpr::RAX] = 0x0200;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0x0102);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u16, 0x0300);
    assert_eq!(vm.bios.rtc.datetime().hour, 1);
    assert_eq!(vm.bios.rtc.datetime().minute, 2);
    assert_eq!(vm.bios.rtc.datetime().second, 3);
}

#[test]
fn aero_cpu_core_int1a_set_rtc_time_rejects_invalid_bcd_fields() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);

    // Hour 0x25 is invalid in 24-hour BCD mode.
    vm.cpu.gpr[gpr::RAX] = 0x0300;
    vm.cpu.gpr[gpr::RCX] = 0x2500;
    vm.cpu.gpr[gpr::RDX] = 0x0000;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 1);
}

#[test]
fn aero_cpu_core_int1a_read_rtc_date_returns_bcd_date_fields() {
    let bios = Bios::new_with_rtc(
        test_bios_config(),
        CmosRtc::new(DateTime::new(2026, 12, 31, 0, 0, 0)),
    );
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0400; // AH=04h Read RTC date

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0x2026);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u16, 0x1231);
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);
}

#[test]
fn aero_cpu_core_int1a_set_rtc_date_updates_rtc() {
    let bios = Bios::new_with_rtc(
        test_bios_config(),
        CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)),
    );
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 1Ah; INT 1Ah; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x1A, 0xCD, 0x1A, 0xF4]);

    // AH=05h Set RTC date to 2027-02-03.
    vm.cpu.gpr[gpr::RAX] = 0x0500;
    vm.cpu.gpr[gpr::RCX] = 0x2027;
    vm.cpu.gpr[gpr::RDX] = 0x0203;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);

    // AH=04h Read RTC date.
    vm.cpu.gpr[gpr::RAX] = 0x0400;
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(vm.cpu.gpr[gpr::RCX] as u16, 0x2027);
    assert_eq!(vm.cpu.gpr[gpr::RDX] as u16, 0x0203);
    assert_eq!(vm.bios.rtc.datetime().year, 2027);
    assert_eq!(vm.bios.rtc.datetime().month, 2);
    assert_eq!(vm.bios.rtc.datetime().day, 3);
}

#[test]
fn aero_cpu_core_int1a_set_rtc_date_rejects_invalid_bcd_fields() {
    let bios = Bios::new(test_bios_config());
    let disk = machine::InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);

    // Month 0x13 is invalid in BCD mode.
    vm.cpu.gpr[gpr::RAX] = 0x0500;
    vm.cpu.gpr[gpr::RCX] = 0x2027;
    vm.cpu.gpr[gpr::RDX] = 0x1301;

    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::BiosInterrupt(0x1A)));
    assert!(matches!(vm.step(), StepExit::Branch));
    assert!(matches!(vm.step(), StepExit::Halted));

    assert!(vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 1);
}

#[test]
fn aero_cpu_core_runs_int_sanity_boot_sector_fixture() {
    // The fixture is built from `test_images/boot_sectors/int_sanity.asm` and exercises a small
    // subset of BIOS interrupts (INT 10/13/15/16) while leaving observable state in low RAM.
    let boot_sector: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test_images/boot_sectors/int_sanity.bin"
    ));
    assert_eq!(boot_sector.len(), 512);

    // Two sectors: boot sector + one data sector read via INT 13h CHS (0,0,2) -> LBA 1.
    let mut disk_bytes = vec![0u8; 2 * 512];
    disk_bytes[..512].copy_from_slice(boot_sector);
    disk_bytes[512] = 0x42;
    let disk = machine::InMemoryDisk::new(disk_bytes);

    let mut bios = Bios::new(test_bios_config());
    bios.push_key(0x2C5A); // scan=0x2C, ascii='Z'

    let mut vm = CoreVm::new(TEST_MEM_SIZE, bios, disk);
    vm.reset();

    // Run until the boot sector's terminal HLT.
    let mut steps = 0u64;
    loop {
        steps += 1;
        if steps > 100_000 {
            panic!("int_sanity boot sector did not halt (steps={steps})");
        }
        match vm.step() {
            StepExit::Continue | StepExit::Branch | StepExit::BiosInterrupt(_) => continue,
            StepExit::Halted => break,
            StepExit::Assist(r) => panic!("unexpected assist while running boot sector: {r:?}"),
        }
    }

    // INT 10h teletype should have emitted 'A'.
    assert_eq!(vm.bios.tty_output(), b"A");

    // INT 15h E820 should have written entry 0 to 0000:0600.
    assert_eq!(vm.mem.read_u64(0x0600), 0);
    assert_ne!(vm.mem.read_u64(0x0608), 0);
    assert_eq!(vm.mem.read_u32(0x0610), 1);
    assert_eq!(vm.mem.read_u32(0x0614), 1);

    // INT 16h AH=00h stores AX at 0000:0510.
    assert_eq!(vm.mem.read_u16(0x0510), 0x2C5A);

    // INT 13h CHS read stores the first byte of the second sector at 0000:0520.
    assert_eq!(vm.mem.read_u8(0x0520), 0x42);

    // Signature written by the fixture for host-side validation.
    assert_eq!(vm.mem.read_u8(0x0530), b'O');
    assert_eq!(vm.mem.read_u8(0x0531), b'K');
}

#[test]
fn aero_cpu_core_real_mode_payload_enables_a20_and_stops_aliasing() {
    // Port of `crates/vm/tests/boot_payloads.rs` to the real CPU engine.
    //
    // This payload:
    // - disables A20 via INT 15h AX=2400 and demonstrates wraparound at 1MiB
    // - enables A20 via INT 15h AX=2401 and demonstrates distinct addressing
    //
    // The payload stores:
    //   [0x0002..0x0003] = value observed at 0x0000 after the "disabled A20" write
    //   [0x0004..0x0005] = value observed at 0x0000 after enabling A20 and rewriting
    let payload: &[u8] = &[
        0xB8, 0x00, 0x24, // mov ax, 0x2400 (disable A20)
        0xCD, 0x15, // int 15h
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x00, 0x11, 0x11, // mov word [0], 0x1111
        0xB8, 0xFF, 0xFF, // mov ax, 0xFFFF
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x10, 0x00, 0x22, 0x22, // mov word [0x0010], 0x2222 (phys 1MiB)
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xA0, 0x00, 0x00, // mov al, [0x0000]
        0xA2, 0x02, 0x00, // mov [0x0002], al
        0xA0, 0x01, 0x00, // mov al, [0x0001]
        0xA2, 0x03, 0x00, // mov [0x0003], al
        0xB8, 0x01, 0x24, // mov ax, 0x2401 (enable A20)
        0xCD, 0x15, // int 15h
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x00, 0x33, 0x33, // mov word [0], 0x3333
        0xB8, 0xFF, 0xFF, // mov ax, 0xFFFF
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x10, 0x00, 0x44, 0x44, // mov word [0x0010], 0x4444 (phys 1MiB)
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xA0, 0x00, 0x00, // mov al, [0x0000]
        0xA2, 0x04, 0x00, // mov [0x0004], al
        0xA0, 0x01, 0x00, // mov al, [0x0001]
        0xA2, 0x05, 0x00, // mov [0x0005], al
        0xF4, // hlt
    ];

    let mut sector = [0u8; 512];
    sector[..payload.len()].copy_from_slice(payload);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    let disk = machine::InMemoryDisk::from_boot_sector(sector);

    let cfg = BiosConfig {
        enable_acpi: false,
        memory_size_bytes: 2 * 1024 * 1024,
        ..test_bios_config()
    };
    let bios = Bios::new(cfg);

    let mut vm = CoreVm::new(2 * 1024 * 1024, bios, disk);
    vm.reset();

    let mut steps = 0u64;
    loop {
        steps += 1;
        if steps > 10_000 {
            panic!("A20 payload did not halt (steps={steps})");
        }
        match vm.step() {
            StepExit::Continue | StepExit::Branch | StepExit::BiosInterrupt(_) => continue,
            StepExit::Halted => break,
            StepExit::Assist(r) => panic!("unexpected assist while running A20 payload: {r:?}"),
        }
    }

    assert!(vm.cpu.halted, "payload should terminate with HLT");
    assert_eq!(
        vm.mem.read_u16(0x0002),
        0x2222,
        "expected aliasing while A20 disabled"
    );
    assert_eq!(
        vm.mem.read_u16(0x0004),
        0x3333,
        "expected 0x0000 to remain 0x3333 after re-enabling A20"
    );
    assert_eq!(
        vm.mem.read_u16(0x1_00000),
        0x4444,
        "expected 0x1_00000 to contain 0x4444 after enabling A20"
    );
}

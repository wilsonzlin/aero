use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit, StepExit};
use aero_cpu_core::mem::CpuBus as CoreCpuBus;
use aero_cpu_core::state::{gpr, CpuMode as CoreCpuMode, CpuState as CoreCpuState, Segment as CoreSegment, FLAG_CF};
use firmware::bda::BiosDataArea;
use firmware::bios::{build_bios_rom, Bios, BiosConfig, BiosBus, BIOS_BASE, BIOS_SEGMENT};
use machine::{
    A20Gate, BlockDevice, CpuState as MachineCpuState, FirmwareMemory, MemoryAccess, PhysicalMemory,
    Segment as MachineSegment,
};

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
    // Hardware reset: CS.selector=0xF000, CS.base=0xFFFF_0000, IP=0xFFF0.
    state.segments.cs.selector = BIOS_SEGMENT;
    state.segments.cs.base = 0xFFFF_0000;
    state.segments.cs.limit = 0xFFFF;
    state.segments.cs.access = 0;
    state.set_rip(0xFFF0);
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

    fn translate_bios_alias(addr: u64) -> u64 {
        if (0xFFFF_0000..=0xFFFF_FFFF).contains(&addr) {
            BIOS_BASE + (addr - 0xFFFF_0000)
        } else {
            addr
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
        self.inner.read_u8(Self::translate_bios_alias(addr))
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        self.inner.write_u8(Self::translate_bios_alias(addr), val)
    }

    fn fetch_code(&self, addr: u64, len: usize) -> &[u8] {
        self.inner.fetch_code(Self::translate_bios_alias(addr), len)
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

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), aero_cpu_core::Exception> {
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
        self.mem.map_rom(BIOS_BASE, &rom);

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

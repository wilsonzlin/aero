use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::CpuMode;
use aero_cpu_core::Exception;
use aero_x86::Register;

#[derive(Debug)]
struct FetchCountBus {
    inner: FlatTestBus,
    fetches: u64,
}

impl FetchCountBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            fetches: 0,
        }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        self.inner.load(addr, data);
    }
}

impl CpuBus for FetchCountBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.inner.write_u128(vaddr, val)
    }

    fn supports_bulk_copy(&self) -> bool {
        self.inner.supports_bulk_copy()
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        self.inner.bulk_copy(dst, src, len)
    }

    fn supports_bulk_set(&self) -> bool {
        self.inner.supports_bulk_set()
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        self.inner.bulk_set(dst, pattern, repeat)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.fetches += 1;
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[test]
fn tier0_interpreter_assist_fetches_once() {
    const BUS_SIZE: usize = 0x1000;
    const CODE_BASE: u64 = 0x100;

    let mut bus = FetchCountBus::new(BUS_SIZE);
    // CPUID (0F A2) triggers a Tier-0 assist.
    bus.load(CODE_BASE, &[0x0F, 0xA2]);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Long, bus);
    // Ensure CS base is 0 so RIP is a flat linear address.
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(CODE_BASE);
    cpu.cpu.state.write_reg(Register::EAX, 0);
    cpu.cpu.state.write_reg(Register::ECX, 0);

    // Run exactly one instruction. Historically Tier-0 would fetch+decode once in `step` and then
    // fetch+decode again when resolving the assist, so this should observe only one fetch call.
    let mut interp = Tier0Interpreter::new(1);
    let exit = interp.exec_block(&mut cpu);
    assert_eq!(exit.instructions_retired, 1);

    assert_eq!(
        cpu.bus.fetches, 1,
        "expected single instruction fetch for assist"
    );
    // Sanity-check: CPUID leaf 0 should match the deterministic policy in `cpuid.rs`.
    assert_eq!(cpu.cpu.state.read_reg(Register::EAX) as u32, 0x1F);
}

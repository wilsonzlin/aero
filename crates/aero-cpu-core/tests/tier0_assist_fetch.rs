use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode};
use aero_cpu_core::Exception;
use aero_x86::Register;

#[derive(Debug, Clone)]
struct CountingBus {
    inner: FlatTestBus,
    fetch_calls: u64,
}

impl CountingBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            fetch_calls: 0,
        }
    }

    fn load(&mut self, addr: u64, bytes: &[u8]) {
        self.inner.load(addr, bytes);
    }
}

impl CpuBus for CountingBus {
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

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.inner.read_bytes(vaddr, dst)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.inner.write_bytes(vaddr, src)
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.inner.preflight_write_bytes(vaddr, len)
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
        self.fetch_calls += 1;
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
fn tier0_exec_glue_does_not_double_fetch_on_assist() {
    let mut bus = CountingBus::new(0x1000);
    bus.load(0, &[0x0F, 0xA2, 0xF4]); // CPUID; HLT

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x800);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.write_reg(Register::EAX, 0);
    cpu.cpu.state.write_reg(Register::ECX, 0);
    cpu.cpu.state.set_rip(0);

    let mut interp = Tier0Interpreter::new(1024);

    // Tier0Interpreter treats assists as a block boundary, so execute CPUID and HLT in two blocks.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), 2);
    assert_eq!(
        cpu.bus.fetch_calls, 1,
        "CPUID assist should not trigger a second instruction fetch"
    );

    interp.exec_block(&mut cpu);
    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.rip(), 3);
    assert_eq!(
        cpu.bus.fetch_calls, 2,
        "each retired instruction should require exactly one fetch"
    );
}

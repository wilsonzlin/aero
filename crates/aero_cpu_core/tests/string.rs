use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{CpuMode, CpuState, FLAG_CF, FLAG_DF, FLAG_SF, FLAG_ZF};
use aero_cpu_core::Exception;
use aero_x86::Register;

trait LoadableBus: CpuBus {
    fn load(&mut self, addr: u64, data: &[u8]);
}

fn exec_one<B: LoadableBus>(state: &mut CpuState, bus: &mut B, code_addr: u64, bytes: &[u8]) {
    // The Tier-0 interpreter fetches from CS:IP.
    state.segments.cs.base = 0;
    state.set_rip(code_addr);
    bus.load(code_addr, bytes);
    let exit = step(state, bus).expect("step");
    assert!(
        matches!(
            exit,
            StepExit::Continue | StepExit::ContinueInhibitInterrupts | StepExit::Branch
        ),
        "unexpected step exit: {exit:?}"
    );
}

// ---------------------------------------------------------------------------
// Minimal test buses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        let start = addr as usize;
        let end = start + data.len();
        self.mem[start..end].copy_from_slice(data);
    }

    fn range(&self, addr: u64, len: usize) -> Result<std::ops::Range<usize>, Exception> {
        if len == 0 {
            let start = addr as usize;
            return Ok(start..start);
        }
        let start: usize = addr.try_into().map_err(|_| Exception::MemoryFault)?;
        let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
        if end > self.mem.len() {
            return Err(Exception::MemoryFault);
        }
        Ok(start..end)
    }

    fn read_le<const N: usize>(&self, addr: u64) -> Result<u64, Exception> {
        let range = self.range(addr, N)?;
        let mut out = 0u64;
        for (i, b) in self.mem[range].iter().enumerate() {
            out |= (*b as u64) << (i * 8);
        }
        Ok(out)
    }

    fn write_le<const N: usize>(&mut self, addr: u64, value: u64) -> Result<(), Exception> {
        let range = self.range(addr, N)?;
        for i in 0..N {
            self.mem[range.start + i] = (value >> (i * 8)) as u8;
        }
        Ok(())
    }
}

impl CpuBus for TestBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        Ok(*self.mem.get(vaddr as usize).ok_or(Exception::MemoryFault)?)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        Ok(self.read_le::<2>(vaddr)? as u16)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        Ok(self.read_le::<4>(vaddr)? as u32)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.read_le::<8>(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let lo = self.read_le::<8>(vaddr)? as u128;
        let hi = self.read_le::<8>(vaddr + 8)? as u128;
        Ok(lo | (hi << 64))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        *self
            .mem
            .get_mut(vaddr as usize)
            .ok_or(Exception::MemoryFault)? = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_le::<2>(vaddr, val as u64)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_le::<4>(vaddr, val as u64)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_le::<8>(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.write_le::<8>(vaddr, val as u64)?;
        self.write_le::<8>(vaddr + 8, (val >> 64) as u64)?;
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = self.read_u8(vaddr + i as u64)?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}

impl LoadableBus for TestBus {
    fn load(&mut self, addr: u64, data: &[u8]) {
        TestBus::load(self, addr, data);
    }
}

#[derive(Debug, Clone)]
struct CountingBus {
    inner: TestBus,
    bulk_copy_calls: usize,
    bulk_set_calls: usize,
}

impl CountingBus {
    fn new(size: usize) -> Self {
        Self {
            inner: TestBus::new(size),
            bulk_copy_calls: 0,
            bulk_set_calls: 0,
        }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        self.inner.load(addr, data);
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

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.inner.io_write(port, size, val)
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        self.bulk_copy_calls += 1;
        if len == 0 {
            return Ok(true);
        }
        let src_range = self.inner.range(src, len)?;
        let dst_range = self.inner.range(dst, len)?;
        if src_range.start == dst_range.start {
            return Ok(true);
        }
        // memmove semantics.
        self.inner
            .mem
            .copy_within(src_range.clone(), dst_range.start);
        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        self.bulk_set_calls += 1;
        if repeat == 0 {
            return Ok(true);
        }
        if pattern.is_empty() {
            return Err(Exception::MemoryFault);
        }
        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;
        let dst_range = self.inner.range(dst, total)?;
        let dst_slice = &mut self.inner.mem[dst_range];

        for chunk in dst_slice.chunks_exact_mut(pattern.len()) {
            chunk.copy_from_slice(pattern);
        }
        Ok(true)
    }
}

impl LoadableBus for CountingBus {
    fn load(&mut self, addr: u64, data: &[u8]) {
        CountingBus::load(self, addr, data);
    }
}

#[derive(Debug, Clone)]
struct OpLimitBus {
    inner: TestBus,
    ops: usize,
    max_ops: usize,
}

impl OpLimitBus {
    fn new(size: usize, max_ops: usize) -> Self {
        Self {
            inner: TestBus::new(size),
            ops: 0,
            max_ops,
        }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        self.inner.load(addr, data);
    }

    fn bump(&mut self) {
        self.ops += 1;
        assert!(
            self.ops <= self.max_ops,
            "bus op limit exceeded: ops={} max_ops={}",
            self.ops,
            self.max_ops
        );
    }
}

impl CpuBus for OpLimitBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.bump();
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.bump();
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.bump();
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.bump();
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        self.bump();
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.bump();
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.bump();
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.bump();
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.bump();
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.bump();
        self.inner.write_u128(vaddr, val)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.bump();
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.bump();
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.bump();
        self.inner.io_write(port, size, val)
    }

    fn supports_bulk_copy(&self) -> bool {
        false
    }

    fn supports_bulk_set(&self) -> bool {
        false
    }
}

impl LoadableBus for OpLimitBus {
    fn load(&mut self, addr: u64, data: &[u8]) {
        OpLimitBus::load(self, addr, data);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn movsb_df0_and_df1() {
    // DF=0 increments.
    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x20);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x10, 0xAA).unwrap();
    exec_one(&mut state, &mut bus, 0, &[0xA4]); // MOVSB

    assert_eq!(bus.read_u8(0x2000 + 0x20).unwrap(), 0xAA);
    assert_eq!(state.read_reg(Register::SI), 0x11);
    assert_eq!(state.read_reg(Register::DI), 0x21);

    // DF=1 decrements.
    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_flag(FLAG_DF, true);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x20);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x10, 0xBB).unwrap();
    exec_one(&mut state, &mut bus, 0, &[0xA4]); // MOVSB

    assert_eq!(bus.read_u8(0x2000 + 0x20).unwrap(), 0xBB);
    assert_eq!(state.read_reg(Register::SI), 0x0F);
    assert_eq!(state.read_reg(Register::DI), 0x1F);
}

#[test]
fn stosw_df0_and_df1() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.es.base = 0x2000;
    state.write_reg(Register::EDI, 0x100);
    state.write_reg(Register::AX, 0x1234);

    let mut bus = TestBus::new(0x10_000);
    exec_one(&mut state, &mut bus, 0, &[0x66, 0xAB]); // STOSW
    assert_eq!(bus.read_u16(0x2000 + 0x100).unwrap(), 0x1234);
    assert_eq!(state.read_reg(Register::EDI), 0x102);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_flag(FLAG_DF, true);
    state.segments.es.base = 0x2000;
    state.write_reg(Register::EDI, 0x100);
    state.write_reg(Register::AX, 0x5678);

    let mut bus = TestBus::new(0x10_000);
    exec_one(&mut state, &mut bus, 0, &[0x66, 0xAB]); // STOSW
    assert_eq!(bus.read_u16(0x2000 + 0x100).unwrap(), 0x5678);
    assert_eq!(state.read_reg(Register::EDI), 0x0FE);
}

#[test]
fn lodsd_df0_and_df1() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.segments.ds.base = 0x1000; // ignored in long mode (DS base forced to 0)
    state.msr.fs_base = 0x3000;
    state.write_reg(Register::RSI, 0x20);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u32(0x3000 + 0x20, 0xDEAD_BEEF).unwrap();

    // FS override, DF=0 increments.
    exec_one(&mut state, &mut bus, 0, &[0x64, 0xAD]); // LODSD
    assert_eq!(state.read_reg(Register::EAX), 0xDEAD_BEEF);
    assert_eq!(state.read_reg(Register::RSI), 0x24);

    // DF=1 decrements.
    let mut state = CpuState::new(CpuMode::Bit64);
    state.set_flag(FLAG_DF, true);
    state.msr.fs_base = 0x3000;
    state.write_reg(Register::RSI, 0x20);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u32(0x3000 + 0x20, 0xAABB_CCDD).unwrap();
    exec_one(&mut state, &mut bus, 0, &[0x64, 0xAD]); // LODSD
    assert_eq!(state.read_reg(Register::EAX), 0xAABB_CCDD);
    assert_eq!(state.read_reg(Register::RSI), 0x1C);
}

#[test]
fn rep_count_zero_is_noop() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x20);
    state.write_reg(Register::CX, 0);
    state.set_flag(FLAG_DF, true);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x10, 0x11).unwrap();
    bus.write_u8(0x2000 + 0x20, 0x22).unwrap();
    let flags_before = state.rflags();

    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xA4]); // REP MOVSB

    assert_eq!(bus.read_u8(0x2000 + 0x20).unwrap(), 0x22);
    assert_eq!(state.read_reg(Register::SI), 0x10);
    assert_eq!(state.read_reg(Register::DI), 0x20);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.rflags(), flags_before);
}

#[test]
fn repe_cmpsb_stops_on_mismatch() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);
    state.write_reg(Register::ECX, 5);

    let mut bus = TestBus::new(0x10_000);
    // First 3 bytes match, 4th differs.
    for i in 0..5u64 {
        bus.write_u8(0x1000 + 0x10 + i, if i == 3 { 0x99 } else { i as u8 })
            .unwrap();
        bus.write_u8(0x2000 + 0x20 + i, i as u8).unwrap();
    }

    // REPE CMPSB
    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xA6]);
    // Mismatch at i=3 means 4 iterations executed.
    assert_eq!(state.read_reg(Register::ESI), 0x14);
    assert_eq!(state.read_reg(Register::EDI), 0x24);
    assert_eq!(state.read_reg(Register::ECX), 1);
    assert!(!state.get_flag(FLAG_ZF));
}

#[test]
fn cmpsb_df0_increments_si_di_and_sets_flags() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x12);
    state.write_reg(Register::EDI, 0x22);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x12, 0x5A).unwrap();
    bus.write_u8(0x2000 + 0x22, 0x5A).unwrap();

    exec_one(&mut state, &mut bus, 0, &[0xA6]); // CMPSB
    assert_eq!(state.read_reg(Register::ESI), 0x13);
    assert_eq!(state.read_reg(Register::EDI), 0x23);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn cmpsb_df1_decrements_si_di_and_sets_flags() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_flag(FLAG_DF, true);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x12);
    state.write_reg(Register::EDI, 0x22);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x12, 0x5A).unwrap();
    bus.write_u8(0x2000 + 0x22, 0x5A).unwrap();

    exec_one(&mut state, &mut bus, 0, &[0xA6]); // CMPSB
    assert_eq!(state.read_reg(Register::ESI), 0x11);
    assert_eq!(state.read_reg(Register::EDI), 0x21);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn repne_scasb_stops_on_match() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.es.base = 0x3000;
    state.write_reg(Register::EDI, 0x10);
    state.write_reg(Register::ECX, 6);
    state.write_reg(Register::AL, 0x7F);

    let mut bus = TestBus::new(0x10_000);
    let hay = [0x00, 0x01, 0x02, 0x7F, 0x03, 0x04];
    for (i, &b) in hay.iter().enumerate() {
        bus.write_u8(0x3000 + 0x10 + i as u64, b).unwrap();
    }

    exec_one(&mut state, &mut bus, 0, &[0xF2, 0xAE]); // REPNE SCASB
                                                      // Stops when ZF=1 at index 3, after 4 iterations executed.
    assert_eq!(state.read_reg(Register::EDI), 0x14);
    assert_eq!(state.read_reg(Register::ECX), 2);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn scasb_df0_increments_di_and_sets_flags() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.es.base = 0x3000;
    state.write_reg(Register::EDI, 0x10);
    state.write_reg(Register::AL, 0x42);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x3000 + 0x10, 0x42).unwrap();

    exec_one(&mut state, &mut bus, 0, &[0xAE]); // SCASB
    assert_eq!(state.read_reg(Register::EDI), 0x11);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn scasb_df1_decrements_di_and_sets_flags() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_flag(FLAG_DF, true);
    state.segments.es.base = 0x3000;
    state.write_reg(Register::EDI, 0x10);
    state.write_reg(Register::AL, 0x42);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x3000 + 0x10, 0x42).unwrap();

    exec_one(&mut state, &mut bus, 0, &[0xAE]); // SCASB
    assert_eq!(state.read_reg(Register::EDI), 0x0F);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn cmpsb_flag_order_is_src_minus_dest() {
    // Intel: CMPS sets flags as if computing SRC - DEST.
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x10, 0x01).unwrap(); // SRC
    bus.write_u8(0x2000 + 0x20, 0x02).unwrap(); // DEST

    exec_one(&mut state, &mut bus, 0, &[0xA6]); // CMPSB
    assert!(!state.get_flag(FLAG_ZF));
    assert!(state.get_flag(FLAG_CF));
    assert!(state.get_flag(FLAG_SF));
}

#[test]
fn addr_size_override_uses_esi_edi_ecx_in_long_mode() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.segments.ds.base = 0x1111_0000; // ignored in long mode (DS base forced to 0)
    state.segments.es.base = 0x2222_0000; // ignored

    // High bits set; address-size override must use the low 32 bits.
    state.write_reg(Register::RSI, 0xAAAA_BBBB_0000_0010);
    state.write_reg(Register::RDI, 0xCCCC_DDDD_0000_0020);
    state.write_reg(Register::RCX, 0x1_0000_0002); // ECX=2, RCX high bits non-zero.

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x10, 0x10).unwrap();
    bus.write_u8(0x11, 0x11).unwrap();

    exec_one(&mut state, &mut bus, 0, &[0x67, 0xF3, 0xA4]); // addr-size override + REP MOVSB

    assert_eq!(bus.read_u8(0x20).unwrap(), 0x10);
    assert_eq!(bus.read_u8(0x21).unwrap(), 0x11);

    // ESI/EDI were used and updated, zero-extending into RSI/RDI.
    assert_eq!(state.read_reg(Register::RSI), 0x0000_0000_0000_0012);
    assert_eq!(state.read_reg(Register::RDI), 0x0000_0000_0000_0022);

    // Count uses ECX, and writing ECX in long mode zero-extends RCX.
    assert_eq!(state.read_reg(Register::RCX), 0);
}

#[test]
fn segment_override_applies_to_source_only_for_movs() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0x1000;
    state.segments.fs.base = 0x3000;
    state.segments.es.base = 0x5000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);

    let mut bus = TestBus::new(0x10_000);
    bus.write_u8(0x1000 + 0x10, 0xAA).unwrap();
    bus.write_u8(0x3000 + 0x10, 0xBB).unwrap();

    // FS override MOVSB: should read from FS:ESI, write to ES:EDI.
    exec_one(&mut state, &mut bus, 0, &[0x64, 0xA4]);
    assert_eq!(bus.read_u8(0x5000 + 0x20).unwrap(), 0xBB);

    // Segment override must not change the destination segment (still ES).
    assert_ne!(bus.read_u8(0x1000 + 0x20).unwrap(), 0xBB);
}

#[test]
fn rep_movsb_addr16_wrap_skips_bulk_copy_and_uses_wrapping_offsets() {
    // In 16-bit address-size mode, SI/DI wrap at 0x10000. If the range wraps, the accessed
    // addresses are not contiguous in linear memory and we must not use bulk-copy fast paths.
    let count = 128u16;

    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.ds.base = 0;
    state.segments.es.base = 0x20000;
    state.write_reg(Register::SI, 0xFFC0);
    state.write_reg(Register::DI, 0x0100);
    state.write_reg(Register::CX, count as u64);

    let mut bus = CountingBus::new(0x30000);
    // Source wraps: [0xFFC0..0xFFFF] then [0x0000..0x003F]
    for i in 0..64u64 {
        bus.write_u8(0xFFC0 + i, 0xAA).unwrap();
        bus.write_u8(i, 0xBB).unwrap();
        // This is what an incorrect bulk-copy would read instead of wrapping to 0x0000.
        bus.write_u8(0x10000 + i, 0xCC).unwrap();
    }

    // Place the instruction away from the wrapped source range at 0x0000.
    exec_one(&mut state, &mut bus, 0x8000, &[0xF3, 0xA4]); // REP MOVSB

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.read_reg(Register::SI), 0x0040);
    assert_eq!(state.read_reg(Register::DI), 0x0180);

    let dst_base = 0x20000 + 0x0100;
    for i in 0..64u64 {
        assert_eq!(bus.read_u8(dst_base + i).unwrap(), 0xAA);
        assert_eq!(bus.read_u8(dst_base + 64 + i).unwrap(), 0xBB);
    }
}

#[test]
fn rep_stosb_addr16_wrap_skips_bulk_set_and_uses_wrapping_offsets() {
    // Same idea as the MOVSB test above, but for STOSB bulk-set behavior.
    let count = 128u16;

    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.es.base = 0;
    state.write_reg(Register::DI, 0xFFC0);
    state.write_reg(Register::CX, count as u64);
    state.write_reg(Register::AL, 0x5A);

    let mut bus = CountingBus::new(0x20000);
    for i in 0..64u64 {
        bus.write_u8(0x10000 + i, 0xCC).unwrap();
    }

    exec_one(&mut state, &mut bus, 0x8000, &[0xF3, 0xAA]); // REP STOSB

    assert_eq!(bus.bulk_set_calls, 0);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.read_reg(Register::DI), 0x0040);

    for i in 0..64u64 {
        assert_eq!(bus.read_u8(0xFFC0 + i).unwrap(), 0x5A);
        assert_eq!(bus.read_u8(i).unwrap(), 0x5A);
        // Incorrect bulk-set would have overwritten this region instead of wrapping to 0x0000.
        assert_eq!(bus.read_u8(0x10000 + i).unwrap(), 0xCC);
    }
}

#[test]
fn rep_movsb_uses_bulk_copy_when_safe() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, 0x100);
    state.write_reg(Register::EDI, 0x2000);
    state.write_reg(Register::ECX, 4096);

    let mut bus = CountingBus::new(0x4000);
    for i in 0..4096u64 {
        bus.write_u8(0x100 + i, (i ^ 0x5Au64) as u8).unwrap();
    }

    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xA4]); // REP MOVSB

    assert_eq!(bus.bulk_copy_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), 0x1100);
    assert_eq!(state.read_reg(Register::EDI), 0x3000);

    for i in 0..4096u64 {
        assert_eq!(bus.read_u8(0x2000 + i).unwrap(), (i ^ 0x5Au64) as u8);
    }
}

#[test]
fn rep_movsb_overlap_hazard_falls_back_to_element_wise() {
    // Hazard case: DF=0 copies low->high, and destination starts inside the source range at a
    // higher address. The result differs from memmove; we must not take the bulk-copy fast path.
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, 0x100);
    state.write_reg(Register::EDI, 0x101);
    state.write_reg(Register::ECX, 4096);

    let mut bus = CountingBus::new(0x4000);
    bus.write_u8(0x100, 0xAA).unwrap();
    for i in 1..=4097u64 {
        bus.write_u8(0x100 + i, i as u8).unwrap();
    }

    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xA4]); // REP MOVSB

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), 0x1100);
    assert_eq!(state.read_reg(Register::EDI), 0x1101);

    for i in 1..=4096u64 {
        assert_eq!(
            bus.read_u8(0x100 + i).unwrap(),
            0xAA,
            "overlap hazard should propagate first byte"
        );
    }
}

#[test]
fn rep_movsb_df1_overlap_safe_uses_bulk_copy() {
    // DF=1 copies high->low. When destination starts inside the source range at a higher
    // address, this direction is safe (equivalent to memmove).
    let count = 4096u32;
    let src_start = 0x100u64;
    let dst_start = 0x101u64;

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_flag(FLAG_DF, true);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, src_start + count as u64 - 1);
    state.write_reg(Register::EDI, dst_start + count as u64 - 1);
    state.write_reg(Register::ECX, count as u64);

    let mut bus = CountingBus::new(0x4000);
    for i in 0..count as u64 {
        bus.write_u8(src_start + i, (i ^ 0x5Au64) as u8).unwrap();
    }

    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xA4]); // REP MOVSB

    assert_eq!(bus.bulk_copy_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), src_start - 1);
    assert_eq!(state.read_reg(Register::EDI), dst_start - 1);

    for i in 0..count as u64 {
        assert_eq!(bus.read_u8(dst_start + i).unwrap(), (i ^ 0x5Au64) as u8);
    }
}

#[test]
fn rep_movsb_df1_overlap_hazard_falls_back_to_element_wise() {
    // DF=1 copies high->low. Hazard case: destination starts inside the source range at a lower
    // address. This causes propagation of the last source byte when the shift is -1.
    let count = 4096u32;
    let src_start = 0x101u64;
    let dst_start = 0x100u64;
    let expected = (count as u64 - 1) as u8;

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_flag(FLAG_DF, true);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, src_start + count as u64 - 1);
    state.write_reg(Register::EDI, dst_start + count as u64 - 1);
    state.write_reg(Register::ECX, count as u64);

    let mut bus = CountingBus::new(0x4000);
    for i in 0..count as u64 {
        bus.write_u8(src_start + i, i as u8).unwrap();
    }

    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xA4]); // REP MOVSB

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), src_start - 1);
    assert_eq!(state.read_reg(Register::EDI), dst_start - 1);

    for i in 0..count as u64 {
        assert_eq!(
            bus.read_u8(dst_start + i).unwrap(),
            expected,
            "DF=1 overlap hazard should propagate last byte"
        );
    }
}

#[test]
fn rep_stosd_uses_bulk_set() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.es.base = 0;
    state.write_reg(Register::EDI, 0x200);
    state.write_reg(Register::ECX, 1024); // 1024 * 4 = 4096 bytes
    state.write_reg(Register::EAX, 0xAABB_CCDD);

    let mut bus = CountingBus::new(0x2000);
    exec_one(&mut state, &mut bus, 0, &[0xF3, 0xAB]); // REP STOSD

    assert_eq!(bus.bulk_set_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::EDI), 0x1200);

    for i in 0..1024u64 {
        assert_eq!(bus.read_u32(0x200 + i * 4).unwrap(), 0xAABB_CCDD);
    }
}

#[test]
fn rep_count_uses_cx_in_16bit_addr_size_even_with_32bit_elements() {
    // In 16-bit mode, `66 A5` is MOVSD but the repeat counter is still CX unless 0x67 overrides
    // the address size.
    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x40);
    state.write_reg(Register::RCX, 0x0000_0000_0001_0002); // ECX=0x0001_0002, CX=2

    let mut bus = OpLimitBus::new(0x400, 200);
    bus.write_u32(0x10, 0x1122_3344).unwrap();
    bus.write_u32(0x14, 0x5566_7788).unwrap();

    exec_one(&mut state, &mut bus, 0, &[0xF3, 0x66, 0xA5]); // REP MOVSD

    assert_eq!(state.read_reg(Register::CX), 0);
    // Upper bits should remain unchanged because CX is the counter in this mode.
    assert_eq!(state.read_reg(Register::RCX), 0x0000_0000_0001_0000);

    assert_eq!(bus.read_u32(0x40).unwrap(), 0x1122_3344);
    assert_eq!(bus.read_u32(0x44).unwrap(), 0x5566_7788);
    assert_eq!(state.read_reg(Register::SI), 0x18);
    assert_eq!(state.read_reg(Register::DI), 0x48);
}

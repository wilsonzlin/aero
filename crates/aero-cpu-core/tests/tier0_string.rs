use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState, FLAG_CF, FLAG_DF, FLAG_SF, FLAG_ZF};
use aero_x86::Register;
use std::collections::BTreeMap;

fn run_to_halt<B: CpuBus>(state: &mut CpuState, bus: &mut B, max: u64) {
    let mut steps = 0u64;
    while steps < max {
        let res = run_batch(state, bus, 1024);
        steps += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => return,
            BatchExit::BiosInterrupt(vector) => {
                panic!(
                    "unexpected BIOS interrupt {vector:#x} at rip=0x{:X}",
                    state.rip()
                )
            }
            BatchExit::Assist(r) => panic!("unexpected assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception: {e:?}"),
        }
    }
    panic!("program did not halt");
}

#[derive(Debug)]
struct CountingBus {
    inner: FlatTestBus,
    bulk_copy_calls: usize,
    bulk_set_calls: usize,
}

#[derive(Debug, Default)]
struct SparseBus {
    mem: BTreeMap<u64, u8>,
    bulk_copy_calls: usize,
    bulk_set_calls: usize,
}

impl SparseBus {
    fn new() -> Self {
        Self::default()
    }

    fn load(&mut self, addr: u64, bytes: &[u8]) {
        for (i, b) in bytes.iter().copied().enumerate() {
            self.mem.insert(addr + i as u64, b);
        }
    }

    fn read_u8_raw(&self, addr: u64) -> u8 {
        self.mem.get(&addr).copied().unwrap_or(0)
    }
}

impl CpuBus for SparseBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception> {
        Ok(self.read_u8_raw(vaddr))
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, aero_cpu_core::Exception> {
        let b0 = self.read_u8_raw(vaddr) as u16;
        let b1 = self.read_u8_raw(vaddr + 1) as u16;
        Ok(b0 | (b1 << 8))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, aero_cpu_core::Exception> {
        let mut out = 0u32;
        for i in 0..4 {
            out |= (self.read_u8_raw(vaddr + i) as u32) << (i * 8);
        }
        Ok(out)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, aero_cpu_core::Exception> {
        let mut out = 0u64;
        for i in 0..8 {
            out |= (self.read_u8_raw(vaddr + i) as u64) << (i * 8);
        }
        Ok(out)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, aero_cpu_core::Exception> {
        let mut out = 0u128;
        for i in 0..16 {
            out |= (self.read_u8_raw(vaddr + i) as u128) << (i * 8);
        }
        Ok(out)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception> {
        self.mem.insert(vaddr, val);
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), aero_cpu_core::Exception> {
        for (i, b) in val.to_le_bytes().iter().copied().enumerate() {
            self.mem.insert(vaddr + i as u64, b);
        }
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), aero_cpu_core::Exception> {
        for (i, b) in val.to_le_bytes().iter().copied().enumerate() {
            self.mem.insert(vaddr + i as u64, b);
        }
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), aero_cpu_core::Exception> {
        for (i, b) in val.to_le_bytes().iter().copied().enumerate() {
            self.mem.insert(vaddr + i as u64, b);
        }
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), aero_cpu_core::Exception> {
        for (i, b) in val.to_le_bytes().iter().copied().enumerate() {
            self.mem.insert(vaddr + i as u64, b);
        }
        Ok(())
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(
        &mut self,
        dst: u64,
        src: u64,
        len: usize,
    ) -> Result<bool, aero_cpu_core::Exception> {
        self.bulk_copy_calls += 1;
        if len == 0 || dst == src {
            return Ok(true);
        }

        // Perform a memmove-style copy in *u64* address space. This intentionally does not apply
        // any x86 wrapping semantics so tests can detect when Tier-0 incorrectly uses the bulk
        // fast path across wrapped address ranges.
        let mut tmp = vec![0u8; len];
        for (i, slot) in tmp.iter_mut().enumerate() {
            *slot = self.read_u8_raw(src + i as u64);
        }
        for (i, b) in tmp.into_iter().enumerate() {
            self.mem.insert(dst + i as u64, b);
        }
        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(
        &mut self,
        dst: u64,
        pattern: &[u8],
        repeat: usize,
    ) -> Result<bool, aero_cpu_core::Exception> {
        self.bulk_set_calls += 1;
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }

        // Like `bulk_copy`, operate on the raw u64 address range without applying wrapping.
        for i in 0..repeat {
            for (j, b) in pattern.iter().copied().enumerate() {
                let addr = dst + (i * pattern.len() + j) as u64;
                self.mem.insert(addr, b);
            }
        }

        Ok(true)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = self.read_u8_raw(vaddr + i as u64);
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

impl CountingBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            bulk_copy_calls: 0,
            bulk_set_calls: 0,
        }
    }
}

impl CpuBus for CountingBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception> {
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, aero_cpu_core::Exception> {
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, aero_cpu_core::Exception> {
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, aero_cpu_core::Exception> {
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, aero_cpu_core::Exception> {
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u128(vaddr, val)
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(
        &mut self,
        dst: u64,
        src: u64,
        len: usize,
    ) -> Result<bool, aero_cpu_core::Exception> {
        self.bulk_copy_calls += 1;
        self.inner.bulk_copy(dst, src, len)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(
        &mut self,
        dst: u64,
        pattern: &[u8],
        repeat: usize,
    ) -> Result<bool, aero_cpu_core::Exception> {
        self.bulk_set_calls += 1;
        self.inner.bulk_set(dst, pattern, repeat)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception> {
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), aero_cpu_core::Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[test]
fn rep_movsb_df0_uses_bulk_copy_and_increments() {
    let count = 256u32;

    let code = [0xF3, 0xA4, 0xF4]; // rep movsb; hlt
    let mut bus = CountingBus::new(0x4000);
    bus.inner.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, 0x100);
    state.write_reg(Register::EDI, 0x200);
    state.write_reg(Register::ECX, count as u64);

    for i in 0..count as u64 {
        bus.inner.write_u8(0x100 + i, (i ^ 0x5A) as u8).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), 0x100 + count as u64);
    assert_eq!(state.read_reg(Register::EDI), 0x200 + count as u64);

    for i in 0..count as u64 {
        assert_eq!(bus.inner.read_u8(0x200 + i).unwrap(), (i ^ 0x5A) as u8);
    }
}

#[test]
fn rep_movsb_df1_uses_bulk_copy_and_decrements() {
    let count = 256u32;
    let src_start = 0x300u64;
    let dst_start = 0x500u64;

    let code = [0xF3, 0xA4, 0xF4]; // rep movsb; hlt
    let mut bus = CountingBus::new(0x2000);
    bus.inner.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.set_flag(FLAG_DF, true);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, (src_start + count as u64 - 1) as u64);
    state.write_reg(Register::EDI, (dst_start + count as u64 - 1) as u64);
    state.write_reg(Register::ECX, count as u64);

    for i in 0..count as u64 {
        bus.inner.write_u8(src_start + i, (i ^ 0xA5) as u8).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), src_start - 1);
    assert_eq!(state.read_reg(Register::EDI), dst_start - 1);

    for i in 0..count as u64 {
        assert_eq!(bus.inner.read_u8(dst_start + i).unwrap(), (i ^ 0xA5) as u8);
    }
}

#[test]
fn rep_movsb_overlap_hazard_df0_falls_back_to_element_wise() {
    // Hazard case: DF=0 copies low->high, and destination starts inside the source range at a
    // higher address. The result differs from memmove; we must not take the bulk-copy fast path.
    let count = 256u32;

    let code = [0xF3, 0xA4, 0xF4]; // rep movsb; hlt
    let mut bus = CountingBus::new(0x2000);
    bus.inner.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, 0x100);
    state.write_reg(Register::EDI, 0x101);
    state.write_reg(Register::ECX, count as u64);

    bus.inner.write_u8(0x100, 0xAA).unwrap();
    for i in 1..=count as u64 {
        bus.inner.write_u8(0x100 + i, i as u8).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), 0x100 + count as u64);
    assert_eq!(state.read_reg(Register::EDI), 0x101 + count as u64);

    for i in 1..=count as u64 {
        assert_eq!(bus.inner.read_u8(0x100 + i).unwrap(), 0xAA);
    }
}

#[test]
fn rep_stosb_uses_bulk_set() {
    let count = 256u32;

    let code = [0xF3, 0xAA, 0xF4]; // rep stosb; hlt
    let mut bus = CountingBus::new(0x2000);
    bus.inner.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.es.base = 0;
    state.write_reg(Register::EDI, 0x200);
    state.write_reg(Register::ECX, count as u64);
    state.write_reg(Register::AL, 0x5A);

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_set_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::EDI), 0x200 + count as u64);
    for i in 0..count as u64 {
        assert_eq!(bus.inner.read_u8(0x200 + i).unwrap(), 0x5A);
    }
}

#[test]
fn repe_cmpsb_stops_on_mismatch() {
    let code = [0xF3, 0xA6, 0xF4]; // repe cmpsb; hlt
    let mut bus = FlatTestBus::new(0x10000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);
    state.write_reg(Register::ECX, 5);

    // First 3 bytes match, 4th differs.
    for i in 0..5u64 {
        let src = if i == 3 { 0x99 } else { i as u8 };
        bus.write_u8(0x1000 + 0x10 + i, src).unwrap();
        bus.write_u8(0x2000 + 0x20 + i, i as u8).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    // Mismatch at i=3 means 4 iterations executed.
    assert_eq!(state.read_reg(Register::ESI), 0x14);
    assert_eq!(state.read_reg(Register::EDI), 0x24);
    assert_eq!(state.read_reg(Register::ECX), 1);
    assert!(!state.get_flag(FLAG_ZF));
}

#[test]
fn repne_scasb_stops_on_match() {
    let code = [0xF2, 0xAE, 0xF4]; // repne scasb; hlt
    let mut bus = FlatTestBus::new(0x10000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.es.base = 0x3000;
    state.write_reg(Register::EDI, 0x10);
    state.write_reg(Register::ECX, 6);
    state.write_reg(Register::AL, 0x7F);

    let hay = [0x00, 0x01, 0x02, 0x7F, 0x03, 0x04];
    for (i, &b) in hay.iter().enumerate() {
        bus.write_u8(0x3000 + 0x10 + i as u64, b).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(state.read_reg(Register::EDI), 0x14);
    assert_eq!(state.read_reg(Register::ECX), 2);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn cmpsb_flag_order_is_src_minus_dest() {
    // Intel: CMPS sets flags as if computing SRC - DEST.
    let code = [0xA6, 0xF4]; // cmpsb; hlt
    let mut bus = FlatTestBus::new(0x10000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);

    bus.write_u8(0x1000 + 0x10, 0x01).unwrap(); // SRC
    bus.write_u8(0x2000 + 0x20, 0x02).unwrap(); // DEST

    run_to_halt(&mut state, &mut bus, 100_000);

    assert!(!state.get_flag(FLAG_ZF));
    assert!(state.get_flag(FLAG_CF));
    assert!(state.get_flag(FLAG_SF));
}

#[test]
fn addr_size_override_uses_esi_edi_ecx_in_long_mode() {
    let code = [0x67, 0xF3, 0xA4, 0xF4]; // addr-size override + rep movsb; hlt
    let mut bus = FlatTestBus::new(0x10000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit64);
    state.set_rip(0);
    state.set_rflags(0x2);

    // High bits set; address-size override must use the low 32 bits.
    state.write_reg(Register::RSI, 0xAAAA_BBBB_0000_0010);
    state.write_reg(Register::RDI, 0xCCCC_DDDD_0000_0020);
    state.write_reg(Register::RCX, 0x1_0000_0002); // ECX=2, RCX high bits non-zero

    bus.write_u8(0x10, 0x10).unwrap();
    bus.write_u8(0x11, 0x11).unwrap();

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.read_u8(0x20).unwrap(), 0x10);
    assert_eq!(bus.read_u8(0x21).unwrap(), 0x11);

    // ESI/EDI were used and updated, zero-extending into RSI/RDI.
    assert_eq!(state.read_reg(Register::RSI), 0x0000_0000_0000_0012);
    assert_eq!(state.read_reg(Register::RDI), 0x0000_0000_0000_0022);

    // Count uses ECX, and writing ECX in long mode zero-extends RCX.
    assert_eq!(state.read_reg(Register::RCX), 0);
}

#[test]
fn addr_size_override_16bit_mode_selects_32bit_index_and_count_regs() {
    // Use CMPSB + REPE with a mismatch on the first element so we only execute one iteration. This
    // keeps the test fast even if the 32-bit counter register contains high bits.
    let code = [0x67, 0xF3, 0xA6, 0xF4]; // addr-size override + repe cmpsb; hlt
    let mut bus = FlatTestBus::new(0x20000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;

    state.write_reg(Register::ESI, 0x1_0000);
    state.write_reg(Register::EDI, 0x1_0100);
    state.write_reg(Register::ECX, 0x1_0000); // CX=0, ECX non-zero

    // Force mismatch immediately (ZF=0) so REPE stops after one iteration.
    bus.write_u8(0x1_0000, 0x01).unwrap(); // SRC
    bus.write_u8(0x1_0100, 0x02).unwrap(); // DEST

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(state.read_reg(Register::ESI), 0x1_0001);
    assert_eq!(state.read_reg(Register::EDI), 0x1_0101);
    assert_eq!(state.read_reg(Register::ECX), 0x0_FFFF);
    assert!(!state.get_flag(FLAG_ZF));
}

#[test]
fn addr_size_override_16bit_mode_lodsb_uses_esi_and_updates_esi() {
    let code = [0x67, 0xAC, 0xF4]; // addr-size override + lodsb; hlt
    let mut bus = FlatTestBus::new(0x30000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;

    // Ensure ESI != SI and that incrementing the effective index depends on a 32-bit carry.
    //
    // ESI=0x0002_FFFF, SI=0xFFFF
    state.write_reg(Register::ESI, 0x0002_0000);
    state.write_reg(Register::SI, 0xFFFF);

    // If Tier-0 incorrectly uses SI, we'd read 0xCD from 0xFFFF and ESI would become 0x0002_0000.
    bus.write_u8(0x0002_FFFF, 0xAB).unwrap();
    bus.write_u8(0xFFFF, 0xCD).unwrap();

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(state.read_reg(Register::AL), 0xAB);
    assert_eq!(state.read_reg(Register::ESI), 0x0003_0000);
}

#[test]
fn addr_size_override_32bit_mode_selects_16bit_index_regs() {
    let code = [0x67, 0xA4, 0xF4]; // addr-size override + movsb; hlt
    let mut bus = FlatTestBus::new(0x20000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;

    // High 16 bits set so SI/DI (16-bit) differ from ESI/EDI (32-bit) but all
    // addresses still stay within the test bus.
    state.write_reg(Register::ESI, 0x1_0010);
    state.write_reg(Register::EDI, 0x1_0020);

    bus.write_u8(0x10, 0xAA).unwrap();
    bus.write_u8(0x1_0010, 0xBB).unwrap();

    run_to_halt(&mut state, &mut bus, 100_000);

    // Should have copied from DS:SI (0x10) to ES:DI (0x20).
    assert_eq!(bus.read_u8(0x20).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x1_0020).unwrap(), 0x00);
}

#[test]
fn addr_size_override_32bit_mode_selects_cx_as_rep_counter() {
    // With address-size override in 32-bit mode, the REP counter is CX. Here CX=0 but ECX is
    // non-zero; the instruction must become a no-op.
    let code = [0x67, 0xF3, 0xA6, 0xF4]; // addr-size override + repe cmpsb; hlt
    let mut bus = FlatTestBus::new(0x10000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;
    state.segments.es.base = 0;

    // ECX=0x0001_0000, CX=0
    state.write_reg(Register::ECX, 0x0001_0000);
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);

    bus.write_u8(0x10, 0x01).unwrap();
    bus.write_u8(0x20, 0x02).unwrap();
    let flags_before = state.rflags();

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(state.read_reg(Register::ECX), 0x0001_0000);
    assert_eq!(state.read_reg(Register::ESI), 0x10);
    assert_eq!(state.read_reg(Register::EDI), 0x20);
    assert_eq!(state.rflags(), flags_before);
}

#[test]
fn segment_override_applies_to_source_only_for_movs() {
    let code = [0x64, 0xA4, 0xF4]; // fs movsb; hlt
    let mut bus = FlatTestBus::new(0x10000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0x1000;
    state.segments.fs.base = 0x3000;
    state.segments.es.base = 0x5000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);

    bus.write_u8(0x1000 + 0x10, 0xAA).unwrap();
    bus.write_u8(0x3000 + 0x10, 0xBB).unwrap();

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.read_u8(0x5000 + 0x20).unwrap(), 0xBB);
    assert_ne!(bus.read_u8(0x1000 + 0x20).unwrap(), 0xBB);
}

#[test]
fn rep_lodsb_repeats_and_consumes_ecx() {
    let code = [0xF3, 0xAC, 0xF4]; // rep lodsb; hlt
    let mut bus = FlatTestBus::new(0x1000);
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.segments.ds.base = 0;
    state.write_reg(Register::ESI, 0x100);
    state.write_reg(Register::ECX, 4);

    let bytes = [0x11u8, 0x22, 0x33, 0x44];
    for (i, b) in bytes.iter().enumerate() {
        bus.write_u8(0x100 + i as u64, *b).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(state.read_reg(Register::AL), 0x44);
    assert_eq!(state.read_reg(Register::ESI), 0x104);
    assert_eq!(state.read_reg(Register::ECX), 0);
}

#[test]
fn rep_movsb_addr16_wrap_skips_bulk_copy_and_uses_wrapping_offsets() {
    // In 16-bit address-size mode, SI/DI wrap at 0x10000. If the range wraps, the accessed
    // addresses are not contiguous in linear memory and we must not use bulk-copy fast paths.
    //
    // This test also ensures we don't corrupt instruction memory by placing the code far from the
    // wrapped data.
    let count = 128u16;
    let code_addr = 0x8000u64;

    let code = [0xF3, 0xA4, 0xF4]; // rep movsb; hlt
    let mut bus = CountingBus::new(0x30000);
    bus.inner.load(code_addr, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(code_addr);
    state.set_rflags(0x2);
    state.segments.cs.base = 0;
    state.segments.ds.base = 0;
    state.segments.es.base = 0x20000;
    state.write_reg(Register::SI, 0xFFC0);
    state.write_reg(Register::DI, 0x0100);
    state.write_reg(Register::CX, count as u64);

    // Source wraps: [0xFFC0..0xFFFF] then [0x0000..0x003F]
    for i in 0..64u64 {
        bus.inner.write_u8(0xFFC0 + i, 0xAA).unwrap();
        bus.inner.write_u8(i, 0xBB).unwrap();
        // This is what an incorrect bulk-copy would read instead of wrapping to 0x0000.
        bus.inner.write_u8(0x10000 + i, 0xCC).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.read_reg(Register::SI), 0x0040);
    assert_eq!(state.read_reg(Register::DI), 0x0180);

    let dst_base = 0x20000 + 0x0100;
    for i in 0..64u64 {
        assert_eq!(bus.inner.read_u8(dst_base + i).unwrap(), 0xAA);
        assert_eq!(bus.inner.read_u8(dst_base + 64 + i).unwrap(), 0xBB);
    }
}

#[test]
fn rep_stosb_addr16_wrap_skips_bulk_set_and_uses_wrapping_offsets() {
    // Same idea as the MOVSB test above, but for STOSB bulk-set behavior.
    let count = 128u16;
    let code_addr = 0x8000u64;

    let code = [0xF3, 0xAA, 0xF4]; // rep stosb; hlt
    let mut bus = CountingBus::new(0x20000);
    bus.inner.load(code_addr, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(code_addr);
    state.set_rflags(0x2);
    state.segments.cs.base = 0;
    state.segments.es.base = 0;
    state.write_reg(Register::DI, 0xFFC0);
    state.write_reg(Register::CX, count as u64);
    state.write_reg(Register::AL, 0x5A);

    for i in 0..64u64 {
        bus.inner.write_u8(0x10000 + i, 0xCC).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_set_calls, 0);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.read_reg(Register::DI), 0x0040);

    for i in 0..64u64 {
        assert_eq!(bus.inner.read_u8(0xFFC0 + i).unwrap(), 0x5A);
        assert_eq!(bus.inner.read_u8(i).unwrap(), 0x5A);
        // Incorrect bulk-set would have overwritten this region instead of wrapping to 0x0000.
        assert_eq!(bus.inner.read_u8(0x10000 + i).unwrap(), 0xCC);
    }
}

#[test]
fn rep_movsb_a20_wrap_skips_bulk_copy_and_uses_wrapping_addresses() {
    // With A20 disabled, real-mode physical addresses wrap at 1MiB. Bulk-copy assumes contiguous
    // linear memory, so we must not use it when the repeated range crosses the A20 boundary.
    let count = 128u16;
    let code_addr = 0x8000u64;

    let code = [0xF3, 0xA4, 0xF4]; // rep movsb; hlt
    let mut bus = CountingBus::new(0x100000);
    bus.inner.load(code_addr, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(code_addr);
    state.set_rflags(0x2);
    state.a20_enabled = false;

    // DS base at 0xFFFF0 makes SI=0 start 16 bytes before 1MiB; copying `count` bytes crosses
    // 0x100000 and should wrap to 0x00000.
    state.write_reg(Register::DS, 0xFFFF);
    state.write_reg(Register::ES, 0x0000);
    state.write_reg(Register::SI, 0x0000);
    state.write_reg(Register::DI, 0x0100);
    state.write_reg(Register::CX, count as u64);

    let src_base = 0xFFFF0u64;
    for i in 0..count as u64 {
        let val = (i ^ 0x5A) as u8;
        let addr = (src_base + i) & 0xFFFFF;
        bus.inner.write_u8(addr, val).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.read_reg(Register::SI), count as u64);
    assert_eq!(state.read_reg(Register::DI), 0x0100 + count as u64);

    for i in 0..count as u64 {
        let expected = (i ^ 0x5A) as u8;
        assert_eq!(bus.inner.read_u8(0x0100 + i).unwrap(), expected);
    }
}

#[test]
fn rep_movsb_a20_disabled_above_1mib_can_still_use_bulk_copy_if_contiguous() {
    // With A20 disabled, only address bit 20 aliases. Ranges above 1MiB can still be contiguous
    // (and safe to bulk-copy) when they don't cross a 1MiB boundary.
    let count = 128u32;
    let src_start = 0x20_0000u64;
    let dst_start = 0x21_0000u64;

    let code = [0x67, 0xF3, 0xA4, 0xF4]; // addr-size override + rep movsb; hlt
    let mut bus = CountingBus::new(0x400000);
    bus.inner.load(0, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(0);
    state.set_rflags(0x2);
    state.a20_enabled = false;
    state.write_reg(Register::DS, 0x0000);
    state.write_reg(Register::ES, 0x0000);

    state.write_reg(Register::ESI, src_start);
    state.write_reg(Register::EDI, dst_start);
    state.write_reg(Register::ECX, count as u64);

    for i in 0..count as u64 {
        bus.inner
            .write_u8(src_start + i, (i ^ 0xA5) as u8)
            .unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 1);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), src_start + count as u64);
    assert_eq!(state.read_reg(Register::EDI), dst_start + count as u64);

    for i in 0..count as u64 {
        assert_eq!(
            bus.inner.read_u8(dst_start + i).unwrap(),
            (i ^ 0xA5) as u8
        );
    }
}

#[test]
fn rep_stosb_a20_wrap_skips_bulk_set_and_uses_wrapping_addresses() {
    // Same idea as the MOVSB A20 test, but for STOSB bulk-set behavior.
    let count = 128u16;
    let code_addr = 0x8000u64;

    let code = [0xF3, 0xAA, 0xF4]; // rep stosb; hlt
    let mut bus = CountingBus::new(0x100000);
    bus.inner.load(code_addr, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(code_addr);
    state.set_rflags(0x2);
    state.a20_enabled = false;

    state.write_reg(Register::ES, 0xFFFF);
    state.write_reg(Register::DI, 0x0000);
    state.write_reg(Register::CX, count as u64);
    state.write_reg(Register::AL, 0x5A);

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_set_calls, 0);
    assert_eq!(state.read_reg(Register::CX), 0);
    assert_eq!(state.read_reg(Register::DI), count as u64);

    let dst_base = 0xFFFF0u64;
    for i in 0..count as u64 {
        let addr = (dst_base + i) & 0xFFFFF;
        assert_eq!(bus.inner.read_u8(addr).unwrap(), 0x5A);
    }
}

#[test]
fn rep_movsb_linear32_wrap_skips_bulk_copy_and_uses_wrapping_addresses() {
    // In non-long modes, linear addresses are 32-bit. A high segment base can make the linear
    // address range wrap at 4GiB even when the SI/DI offsets themselves don't wrap, so we must not
    // use bulk-copy fast paths.
    let count = 128u32;
    let code_addr = 0x8000u64;

    let code = [0xF3, 0xA4, 0xF4]; // rep movsb; hlt
    let mut bus = SparseBus::new();
    bus.load(code_addr, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(code_addr);
    state.set_rflags(0x2);
    state.segments.ds.base = 0xFFFF_FFC0;
    state.segments.es.base = 0;
    state.write_reg(Register::ESI, 0);
    state.write_reg(Register::EDI, 0x0100);
    state.write_reg(Register::ECX, count as u64);

    // Source wraps: [0xFFFF_FFC0..0xFFFF_FFFF] then [0x0000_0000..0x0000_003F].
    for i in 0..64u64 {
        bus.write_u8(0xFFFF_FFC0 + i, 0xAA).unwrap();
        bus.write_u8(i, 0xBB).unwrap();
        // Incorrect bulk-copy would read these bytes instead of wrapping back to 0x0000_0000.
        bus.write_u8(0x1_0000_0000 + i, 0xCC).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_copy_calls, 0);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::ESI), count as u64);
    assert_eq!(state.read_reg(Register::EDI), 0x0100 + count as u64);

    for i in 0..64u64 {
        assert_eq!(bus.read_u8(0x0100 + i).unwrap(), 0xAA);
        assert_eq!(bus.read_u8(0x0100 + 64 + i).unwrap(), 0xBB);
        assert_eq!(bus.read_u8(0x1_0000_0000 + i).unwrap(), 0xCC);
    }
}

#[test]
fn rep_stosb_linear32_wrap_skips_bulk_set_and_uses_wrapping_addresses() {
    let count = 128u32;
    let code_addr = 0x8000u64;

    let code = [0xF3, 0xAA, 0xF4]; // rep stosb; hlt
    let mut bus = SparseBus::new();
    bus.load(code_addr, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(code_addr);
    state.set_rflags(0x2);
    state.segments.es.base = 0xFFFF_FFC0;
    state.write_reg(Register::EDI, 0);
    state.write_reg(Register::ECX, count as u64);
    state.write_reg(Register::AL, 0x5A);

    for i in 0..64u64 {
        bus.write_u8(i, 0xCC).unwrap();
        bus.write_u8(0x1_0000_0000 + i, 0xCC).unwrap();
    }

    run_to_halt(&mut state, &mut bus, 100_000);

    assert_eq!(bus.bulk_set_calls, 0);
    assert_eq!(state.read_reg(Register::ECX), 0);
    assert_eq!(state.read_reg(Register::EDI), count as u64);

    for i in 0..64u64 {
        assert_eq!(bus.read_u8(0xFFFF_FFC0 + i).unwrap(), 0x5A);
        assert_eq!(bus.read_u8(i).unwrap(), 0x5A);
        assert_eq!(bus.read_u8(0x1_0000_0000 + i).unwrap(), 0xCC);
    }
}

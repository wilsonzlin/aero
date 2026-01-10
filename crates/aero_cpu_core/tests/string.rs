use aero_cpu_core::{Bus, Cpu, CpuMode, RamBus};

fn setup_bus() -> RamBus {
    RamBus::new(0x10_000)
}

#[test]
fn movsb_df0_and_df1() {
    // DF=0 increments.
    let mut cpu = Cpu::new(CpuMode::Real16);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_si(0x10);
    cpu.regs.set_di(0x20);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0xAA);

    cpu.execute_bytes(&mut bus, &[0xA4]).unwrap(); // MOVSB
    assert_eq!(bus.read_u8(0x2000 + 0x20), 0xAA);
    assert_eq!(cpu.regs.si(), 0x11);
    assert_eq!(cpu.regs.di(), 0x21);

    // DF=1 decrements.
    let mut cpu = Cpu::new(CpuMode::Real16);
    cpu.rflags.set_df(true);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_si(0x10);
    cpu.regs.set_di(0x20);
    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0xBB);

    cpu.execute_bytes(&mut bus, &[0xA4]).unwrap(); // MOVSB
    assert_eq!(bus.read_u8(0x2000 + 0x20), 0xBB);
    assert_eq!(cpu.regs.si(), 0x0F);
    assert_eq!(cpu.regs.di(), 0x1F);
}

#[test]
fn stosw_df0_and_df1() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_di(0x100);
    cpu.regs.set_ax(0x1234);

    let mut bus = setup_bus();
    cpu.execute_bytes(&mut bus, &[0x66, 0xAB]).unwrap(); // STOSW
    assert_eq!(bus.read_u16(0x2000 + 0x100), 0x1234);
    assert_eq!(cpu.regs.di(), 0x102);

    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.rflags.set_df(true);
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_di(0x100);
    cpu.regs.set_ax(0x5678);
    let mut bus = setup_bus();
    cpu.execute_bytes(&mut bus, &[0x66, 0xAB]).unwrap(); // STOSW
    assert_eq!(bus.read_u16(0x2000 + 0x100), 0x5678);
    assert_eq!(cpu.regs.di(), 0x0FE);
}

#[test]
fn lodsd_df0_and_df1() {
    let mut cpu = Cpu::new(CpuMode::Long64);
    cpu.segs.ds.base = 0x1000; // ignored in long mode (DS base forced to 0)
    cpu.segs.fs.base = 0x3000;
    cpu.regs.set_rsi(0x20);
    let mut bus = setup_bus();
    bus.write_u32(0x3000 + 0x20, 0xDEADBEEF);

    // FS override, DF=0 increments.
    cpu.execute_bytes(&mut bus, &[0x64, 0xAD]).unwrap(); // LODSD
    assert_eq!(cpu.regs.eax(), 0xDEADBEEF);
    assert_eq!(cpu.regs.rsi, 0x24);

    // DF=1 decrements.
    let mut cpu = Cpu::new(CpuMode::Long64);
    cpu.rflags.set_df(true);
    cpu.segs.fs.base = 0x3000;
    cpu.regs.set_rsi(0x20);
    let mut bus = setup_bus();
    bus.write_u32(0x3000 + 0x20, 0xAABBCCDD);
    cpu.execute_bytes(&mut bus, &[0x64, 0xAD]).unwrap(); // LODSD
    assert_eq!(cpu.regs.eax(), 0xAABBCCDD);
    assert_eq!(cpu.regs.rsi, 0x1C);
}

#[test]
fn rep_count_zero_is_noop() {
    let mut cpu = Cpu::new(CpuMode::Real16);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_si(0x10);
    cpu.regs.set_di(0x20);
    cpu.regs.set_cx(0);
    cpu.rflags.set_df(true);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0x11);
    bus.write_u8(0x2000 + 0x20, 0x22);
    let flags_before = cpu.rflags.bits();

    cpu.execute_bytes(&mut bus, &[0xF3, 0xA4]).unwrap(); // REP MOVSB

    assert_eq!(bus.read_u8(0x2000 + 0x20), 0x22);
    assert_eq!(cpu.regs.si(), 0x10);
    assert_eq!(cpu.regs.di(), 0x20);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.rflags.bits(), flags_before);
}

#[test]
fn repe_cmpsb_stops_on_mismatch() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_esi(0x10, CpuMode::Protected32);
    cpu.regs.set_edi(0x20, CpuMode::Protected32);
    cpu.regs.set_ecx(5, CpuMode::Protected32);

    let mut bus = setup_bus();
    // First 3 bytes match, 4th differs.
    for i in 0..5 {
        bus.write_u8(0x1000 + 0x10 + i, if i == 3 { 0x99 } else { i as u8 });
        bus.write_u8(0x2000 + 0x20 + i, i as u8);
    }

    cpu.execute_bytes(&mut bus, &[0xF3, 0xA6]).unwrap(); // REPE CMPSB
    // Per-iteration: compare then inc/dec then decrement ECX.
    // Mismatch at i=3 means 4 iterations executed.
    assert_eq!(cpu.regs.esi(), 0x14);
    assert_eq!(cpu.regs.edi(), 0x24);
    assert_eq!(cpu.regs.ecx(), 1);
    assert!(!cpu.rflags.zf());
}

#[test]
fn repne_scasb_stops_on_match() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.segs.es.base = 0x3000;
    cpu.regs.set_edi(0x10, CpuMode::Protected32);
    cpu.regs.set_ecx(6, CpuMode::Protected32);
    cpu.regs.set_al(0x7F);

    let mut bus = setup_bus();
    let hay = [0x00, 0x01, 0x02, 0x7F, 0x03, 0x04];
    for (i, &b) in hay.iter().enumerate() {
        bus.write_u8(0x3000 + 0x10 + i as u64, b);
    }

    cpu.execute_bytes(&mut bus, &[0xF2, 0xAE]).unwrap(); // REPNE SCASB
    // Stops when ZF=1 at index 3, after 4 iterations executed.
    assert_eq!(cpu.regs.edi(), 0x14);
    assert_eq!(cpu.regs.ecx(), 2);
    assert!(cpu.rflags.zf());
}

#[test]
fn cmpsb_df1_decrements_si_di_and_sets_flags() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.rflags.set_df(true);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_esi(0x12, CpuMode::Protected32);
    cpu.regs.set_edi(0x22, CpuMode::Protected32);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x12, 0x5A);
    bus.write_u8(0x2000 + 0x22, 0x5A);

    cpu.execute_bytes(&mut bus, &[0xA6]).unwrap(); // CMPSB
    assert_eq!(cpu.regs.esi(), 0x11);
    assert_eq!(cpu.regs.edi(), 0x21);
    assert!(cpu.rflags.zf());
}

#[test]
fn scasb_df1_decrements_di_and_sets_flags() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.rflags.set_df(true);
    cpu.segs.es.base = 0x3000;
    cpu.regs.set_edi(0x10, CpuMode::Protected32);
    cpu.regs.set_al(0x42);

    let mut bus = setup_bus();
    bus.write_u8(0x3000 + 0x10, 0x42);

    cpu.execute_bytes(&mut bus, &[0xAE]).unwrap(); // SCASB
    assert_eq!(cpu.regs.edi(), 0x0F);
    assert!(cpu.rflags.zf());
}

#[test]
fn addr_size_override_uses_esi_edi_ecx_in_long_mode() {
    let mut cpu = Cpu::new(CpuMode::Long64);
    cpu.segs.ds.base = 0x1111_0000; // ignored (DS base forced to 0 in long mode)
    cpu.segs.es.base = 0x2222_0000; // ignored

    // High bits set; address-size override must use the low 32 bits.
    cpu.regs.set_rsi(0xAAAA_BBBB_0000_0010);
    cpu.regs.set_rdi(0xCCCC_DDDD_0000_0020);
    cpu.regs.set_rcx(0x1_0000_0002); // ECX=2, RCX high bits non-zero.

    let mut bus = setup_bus();
    bus.write_u8(0x10, 0x10);
    bus.write_u8(0x11, 0x11);

    // 67 F3 A4 => addr-size override + REP MOVSB
    cpu.execute_bytes(&mut bus, &[0x67, 0xF3, 0xA4]).unwrap();

    assert_eq!(bus.read_u8(0x20), 0x10);
    assert_eq!(bus.read_u8(0x21), 0x11);

    // ESI/EDI were used and updated, zero-extending into RSI/RDI.
    assert_eq!(cpu.regs.rsi, 0x0000_0000_0000_0012);
    assert_eq!(cpu.regs.rdi, 0x0000_0000_0000_0022);

    // Count uses ECX, and writing ECX in long mode zero-extends RCX.
    assert_eq!(cpu.regs.rcx, 0);
}

#[test]
fn segment_override_applies_to_source_only_for_movs() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.fs.base = 0x3000;
    cpu.segs.es.base = 0x5000;
    cpu.regs.set_esi(0x10, CpuMode::Protected32);
    cpu.regs.set_edi(0x20, CpuMode::Protected32);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0xAA);
    bus.write_u8(0x3000 + 0x10, 0xBB);

    // FS override MOVSB: should read from FS:ESI, write to ES:EDI.
    cpu.execute_bytes(&mut bus, &[0x64, 0xA4]).unwrap();
    assert_eq!(bus.read_u8(0x5000 + 0x20), 0xBB);

    // Segment override must not change the destination segment (still ES).
    assert_ne!(bus.read_u8(0x1000 + 0x20), 0xBB);
}

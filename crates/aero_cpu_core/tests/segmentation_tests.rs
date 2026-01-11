use aero_cpu_core::descriptors::{parse_descriptor_8, Descriptor};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::mode::CodeMode;
use aero_cpu_core::segmentation::{AccessType, LoadReason, Seg};
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::Exception;

fn make_descriptor(
    base: u32,
    limit_raw: u32,
    typ: u8,
    s: bool,
    dpl: u8,
    present: bool,
    avl: bool,
    l: bool,
    db: bool,
    g: bool,
) -> u64 {
    let mut raw = 0u64;
    raw |= (limit_raw & 0xFFFF) as u64;
    raw |= ((base & 0xFFFF) as u64) << 16;
    raw |= (((base >> 16) & 0xFF) as u64) << 32;
    let access =
        (typ as u64) | ((s as u64) << 4) | (((dpl as u64) & 0x3) << 5) | ((present as u64) << 7);
    raw |= access << 40;
    raw |= (((limit_raw >> 16) & 0xF) as u64) << 48;
    let flags = (avl as u64) | ((l as u64) << 1) | ((db as u64) << 2) | ((g as u64) << 3);
    raw |= flags << 52;
    raw |= (((base >> 24) & 0xFF) as u64) << 56;
    raw
}

fn make_system_descriptor_16(
    base: u64,
    limit_raw: u32,
    typ: u8,
    dpl: u8,
    present: bool,
) -> (u64, u64) {
    let low = make_descriptor(
        base as u32,
        limit_raw,
        typ,
        false,
        dpl,
        present,
        false,
        false,
        false,
        false,
    );
    let high = (base >> 32) & 0xFFFF_FFFF;
    (low, high)
}

#[test]
fn parse_segment_descriptor_limit_granularity() {
    // 32-bit code segment: base=0, limit_raw=0xFFFFF, G=1 => 4GiB-1 effective limit.
    let raw = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let parsed = parse_descriptor_8(raw);
    match parsed {
        Descriptor::Segment(seg) => {
            assert_eq!(seg.base, 0);
            assert_eq!(seg.limit, 0xFFFF_FFFF);
            assert!(seg.attrs.is_code());
            assert_eq!(seg.attrs.dpl, 0);
            assert!(seg.attrs.present);
            assert!(seg.attrs.default_big);
            assert!(seg.attrs.granularity);
        }
        other => panic!("unexpected descriptor: {other:?}"),
    }
}

fn setup_gdt(bus: &mut FlatTestBus, gdt_base: u64, descriptors: &[u64]) {
    for (i, &desc) in descriptors.iter().enumerate() {
        bus.load(gdt_base + (i as u64) * 8, &desc.to_le_bytes());
    }
}

#[test]
fn load_ds_null_selector_allowed_and_faults_on_use() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);
    assert_eq!(cpu.cpu_mode(), CpuMode::Protected);

    let mut bus = FlatTestBus::new(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let data32 = make_descriptor(0, 0xFFFFF, 0x2, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code32, data32]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    cpu.load_seg(&mut bus, Seg::DS, 0x0000, LoadReason::Data)
        .unwrap();

    assert!(cpu.segments.ds.is_unusable());
    assert_eq!(
        cpu.linearize(Seg::DS, 0x10, AccessType::read(1)),
        Err(Exception::gp0())
    );
}

#[test]
fn load_ss_null_selector_faults() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);
    let mut bus = FlatTestBus::new(0x100);

    assert_eq!(
        cpu.load_seg(&mut bus, Seg::SS, 0, LoadReason::Stack),
        Err(Exception::gp0())
    );
}

#[test]
fn load_non_present_descriptor_raises_np_or_ss() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let data_np = make_descriptor(0, 0xFFFFF, 0x2, true, 0, false, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code32, data_np]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);
    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();

    assert_eq!(
        cpu.load_seg(&mut bus, Seg::DS, 0x10, LoadReason::Data),
        Err(Exception::np(0x10))
    );
    assert_eq!(
        cpu.load_seg(&mut bus, Seg::SS, 0x10, LoadReason::Stack),
        Err(Exception::ss(0x10))
    );
}

#[test]
fn privilege_check_on_data_segment_load() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);
    cpu.segments.cs.selector = 3;

    let mut bus = FlatTestBus::new(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code_ring3 = make_descriptor(0, 0xFFFFF, 0xA, true, 3, true, false, false, true, true);
    let data_ring0 = make_descriptor(0, 0xFFFFF, 0x2, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code_ring3, data_ring0]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08 | 3, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpl(), 3);

    assert_eq!(
        cpu.load_seg(&mut bus, Seg::DS, 0x10 | 3, LoadReason::Data),
        Err(Exception::gp(0x13))
    );
}

#[test]
fn real_to_protected_to_long_transition() {
    let mut cpu = CpuState::new(CpuMode::Real);
    assert_eq!(cpu.cpu_mode(), CpuMode::Real);
    assert_eq!(cpu.code_mode(), CodeMode::Real16);

    let mut bus = FlatTestBus::new(0x4000);
    let gdt_base = 0x200;
    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let code64 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, true, false, true);
    let data32 = make_descriptor(0, 0xFFFFF, 0x2, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code32, data32, code64]);
    cpu.set_gdtr(gdt_base, (4 * 8 - 1) as u16);

    cpu.set_protected_enable(true);
    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpu_mode(), CpuMode::Protected);
    assert_eq!(cpu.code_mode(), CodeMode::Protected32);

    cpu.set_cr4(cpu.control.cr4 | CR4_PAE);
    cpu.set_efer(cpu.msr.efer | EFER_LME);
    cpu.set_cr0(cpu.control.cr0 | CR0_PG);

    cpu.load_seg(&mut bus, Seg::CS, 0x18, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpu_mode(), CpuMode::Long);
    assert_eq!(cpu.code_mode(), CodeMode::Long64);
}

#[test]
fn long_mode_fs_gs_base_and_swapgs() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code64 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, true, false, true);
    setup_gdt(&mut bus, gdt_base, &[null, code64]);
    cpu.set_gdtr(gdt_base, (2 * 8 - 1) as u16);

    cpu.set_cr4(cpu.control.cr4 | CR4_PAE);
    cpu.set_efer(cpu.msr.efer | EFER_LME);
    cpu.set_cr0(cpu.control.cr0 | CR0_PG);
    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpu_mode(), CpuMode::Long);

    // Use canonical addresses for MSR-backed bases.
    cpu.msr.fs_base = 0x0000_2222_3333_4444;
    cpu.msr.gs_base = 0xFFFF_8000_0000_2000;
    cpu.msr.kernel_gs_base = 0x0000_0000_1234_5678;

    assert_eq!(cpu.seg_base(Seg::FS), cpu.msr.fs_base);
    assert_eq!(cpu.seg_base(Seg::GS), cpu.msr.gs_base);
    assert_eq!(cpu.seg_base(Seg::DS), 0);

    assert_eq!(
        cpu.linearize(Seg::FS, 0x10, AccessType::read(1)).unwrap(),
        cpu.msr.fs_base + 0x10
    );

    cpu.swapgs();
    assert_eq!(cpu.seg_base(Seg::GS), 0x0000_0000_1234_5678);

    // Non-canonical address must fault.
    assert_eq!(
        cpu.linearize(Seg::DS, 0x0001_0000_0000_0000, AccessType::read(1)),
        Err(Exception::gp0())
    );
}

#[test]
fn conforming_code_segment_does_not_change_cpl() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);
    cpu.segments.cs.selector = 3;

    let mut bus = FlatTestBus::new(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code_ring3 = make_descriptor(0, 0xFFFFF, 0xA, true, 3, true, false, false, true, true);
    let conforming_ring0 =
        make_descriptor(0, 0xFFFFF, 0xE, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code_ring3, conforming_ring0]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08 | 3, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpl(), 3);

    cpu.load_seg(&mut bus, Seg::CS, 0x10 | 3, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpl(), 3);
    assert_eq!(cpu.segments.cs.selector & 0x3, 3);
    assert!(cpu.segments.cs.code_conforming());
}

#[test]
fn expand_down_segment_enforces_lower_bound() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    // 16-bit expand-down writable data segment with limit 0x0FFF.
    let expand_down = make_descriptor(
        0x1234_0000,
        0x0FFF,
        0x6,
        true,
        0,
        true,
        false,
        false,
        false,
        false,
    );
    setup_gdt(&mut bus, gdt_base, &[null, code32, expand_down]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    cpu.load_seg(&mut bus, Seg::DS, 0x10, LoadReason::Data)
        .unwrap();

    assert_eq!(
        cpu.linearize(Seg::DS, 0x0FFF, AccessType::read(1)),
        Err(Exception::gp0())
    );

    assert_eq!(
        cpu.linearize(Seg::DS, 0x1000, AccessType::read(1)).unwrap(),
        0x1234_0000u64 + 0x1000
    );
}

#[test]
fn descriptor_table_limit_violation_raises_gp() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x1000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code32]);
    cpu.set_gdtr(gdt_base, (2 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();

    assert_eq!(
        cpu.load_seg(&mut bus, Seg::DS, 0x10, LoadReason::Data),
        Err(Exception::gp(0x10))
    );
}

#[test]
fn load_ldtr_allows_accessing_ldt_descriptors() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x4000);
    let gdt_base = 0x100;
    let ldt_base = 0x800;

    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let ldt_desc = make_descriptor(
        ldt_base as u32,
        0x0F,
        0x2,
        false,
        0,
        true,
        false,
        false,
        false,
        false,
    );
    setup_gdt(&mut bus, gdt_base, &[null, code32, ldt_desc]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    // LDT entry 1: flat data segment with base 0x1234_0000.
    let ldt_data = make_descriptor(
        0x1234_0000,
        0xFFFF,
        0x2,
        true,
        0,
        true,
        false,
        false,
        false,
        false,
    );
    bus.load(ldt_base + 8, &ldt_data.to_le_bytes());

    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    cpu.load_ldtr(&mut bus, 0x10).unwrap();

    // Selector index 1 in LDT: (1<<3) | TI.
    cpu.load_seg(&mut bus, Seg::DS, 0x0C, LoadReason::Data)
        .unwrap();
    assert_eq!(cpu.seg_base(Seg::DS), 0x1234_0000);
}

#[test]
fn tss32_ring0_stack_read() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x4000);
    let gdt_base = 0x100;
    let tss_base = 0x900;

    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let tss_desc = make_descriptor(
        tss_base as u32,
        0x67,
        0x9,
        false,
        0,
        true,
        false,
        false,
        false,
        false,
    );
    setup_gdt(&mut bus, gdt_base, &[null, code32, tss_desc]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    cpu.load_tr(&mut bus, 0x10).unwrap();

    let esp0 = 0xDEAD_BEEFu32;
    let ss0 = 0x10u16;
    bus.load(tss_base + 4, &esp0.to_le_bytes());
    bus.load(tss_base + 8, &ss0.to_le_bytes());

    assert_eq!(cpu.tss32_ring0_stack(&mut bus).unwrap(), (ss0, esp0));
}

#[test]
fn tss64_rsp0_and_ist_reads() {
    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_protected_enable(true);

    let mut bus = FlatTestBus::new(0x8000);
    let gdt_base = 0x100;
    let tss_base = 0x2000u64;

    let null = 0u64;
    let code64 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, true, false, true);
    let (tss_low, tss_high) = make_system_descriptor_16(tss_base, 0x67, 0x9, 0, true);
    setup_gdt(&mut bus, gdt_base, &[null, code64, tss_low, tss_high]);
    cpu.set_gdtr(gdt_base, (4 * 8 - 1) as u16);

    cpu.set_cr4(cpu.control.cr4 | CR4_PAE);
    cpu.set_efer(cpu.msr.efer | EFER_LME);
    cpu.set_cr0(cpu.control.cr0 | CR0_PG);
    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpu_mode(), CpuMode::Long);

    cpu.load_tr(&mut bus, 0x10).unwrap();

    let rsp0 = 0xFFFF_8000_0000_1234u64;
    let ist1 = 0xFFFF_8000_0000_2000u64;
    let iomap_base = 0x70u16;
    bus.load(tss_base + 4, &rsp0.to_le_bytes());
    bus.load(tss_base + 0x24, &ist1.to_le_bytes());
    bus.load(tss_base + 0x66, &iomap_base.to_le_bytes());

    assert_eq!(cpu.tss64_rsp0(&mut bus).unwrap(), rsp0);
    assert_eq!(cpu.tss64_ist(&mut bus, 1).unwrap(), ist1);
    assert_eq!(cpu.tss64_iomap_base(&mut bus).unwrap(), iomap_base);
    assert_eq!(cpu.tss64_ist(&mut bus, 0), Err(Exception::ts(0)));
}

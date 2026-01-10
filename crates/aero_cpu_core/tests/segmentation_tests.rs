use aero_cpu_core::descriptors::{parse_descriptor_8, Descriptor};
use aero_cpu_core::mode::{CodeMode, CpuMode, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::segmentation::{AccessType, LoadReason, Seg};
use aero_cpu_core::{CpuBus, CpuState, Exception};

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

#[derive(Default)]
struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn with_size(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        let addr = addr as usize;
        self.mem[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
    }
}

impl CpuBus for TestBus {
    fn read_u8(&mut self, addr: u64) -> Result<u8, Exception> {
        self.mem
            .get(addr as usize)
            .copied()
            .ok_or_else(Exception::gp0)
    }
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

fn setup_gdt(bus: &mut TestBus, gdt_base: u64, descriptors: &[u64]) {
    for (i, &desc) in descriptors.iter().enumerate() {
        bus.write_u64(gdt_base + (i as u64) * 8, desc);
    }
}

#[test]
fn load_ds_null_selector_allowed_and_faults_on_use() {
    let mut cpu = CpuState::new();
    cpu.set_protected_enable(true);
    assert_eq!(cpu.cpu_mode(), CpuMode::Protected);

    let mut bus = TestBus::with_size(0x2000);
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

    assert_eq!(cpu.segments.ds.cache.attrs.unusable, true);
    assert_eq!(
        cpu.linearize(Seg::DS, 0x10, AccessType::read(1)),
        Err(Exception::gp0())
    );
}

#[test]
fn load_ss_null_selector_faults() {
    let mut cpu = CpuState::new();
    cpu.set_protected_enable(true);
    let mut bus = TestBus::with_size(0x100);

    assert_eq!(
        cpu.load_seg(&mut bus, Seg::SS, 0, LoadReason::Stack),
        Err(Exception::gp0())
    );
}

#[test]
fn load_non_present_descriptor_raises_np_or_ss() {
    let mut cpu = CpuState::new();
    cpu.set_protected_enable(true);

    let mut bus = TestBus::with_size(0x2000);
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
    let mut cpu = CpuState::new();
    cpu.set_protected_enable(true);
    cpu.cpl = 3;

    let mut bus = TestBus::with_size(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code_ring3 = make_descriptor(0, 0xFFFFF, 0xA, true, 3, true, false, false, true, true);
    let data_ring0 = make_descriptor(0, 0xFFFFF, 0x2, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code_ring3, data_ring0]);
    cpu.set_gdtr(gdt_base, (3 * 8 - 1) as u16);

    cpu.load_seg(&mut bus, Seg::CS, 0x08 | 3, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpl, 3);

    assert_eq!(
        cpu.load_seg(&mut bus, Seg::DS, 0x10 | 3, LoadReason::Data),
        Err(Exception::gp(0x13))
    );
}

#[test]
fn real_to_protected_to_long_transition() {
    let mut cpu = CpuState::new();
    assert_eq!(cpu.cpu_mode(), CpuMode::Real);
    assert_eq!(cpu.code_mode(), CodeMode::Real16);

    let mut bus = TestBus::with_size(0x4000);
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

    cpu.set_cr4(cpu.cr4 | CR4_PAE);
    cpu.set_efer(cpu.efer | EFER_LME);
    cpu.set_cr0(cpu.cr0 | CR0_PG);

    cpu.load_seg(&mut bus, Seg::CS, 0x18, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpu_mode(), CpuMode::Long);
    assert_eq!(cpu.code_mode(), CodeMode::Long64);
}

#[test]
fn long_mode_fs_gs_base_and_swapgs() {
    let mut cpu = CpuState::new();
    cpu.set_protected_enable(true);

    let mut bus = TestBus::with_size(0x2000);
    let gdt_base = 0x100;
    let null = 0u64;
    let code64 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, true, false, true);
    setup_gdt(&mut bus, gdt_base, &[null, code64]);
    cpu.set_gdtr(gdt_base, (2 * 8 - 1) as u16);

    cpu.set_cr4(cpu.cr4 | CR4_PAE);
    cpu.set_efer(cpu.efer | EFER_LME);
    cpu.set_cr0(cpu.cr0 | CR0_PG);
    cpu.load_seg(&mut bus, Seg::CS, 0x08, LoadReason::FarControlTransfer)
        .unwrap();
    assert_eq!(cpu.cpu_mode(), CpuMode::Long);

    // Use canonical addresses for MSR-backed bases.
    cpu.msr_fs_base = 0x0000_2222_3333_4444;
    cpu.msr_gs_base = 0xFFFF_8000_0000_2000;
    cpu.msr_kernel_gs_base = 0x0000_0000_1234_5678;

    assert_eq!(cpu.seg_base(Seg::FS), cpu.msr_fs_base);
    assert_eq!(cpu.seg_base(Seg::GS), cpu.msr_gs_base);
    assert_eq!(cpu.seg_base(Seg::DS), 0);

    assert_eq!(
        cpu.linearize(Seg::FS, 0x10, AccessType::read(1)).unwrap(),
        cpu.msr_fs_base + 0x10
    );

    cpu.swapgs();
    assert_eq!(cpu.seg_base(Seg::GS), 0x0000_0000_1234_5678);

    // Non-canonical address must fault.
    assert_eq!(
        cpu.linearize(Seg::DS, 0x0001_0000_0000_0000, AccessType::read(1)),
        Err(Exception::gp0())
    );
}

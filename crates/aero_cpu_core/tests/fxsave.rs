use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::sse_state::MXCSR_MASK;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_EM, CR0_TS, CR4_OSFXSR, CR4_OSXMMEXCPT};
use aero_cpu_core::{Exception, FXSAVE_AREA_SIZE};
use aero_x86::Register;

const BUS_SIZE: usize = 0x4000;
const CODE_BASE: u64 = 0x0100;
const DATA_BASE: u64 = 0x0200;

const ST80_MASK: u128 = (1u128 << 80) - 1;
const FSW_TOP_MASK: u16 = 0b111 << 11;

fn patterned_u128(seed: u8) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = seed.wrapping_add(i as u8);
    }
    u128::from_le_bytes(bytes)
}

fn patterned_st80(seed: u8) -> u128 {
    let mut bytes = [0u8; 16];
    for i in 0..10 {
        bytes[i] = seed.wrapping_add(i as u8);
    }
    u128::from_le_bytes(bytes)
}

fn mask_st80(raw: u128) -> u128 {
    raw & ST80_MASK
}

fn step_ok(state: &mut CpuState, bus: &mut FlatTestBus) {
    match step(state, bus) {
        Ok(StepExit::Continue) => {}
        Ok(other) => panic!("unexpected tier0 exit: {other:?}"),
        Err(e) => panic!("unexpected tier0 exception: {e:?}"),
    }
}

#[test]
fn fxsave_legacy_layout_matches_intel_sdm_via_tier0() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE);

    state.fpu.fcw = 0x1234;
    state.fpu.fsw = 0x4567;
    state.fpu.top = 3;
    state.fpu.ftw = 0x9A;
    state.fpu.fop = 0xBEEF;
    state.fpu.fip = 0x1122_3344;
    state.fpu.fcs = 0x5566;
    state.fpu.fdp = 0x7788_99AA;
    state.fpu.fds = 0xBBCC;

    state.sse.mxcsr = 0xCCDD & MXCSR_MASK;

    for i in 0..8 {
        // Fill the reserved bytes to ensure FXSAVE zeroes them in the output.
        state.fpu.st[i] = patterned_u128(0x10 + i as u8);
    }
    for i in 0..16 {
        state.sse.xmm[i] = patterned_u128(0x80 + i as u8);
    }

    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x00]); // fxsave [rax]

    step_ok(&mut state, &mut bus);

    let image = bus.slice(DATA_BASE, FXSAVE_AREA_SIZE);

    let mut expected = [0u8; FXSAVE_AREA_SIZE];
    expected[0..2].copy_from_slice(&0x1234u16.to_le_bytes());
    expected[2..4].copy_from_slice(&(state.fpu.fsw_with_top()).to_le_bytes());
    expected[4] = 0x9A;
    expected[6..8].copy_from_slice(&0xBEEFu16.to_le_bytes());
    expected[8..12].copy_from_slice(&(0x1122_3344u32).to_le_bytes());
    expected[12..14].copy_from_slice(&0x5566u16.to_le_bytes());
    expected[16..20].copy_from_slice(&(0x7788_99AAu32).to_le_bytes());
    expected[20..22].copy_from_slice(&0xBBCCu16.to_le_bytes());
    expected[24..28].copy_from_slice(&(0xCCDDu32).to_le_bytes());
    expected[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

    for i in 0..8 {
        let start = 32 + i * 16;
        expected[start..start + 16].copy_from_slice(&patterned_st80(0x10 + i as u8).to_le_bytes());
    }

    for i in 0..8 {
        let start = 160 + i * 16;
        expected[start..start + 16].copy_from_slice(&state.sse.xmm[i].to_le_bytes());
    }

    assert_eq!(image, expected);
}

#[test]
fn fxsave_fxrstor_legacy_roundtrip_restores_state() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE);

    state.fpu.fcw = 0x1111;
    state.fpu.fsw = 0x2222 & !FSW_TOP_MASK;
    state.fpu.top = 6;
    state.fpu.ftw = 0x0F;
    state.fpu.fop = 0x3333;
    state.fpu.fip = 0x4444_5555;
    state.fpu.fcs = 0x6666;
    state.fpu.fdp = 0x7777_8888;
    state.fpu.fds = 0x9999;

    state.sse.mxcsr = 0x1F80;

    for i in 0..8 {
        state.fpu.st[i] = patterned_st80(0x40 + i as u8);
        state.sse.xmm[i] = patterned_u128(0xA0 + i as u8);
    }
    for i in 8..16 {
        state.sse.xmm[i] = patterned_u128(0xC0 + i as u8);
    }
    let original_fpu = state.fpu.clone();
    let original_low_xmm = state.sse.xmm[0..8].to_vec();

    let mut bus = FlatTestBus::new(BUS_SIZE);
    // fxsave [rax]; fxrstor [rax]
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x00, 0x0F, 0xAE, 0x08]);

    // Save.
    step_ok(&mut state, &mut bus);

    // Clobber the state (including upper XMM registers, which legacy FXRSTOR should NOT restore).
    state.fpu.fcw = 0;
    state.fpu.fsw = 0;
    state.fpu.top = 0;
    state.fpu.ftw = 0;
    state.fpu.fop = 0;
    state.fpu.fip = 0;
    state.fpu.fcs = 0;
    state.fpu.fdp = 0;
    state.fpu.fds = 0;
    state.fpu.st = [0u128; 8];
    state.sse.mxcsr = 0;
    state.sse.xmm[0..8].fill(0);
    for i in 8..16 {
        state.sse.xmm[i] = patterned_u128(0xD0 + i as u8);
    }
    let clobbered_upper_xmm = state.sse.xmm[8..16].to_vec();

    // Restore.
    step_ok(&mut state, &mut bus);

    assert_eq!(state.fpu, original_fpu);
    assert_eq!(&state.sse.xmm[0..8], &original_low_xmm[..]);
    assert_eq!(&state.sse.xmm[8..16], &clobbered_upper_xmm[..]);
}

#[test]
fn fxsave64_fxrstor64_roundtrip_restores_state() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE);

    state.fpu.fcw = 0xABCD;
    state.fpu.fsw = 0x7777 & !FSW_TOP_MASK;
    state.fpu.top = 7;
    state.fpu.ftw = 0x55;
    state.fpu.fop = 0x0BAD;
    state.fpu.fip = 0x1122_3344_5566_7788;
    state.fpu.fdp = 0x99AA_BBCC_DDEE_FF00;
    state.fpu.fcs = 0x1111;
    state.fpu.fds = 0x2222;

    state.sse.mxcsr = 0x1F80;

    for i in 0..8 {
        state.fpu.st[i] = patterned_st80(0x40 + i as u8);
    }
    for i in 0..16 {
        state.sse.xmm[i] = patterned_u128(0xC0 + i as u8);
    }

    let original_fpu = state.fpu.clone();
    let original_xmm = state.sse.xmm.to_vec();

    let mut bus = FlatTestBus::new(BUS_SIZE);
    // fxsave64 [rax]; fxrstor64 [rax]
    bus.load(CODE_BASE, &[0x48, 0x0F, 0xAE, 0x00, 0x48, 0x0F, 0xAE, 0x08]);

    step_ok(&mut state, &mut bus);

    // Change the non-restored legacy fields (FCS/FDS) to ensure FXRSTOR64 doesn't touch them.
    state.fpu.fcs = 0xAAAA;
    state.fpu.fds = 0xBBBB;
    // Clobber the rest of the image-covered state.
    state.fpu.fcw = 0;
    state.fpu.fsw = 0;
    state.fpu.top = 0;
    state.fpu.ftw = 0;
    state.fpu.fop = 0;
    state.fpu.fip = 0;
    state.fpu.fdp = 0;
    state.fpu.st = [0u128; 8];
    state.sse.mxcsr = 0;
    state.sse.xmm.fill(0);

    step_ok(&mut state, &mut bus);

    assert_eq!(state.fpu.fcw, original_fpu.fcw);
    assert_eq!(state.fpu.fsw, original_fpu.fsw);
    assert_eq!(state.fpu.top, original_fpu.top);
    assert_eq!(state.fpu.ftw, original_fpu.ftw);
    assert_eq!(state.fpu.fop, original_fpu.fop);
    assert_eq!(state.fpu.fip, original_fpu.fip);
    assert_eq!(state.fpu.fdp, original_fpu.fdp);
    assert_eq!(state.fpu.st, original_fpu.st);
    assert_eq!(state.sse.mxcsr, 0x1F80);
    assert_eq!(&state.sse.xmm[..], &original_xmm[..]);

    // FCS/FDS are not part of the 64-bit image.
    assert_eq!(state.fpu.fcs, 0xAAAA);
    assert_eq!(state.fpu.fds, 0xBBBB);
}

#[test]
fn fxrstor_rejects_reserved_mxcsr_without_modifying_state() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE);

    state.fpu.fcw = 0x1234;
    state.sse.mxcsr = 0x1F80;
    state.sse.xmm[0] = patterned_u128(0xAA);
    let snapshot_fpu = state.fpu.clone();
    let snapshot_sse = state.sse;

    let mut image = [0u8; FXSAVE_AREA_SIZE];
    state.fxsave32(&mut image);

    // Set a reserved MXCSR bit (outside MXCSR_MASK).
    image[24..28].copy_from_slice(&(MXCSR_MASK | (1 << 31)).to_le_bytes());

    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x08]); // fxrstor [rax]
    bus.load(DATA_BASE, &image);

    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));
    assert_eq!(state.fpu, snapshot_fpu);
    assert_eq!(state.sse, snapshot_sse);
}

#[test]
fn ldmxcsr_rejects_reserved_mxcsr_without_modifying_state() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE);
    state.sse.mxcsr = 0x1F80;

    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x10]); // ldmxcsr [rax]
    bus.write_u32(DATA_BASE, MXCSR_MASK | (1 << 31)).unwrap();

    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));
    assert_eq!(state.sse.mxcsr, 0x1F80);
}

#[test]
fn fx_instructions_enforce_control_register_gating() {
    // CR4.OSFXSR=0 => #UD
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        state.control.cr4 = 0;
        state.set_rip(CODE_BASE);
        state.write_reg(Register::RAX, DATA_BASE);

        let mut bus = FlatTestBus::new(BUS_SIZE);
        bus.load(CODE_BASE, &[0x0F, 0xAE, 0x00]); // fxsave [rax]

        assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));
    }

    // CR0.EM=1 => #UD
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        state.control.cr4 |= CR4_OSFXSR;
        state.control.cr0 |= CR0_EM;
        state.set_rip(CODE_BASE);
        state.write_reg(Register::RAX, DATA_BASE);

        let mut bus = FlatTestBus::new(BUS_SIZE);
        bus.load(CODE_BASE, &[0x0F, 0xAE, 0x00]); // fxsave [rax]

        assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));
    }

    // CR0.TS=1 => #NM
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        state.control.cr4 |= CR4_OSFXSR;
        state.control.cr0 |= CR0_TS;
        state.set_rip(CODE_BASE);
        state.write_reg(Register::RAX, DATA_BASE);

        let mut bus = FlatTestBus::new(BUS_SIZE);
        bus.load(CODE_BASE, &[0x0F, 0xAE, 0x00]); // fxsave [rax]

        assert_eq!(
            step(&mut state, &mut bus),
            Err(Exception::DeviceNotAvailable)
        );
    }
}

#[test]
fn fx_instructions_require_aligned_memory_operands() {
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);

    let mut bus = FlatTestBus::new(BUS_SIZE);

    // MXCSR load/store require 4-byte alignment.
    state.write_reg(Register::RAX, DATA_BASE + 1);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x18]); // stmxcsr [rax]
    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));

    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x10]); // ldmxcsr [rax]
    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));

    // FXSAVE/FXRSTOR require 16-byte alignment.
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE + 1);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x00]); // fxsave [rax]
    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));

    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x08]); // fxrstor [rax]
    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));
}

#[test]
fn fxrstor_masks_st_reserve_bits() {
    // Ensure that the non-architectural bytes in the ST/MM image are ignored by FXRSTOR.
    let mut state = CpuState::new(CpuMode::Bit64);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state.set_rip(CODE_BASE);
    state.write_reg(Register::RAX, DATA_BASE);

    let mut image = [0u8; FXSAVE_AREA_SIZE];
    image[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());
    for i in 0..8 {
        let start = 32 + i * 16;
        image[start..start + 16].copy_from_slice(&patterned_u128(0x10 + i as u8).to_le_bytes());
    }

    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0x08]); // fxrstor [rax]
    bus.load(DATA_BASE, &image);

    step_ok(&mut state, &mut bus);

    for i in 0..8 {
        assert_eq!(state.fpu.st[i], mask_st80(patterned_u128(0x10 + i as u8)));
    }
}

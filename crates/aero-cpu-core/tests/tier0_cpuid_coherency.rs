use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::cpuid::{
    bits, CpuFeatureOverrides, CpuFeatureSet, CpuFeatures, CpuProfile, CpuTopology,
};
use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::interrupts::CpuCore;
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::CpuMode;
use aero_cpu_core::CpuBus;
use aero_cpu_core::Exception;
use aero_x86::Register;

const BUS_SIZE: usize = 0x2000;
const CODE_BASE: u64 = 0x0000;
const CPUID_ECX_ADDR: u64 = 0x0500;
const CRC32_RESULT_ADDR: u64 = 0x0504;

fn coherency_program() -> Vec<u8> {
    // CPUID(EAX=1, ECX=0); store ECX; CRC32 EAX, ECX; store EAX; HLT
    vec![
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
        0x31, 0xC9, // xor ecx, ecx
        0x0F, 0xA2, // cpuid
        0x89, 0x0D, 0x00, 0x05, 0x00, 0x00, // mov [0x500], ecx
        0x31, 0xC0, // xor eax, eax (seed = 0)
        0xB9, 0x78, 0x56, 0x34, 0x12, // mov ecx, 0x12345678
        0xF2, 0x0F, 0x38, 0xF1, 0xC1, // crc32 eax, ecx
        0xA3, 0x04, 0x05, 0x00, 0x00, // mov [0x504], eax
        0xF4, // hlt
    ]
}

fn make_cpu() -> CpuCore {
    let mut cpu = CpuCore::new(CpuMode::Bit32);
    cpu.state.set_rip(CODE_BASE);
    cpu.state.set_rflags(0x0002);
    // Ensure CPL0 so HLT is permitted.
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu
}

fn features_optimized(overrides: CpuFeatureOverrides) -> CpuFeatures {
    CpuFeatures::from_profile(
        CpuProfile::Optimized,
        CpuFeatureSet::optimized_mask(),
        overrides,
        CpuTopology::default(),
    )
    .expect("cpu feature profile must be valid")
}

#[test]
fn disabling_sse42_in_cpuid_makes_tier0_crc32_ud() {
    let features = features_optimized(CpuFeatureOverrides {
        force_disable: CpuFeatureSet {
            leaf1_ecx: bits::LEAF1_ECX_SSE42,
            ..CpuFeatureSet::default()
        },
        ..CpuFeatureOverrides::default()
    });

    let mut ctx = AssistContext {
        features,
        ..AssistContext::default()
    };
    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &coherency_program());
    let mut cpu = make_cpu();

    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 64);
    assert_eq!(res.exit, BatchExit::Exception(Exception::InvalidOpcode));

    let leaf1_ecx = bus.read_u32(CPUID_ECX_ADDR).unwrap();
    assert_eq!(leaf1_ecx & bits::LEAF1_ECX_SSE42, 0);
}

#[test]
fn enabling_sse42_in_cpuid_allows_tier0_crc32() {
    let features = features_optimized(CpuFeatureOverrides::default());

    let mut ctx = AssistContext {
        features,
        ..AssistContext::default()
    };
    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &coherency_program());
    let mut cpu = make_cpu();

    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 64);
    assert_eq!(res.exit, BatchExit::Halted);

    let leaf1_ecx = bus.read_u32(CPUID_ECX_ADDR).unwrap();
    assert_ne!(leaf1_ecx & bits::LEAF1_ECX_SSE42, 0);

    let crc32 = bus.read_u32(CRC32_RESULT_ADDR).unwrap();
    let expected = aero_cpu_core::interp::sse42::crc32_u32(0, 0x1234_5678);
    assert_eq!(crc32, expected);
}

fn run_to_halt<B: CpuBus>(cpu: &mut Vcpu<B>, interp: &mut Tier0Interpreter, max_iters: u64) {
    for _ in 0..max_iters {
        if cpu.exit.is_some() {
            panic!("unexpected CPU exit: {:?}", cpu.exit);
        }
        if cpu.cpu.state.halted {
            return;
        }
        interp.exec_block(cpu);
    }
    panic!("program did not halt");
}

#[test]
fn exec_tier0_interpreter_uses_same_cpuid_policy_for_gating() {
    let features = features_optimized(CpuFeatureOverrides::default());

    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &coherency_program());

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.set_rip(CODE_BASE);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;

    // Ensure we query CPUID leaf 1, then execute CRC32.
    cpu.cpu.state.write_reg(Register::EAX, 1);
    cpu.cpu.state.write_reg(Register::ECX, 0);

    let mut interp = Tier0Interpreter::new(1024);
    interp.assist.features = features;

    run_to_halt(&mut cpu, &mut interp, 32);
    assert!(cpu.cpu.state.halted);

    let leaf1_ecx = cpu.bus.read_u32(CPUID_ECX_ADDR).unwrap();
    assert_ne!(leaf1_ecx & bits::LEAF1_ECX_SSE42, 0);
    assert_eq!(
        cpu.bus.read_u32(CRC32_RESULT_ADDR).unwrap(),
        aero_cpu_core::interp::sse42::crc32_u32(0, 0x1234_5678)
    );
}

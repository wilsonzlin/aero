use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::CpuBus;
use aero_cpu_core::state::{CpuMode, CpuState, RFLAGS_IF};
use aero_x86::Register;

const BUS_SIZE: usize = 0x10000;
const CODE_BASE: u64 = 0x0700;
const STACK_TOP: u64 = 0x9000;
const RETURN_IP: u32 = 0xDEAD_BEEF;

#[test]
fn tier0_assists_execute_cpuid_msr_tsc_and_interrupt_flag_ops() {
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // A small protected-mode (32-bit) snippet that exercises Tier-0 assists:
    // - CPUID
    // - WRMSR/RDMSR roundtrip (IA32_TSC)
    // - RDTSC
    // - CLI/STI
    //
    // The snippet stores its observable outputs into memory for assertions.
    let code: Vec<u8> = vec![
        0x31, 0xC0, // xor eax,eax
        0x31, 0xC9, // xor ecx,ecx
        0x0F, 0xA2, // cpuid
        0xA3, 0x00, 0x05, 0x00, 0x00, // mov [0x500], eax
        0x89, 0x1D, 0x04, 0x05, 0x00, 0x00, // mov [0x504], ebx
        0x89, 0x0D, 0x08, 0x05, 0x00, 0x00, // mov [0x508], ecx
        0x89, 0x15, 0x0C, 0x05, 0x00, 0x00, // mov [0x50C], edx
        0xB9, 0x10, 0x00, 0x00, 0x00, // mov ecx, 0x10 (IA32_TSC)
        0xB8, 0xF0, 0xDE, 0xBC, 0x9A, // mov eax, 0x9ABC_DEF0
        0xBA, 0x78, 0x56, 0x34, 0x12, // mov edx, 0x1234_5678
        0x0F, 0x30, // wrmsr
        0xB9, 0x10, 0x00, 0x00, 0x00, // mov ecx, 0x10
        0x0F, 0x32, // rdmsr
        0xA3, 0x10, 0x05, 0x00, 0x00, // mov [0x510], eax
        0x89, 0x15, 0x14, 0x05, 0x00, 0x00, // mov [0x514], edx
        0x0F, 0x31, // rdtsc
        0xA3, 0x18, 0x05, 0x00, 0x00, // mov [0x518], eax
        0x89, 0x15, 0x1C, 0x05, 0x00, 0x00, // mov [0x51C], edx
        0xFA, // cli
        0xFB, // sti
        0xC3, // ret
    ];
    bus.load(CODE_BASE, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(CODE_BASE);

    // Set up a near return address so the snippet can stop via `ret`.
    let sp_pushed = STACK_TOP - 4;
    bus.write_u32(sp_pushed, RETURN_IP).expect("stack write");
    state.write_reg(Register::ESP, sp_pushed);

    let mut ctx = AssistContext::default();

    let mut executed_total = 0u64;
    loop {
        if state.rip() == RETURN_IP as u64 {
            break;
        }
        let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1024);
        executed_total += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => panic!("unexpected HLT at rip=0x{:X}", state.rip()),
            BatchExit::BiosInterrupt(vector) => {
                panic!("unexpected BIOS interrupt {vector:#x} at rip=0x{:X}", state.rip())
            }
            BatchExit::Assist(r) => panic!("unexpected unhandled assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception after {executed_total} insts: {e:?}"),
        }
    }

    // CPUID leaf 0 result should match the deterministic policy in `cpuid.rs`.
    assert_eq!(bus.read_u32(0x500).unwrap(), 0x1F);
    assert_eq!(bus.read_u32(0x504).unwrap(), u32::from_le_bytes(*b"Genu"));
    assert_eq!(bus.read_u32(0x50C).unwrap(), u32::from_le_bytes(*b"ineI"));
    assert_eq!(bus.read_u32(0x508).unwrap(), u32::from_le_bytes(*b"ntel"));

    // RDMSR should roundtrip the IA32_TSC value we wrote.
    assert_eq!(bus.read_u32(0x510).unwrap(), 0x9ABC_DEF0);
    assert_eq!(bus.read_u32(0x514).unwrap(), 0x1234_5678);

    // RDTSC should be at least the written value (the deterministic model may
    // advance the counter after reads).
    let rdtsc_lo = bus.read_u32(0x518).unwrap() as u64;
    let rdtsc_hi = bus.read_u32(0x51C).unwrap() as u64;
    let rdtsc = (rdtsc_hi << 32) | rdtsc_lo;
    assert!(rdtsc >= 0x1234_5678_9ABC_DEF0);

    // CLI/STI should leave IF set.
    assert!(state.get_flag(RFLAGS_IF));
}

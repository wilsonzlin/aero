use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::cpuid::{cpuid, CpuFeatures};
use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::msr;
use aero_cpu_core::state::{
    CpuMode, CpuState, CR0_PE, RFLAGS_IF, RFLAGS_IOPL_MASK, RFLAGS_RESERVED1,
};
use aero_cpu_core::{AssistReason, Exception};
use aero_x86::Register;

const BUS_SIZE: usize = 0x4000;
const CODE_BASE: u64 = 0x1000;

trait LoadableBus: CpuBus {
    fn load(&mut self, addr: u64, data: &[u8]);
}

impl LoadableBus for FlatTestBus {
    fn load(&mut self, addr: u64, data: &[u8]) {
        FlatTestBus::load(self, addr, data);
    }
}

#[derive(Debug)]
struct IoBus {
    inner: FlatTestBus,
    reads: Vec<(u16, u32)>,
    writes: Vec<(u16, u32, u64)>,
    next_read: u64,
}

impl IoBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            reads: Vec::new(),
            writes: Vec::new(),
            next_read: 0,
        }
    }
}

impl CpuBus for IoBus {
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
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.reads.push((port, size));
        Ok(self.next_read)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.writes.push((port, size, val));
        Ok(())
    }
}

impl LoadableBus for IoBus {
    fn load(&mut self, addr: u64, data: &[u8]) {
        self.inner.load(addr, data);
    }
}

fn exec_assist<B: LoadableBus>(
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    rip: u64,
    bytes: &[u8],
    reason: AssistReason,
) -> Result<(), Exception> {
    state.set_rip(rip);
    bus.load(rip, bytes);
    aero_cpu_core::assist::handle_assist(ctx, state, bus, reason)
}

fn exec_wrmsr<B: LoadableBus>(
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    msr_index: u32,
    value: u64,
) {
    state.write_reg(Register::ECX, msr_index as u64);
    state.write_reg(Register::EAX, value as u32 as u64);
    state.write_reg(Register::EDX, (value >> 32) as u32 as u64);
    exec_assist(ctx, state, bus, CODE_BASE, &[0x0F, 0x30], AssistReason::Msr).unwrap();
}

fn exec_rdmsr<B: LoadableBus>(
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    msr_index: u32,
) -> Result<u64, Exception> {
    state.write_reg(Register::ECX, msr_index as u64);
    exec_assist(ctx, state, bus, CODE_BASE, &[0x0F, 0x32], AssistReason::Msr)?;
    let lo = state.read_reg(Register::EAX) as u32 as u64;
    let hi = state.read_reg(Register::EDX) as u32 as u64;
    Ok((hi << 32) | lo)
}

#[test]
fn cpuid_leafs_are_deterministic() {
    let features = CpuFeatures::default();

    let leaf0 = cpuid(&features, 0, 0);
    assert_eq!(leaf0.eax, 0x1F);
    assert_eq!(leaf0.ebx, u32::from_le_bytes(*b"Genu"));
    assert_eq!(leaf0.edx, u32::from_le_bytes(*b"ineI"));
    assert_eq!(leaf0.ecx, u32::from_le_bytes(*b"ntel"));

    let leaf1 = cpuid(&features, 1, 0);
    assert_eq!(leaf1.eax, features.leaf1_eax);
    assert_eq!(leaf1.ebx, features.leaf1_ebx);
    assert_eq!(leaf1.ecx, features.leaf1_ecx);
    assert_eq!(leaf1.edx, features.leaf1_edx);

    let leaf7 = cpuid(&features, 7, 0);
    assert_eq!(leaf7.ebx, features.leaf7_ebx);
    assert_eq!(leaf7.ecx, features.leaf7_ecx);
    assert_eq!(leaf7.edx, features.leaf7_edx);

    // Leaf 2 is a fixed QEMU-like cache/TLB descriptor set (not performance critical).
    let leaf2 = cpuid(&features, 2, 0);
    assert_eq!(leaf2.eax, 0x7603_6301);

    let ext_max = cpuid(&features, 0x8000_0000, 0);
    assert_eq!(ext_max.eax, 0x8000_0008);

    let ext1 = cpuid(&features, 0x8000_0001, 0);
    assert_eq!(ext1.ecx, features.ext1_ecx);
    assert_eq!(ext1.edx, features.ext1_edx);

    let ext7 = cpuid(&features, 0x8000_0007, 0);
    assert_eq!(ext7.edx, features.ext7_edx);
    assert_ne!(ext7.edx & (1 << 8), 0, "expected invariant TSC bit");

    let addr = cpuid(&features, 0x8000_0008, 0);
    assert_eq!(addr.eax & 0xFF, 48);
    assert_eq!((addr.eax >> 8) & 0xFF, 48);
}

#[test]
fn msr_roundtrip_supported() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.segments.cs.selector = 0x08; // CPL0

    exec_wrmsr(
        &mut ctx,
        &mut state,
        &mut bus,
        msr::IA32_EFER,
        msr::EFER_SCE | msr::EFER_NXE,
    );
    assert_eq!(
        exec_rdmsr(&mut ctx, &mut state, &mut bus, msr::IA32_EFER).unwrap(),
        msr::EFER_SCE | msr::EFER_NXE
    );

    // Unknown MSRs raise #GP(0).
    let err = exec_rdmsr(&mut ctx, &mut state, &mut bus, 0xDEAD_BEEF).unwrap_err();
    assert_eq!(err, Exception::gp0());

    // IA32_APIC_BASE reset value has enable + BSP bits set.
    let apic_base = exec_rdmsr(&mut ctx, &mut state, &mut bus, msr::IA32_APIC_BASE).unwrap();
    assert_ne!(apic_base & (1 << 11), 0);
    assert_ne!(apic_base & (1 << 8), 0);
}

#[test]
fn syscall_sysret_transitions_privilege() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit64);

    // User mode starting state.
    state.segments.cs.selector = 0x33; // CPL3 (64-bit user code selector)
    state.segments.ss.selector = 0x2B; // CPL3 (user data selector)
    state.set_rip(0x1000);
    state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);

    // Configure syscall MSRs.
    state.msr.efer = msr::EFER_SCE;
    // STAR: kernel CS in bits 47:32, SYSRET base selector in bits 63:48.
    // SYSRET loads CS = base + 16, SS = base + 8.
    state.msr.star = ((0x08u64) << 32) | ((0x23u64) << 48);
    state.msr.lstar = 0xFFFF_8000_0000_0000;
    state.msr.fmask = RFLAGS_IF; // Mask IF on entry.

    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        0x1000,
        &[0x0F, 0x05], // SYSCALL
        AssistReason::Privileged,
    )
    .unwrap();

    assert_eq!(state.cpl(), 0);
    assert_eq!(state.segments.cs.selector, 0x08);
    assert_eq!(state.segments.ss.selector, 0x10);
    assert_eq!(state.read_reg(Register::RCX), 0x1002);
    assert_eq!(state.read_reg(Register::R11), RFLAGS_RESERVED1 | RFLAGS_IF);
    assert_eq!(state.rflags() & RFLAGS_IF, 0);
    assert_eq!(state.rip(), state.msr.lstar);

    // Return to user.
    state.write_reg(Register::R11, RFLAGS_RESERVED1 | RFLAGS_IF);
    state.write_reg(Register::RCX, 0x2000);
    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0x0F, 0x07], // SYSRET
        AssistReason::Privileged,
    )
    .unwrap();

    assert_eq!(state.cpl(), 3);
    assert_eq!(state.segments.cs.selector, 0x33);
    assert_eq!(state.segments.ss.selector, 0x2B);
    assert_eq!(state.rip(), 0x2000);
    assert_eq!(state.rflags() & RFLAGS_IF, RFLAGS_IF);
}

#[test]
fn sysenter_sysexit_transitions_32bit() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;

    // Start in user mode.
    state.segments.cs.selector = 0x23;
    state.segments.ss.selector = 0x2B;
    state.set_rip(0x1000);
    state.write_reg(Register::ESP, 0xDEAD_BEEF);

    // Configure SYSENTER MSRs.
    state.msr.sysenter_cs = 0x08;
    state.msr.sysenter_eip = 0xC000_1000;
    state.msr.sysenter_esp = 0xC000_2000;

    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0x0F, 0x34], // SYSENTER
        AssistReason::Privileged,
    )
    .unwrap();

    assert_eq!(state.cpl(), 0);
    assert_eq!(state.segments.cs.selector, 0x08);
    assert_eq!(state.segments.ss.selector, 0x10);
    assert_eq!(state.rip(), 0xC000_1000);
    assert_eq!(state.read_reg(Register::ESP), 0xC000_2000);

    // Setup return state for SYSEXIT.
    state.write_reg(Register::EDX, 0xB800_0000); // user EIP (EDX)
    state.write_reg(Register::ECX, 0x0012_3400); // user ESP (ECX)
    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0x0F, 0x35], // SYSEXIT
        AssistReason::Privileged,
    )
    .unwrap();

    assert_eq!(state.cpl(), 3);
    assert_eq!(state.segments.cs.selector, 0x1B); // 0x08 + 16 + RPL3
    assert_eq!(state.segments.ss.selector, 0x23); // 0x08 + 24 + RPL3
    assert_eq!(state.rip(), 0xB800_0000);
    assert_eq!(state.read_reg(Register::ESP), 0x0012_3400);
}

#[test]
fn privilege_violations_raise_gp() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.segments.cs.selector = 0x23; // CPL3

    // MOV to CR is privileged.
    state.write_reg(Register::EAX, 0x1234);
    let err = exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0x0F, 0x22, 0xD8], // mov cr3, eax
        AssistReason::Privileged,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());

    // RDMSR is privileged.
    state.write_reg(Register::ECX, msr::IA32_EFER as u64);
    let err = exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0x0F, 0x32], // RDMSR
        AssistReason::Msr,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());
}

#[test]
fn in_out_routes_to_port_io() {
    let mut bus = IoBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;

    state.segments.cs.selector = 0x23; // CPL3
                                       // Allow user I/O for the test by setting IOPL=3.
    state.set_rflags(RFLAGS_RESERVED1 | (3u64 << 12));

    state.write_reg(Register::DX, 0x3F8);
    bus.next_read = 0xAA;

    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xEC], // IN AL, DX
        AssistReason::Io,
    )
    .unwrap();
    assert_eq!(state.read_reg(Register::AL), 0xAA);
    assert_eq!(bus.reads, vec![(0x3F8, 1)]);

    state.write_reg(Register::AL, 0x55);
    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xEE], // OUT DX, AL
        AssistReason::Io,
    )
    .unwrap();
    assert_eq!(bus.writes, vec![(0x3F8, 1, 0x55)]);
}

#[test]
fn io_privilege_checks_raise_gp_and_do_not_touch_device() {
    let mut bus = IoBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;

    state.segments.cs.selector = 0x23; // CPL3
    state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF); // IOPL=0
    state.write_reg(Register::DX, 0x3F8);
    bus.next_read = 0xAA;

    let err = exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xEC], // IN AL, DX
        AssistReason::Io,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());
    assert_eq!(bus.reads, Vec::new());

    let err = exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xEE], // OUT DX, AL
        AssistReason::Io,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());
    assert_eq!(bus.writes, Vec::new());
}

#[test]
fn cli_sti_are_privileged_by_iopl() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;

    state.segments.cs.selector = 0x23; // CPL3
    state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF); // IOPL=0

    let err = exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xFA], // CLI
        AssistReason::Interrupt,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());

    let err = exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xFB], // STI
        AssistReason::Interrupt,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());

    // Raise IOPL so user mode is allowed to toggle IF.
    state.set_rflags((state.rflags() & !RFLAGS_IOPL_MASK) | (3u64 << 12));

    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xFA], // CLI
        AssistReason::Interrupt,
    )
    .unwrap();
    assert_eq!(state.rflags() & RFLAGS_IF, 0);

    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0xFB], // STI
        AssistReason::Interrupt,
    )
    .unwrap();
    assert_eq!(state.rflags() & RFLAGS_IF, RFLAGS_IF);
}

#[test]
fn real_mode_treats_privileged_checks_as_cpl0() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit16);

    // Real-mode CS values are not selectors; the low bits are not an RPL and may be non-zero.
    state.segments.cs.selector = 0x1235;

    // Privileged helpers should not raise #GP due to CS low bits in real mode.
    state.write_reg(Register::EAX, 0);
    exec_assist(
        &mut ctx,
        &mut state,
        &mut bus,
        CODE_BASE,
        &[0x0F, 0x22, 0xC0], // mov cr0, eax
        AssistReason::Privileged,
    )
    .unwrap();

    // RDMSR/WRMSR still require CPL0, which real mode provides.
    exec_wrmsr(
        &mut ctx,
        &mut state,
        &mut bus,
        msr::IA32_EFER,
        msr::EFER_SCE,
    );
    assert_eq!(
        exec_rdmsr(&mut ctx, &mut state, &mut bus, msr::IA32_EFER).unwrap() & msr::EFER_SCE,
        msr::EFER_SCE
    );

    // HLT is also privileged, but real mode has CPL=0.
    bus.load(CODE_BASE, &[0xF4]); // HLT
    state.set_rip(CODE_BASE);
    let exit = step(&mut state, &mut bus).expect("step");
    assert_eq!(exit, StepExit::Halted);
    assert!(state.halted);
}

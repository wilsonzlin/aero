use aero_cpu_core::cpuid::{cpuid, CpuFeatures};
use aero_cpu_core::msr;
use aero_cpu_core::system::{Cpu, CpuMode, PortIo};
use aero_cpu_core::Exception;

#[test]
fn cpuid_leafs_are_deterministic() {
    let features = CpuFeatures::default();

    let leaf0 = cpuid(&features, 0, 0);
    assert_eq!(leaf0.eax, 7);
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

    let ext_max = cpuid(&features, 0x8000_0000, 0);
    assert_eq!(ext_max.eax, 0x8000_0008);

    let ext1 = cpuid(&features, 0x8000_0001, 0);
    assert_eq!(ext1.ecx, features.ext1_ecx);
    assert_eq!(ext1.edx, features.ext1_edx);

    let addr = cpuid(&features, 0x8000_0008, 0);
    assert_eq!(addr.eax & 0xFF, 48);
    assert_eq!((addr.eax >> 8) & 0xFF, 48);
}

#[test]
fn msr_roundtrip_supported() {
    let mut cpu = Cpu::default();
    cpu.cs = 0x8; // CPL0

    cpu.wrmsr_value(msr::IA32_EFER, msr::EFER_SCE | msr::EFER_NXE)
        .unwrap();
    assert_eq!(
        cpu.rdmsr_value(msr::IA32_EFER).unwrap(),
        msr::EFER_SCE | msr::EFER_NXE
    );

    // Unknown MSRs raise #GP(0).
    let err = cpu.rdmsr_value(0xDEAD_BEEF).unwrap_err();
    assert_eq!(err, Exception::GeneralProtection(0));
}

#[test]
fn syscall_sysret_transitions_privilege() {
    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Long64;

    // User mode starting state.
    cpu.cs = 0x33; // CPL3 (64-bit user code selector)
    cpu.ss = 0x2B; // CPL3 (user data selector)
    cpu.rip = 0x1000;
    cpu.rflags = Cpu::RFLAGS_FIXED1 | Cpu::RFLAGS_IF;

    // Configure syscall MSRs.
    cpu.msr.efer = msr::EFER_SCE;
    // STAR: kernel CS in bits 47:32, SYSRET base selector in bits 63:48.
    // SYSRET loads CS = base + 16, SS = base + 8.
    cpu.msr.star = ((0x08u64) << 32) | ((0x23u64) << 48);
    cpu.msr.lstar = 0xFFFF_8000_0000_0000;
    cpu.msr.fmask = Cpu::RFLAGS_IF; // Mask IF on entry.

    cpu.syscall().unwrap();

    assert_eq!(cpu.cpl(), 0);
    assert_eq!(cpu.cs, 0x08);
    assert_eq!(cpu.ss, 0x10);
    assert_eq!(cpu.rcx, 0x1002);
    assert_eq!(cpu.r11, Cpu::RFLAGS_FIXED1 | Cpu::RFLAGS_IF);
    assert_eq!(cpu.rflags & Cpu::RFLAGS_IF, 0);
    assert_eq!(cpu.rip, cpu.msr.lstar);

    // Return to user.
    cpu.r11 = Cpu::RFLAGS_FIXED1 | Cpu::RFLAGS_IF;
    cpu.rcx = 0x2000;
    cpu.sysret().unwrap();

    assert_eq!(cpu.cpl(), 3);
    assert_eq!(cpu.cs, 0x33);
    assert_eq!(cpu.ss, 0x2B);
    assert_eq!(cpu.rip, 0x2000);
    assert_eq!(cpu.rflags & Cpu::RFLAGS_IF, Cpu::RFLAGS_IF);
}

#[test]
fn sysenter_sysexit_transitions_32bit() {
    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;

    // Start in user mode.
    cpu.cs = 0x23;
    cpu.ss = 0x2B;
    cpu.rip = 0x1000;
    cpu.rsp = 0xDEAD_BEEF;

    // Configure SYSENTER MSRs.
    cpu.msr.sysenter_cs = 0x08;
    cpu.msr.sysenter_eip = 0xC000_1000;
    cpu.msr.sysenter_esp = 0xC000_2000;

    cpu.sysenter().unwrap();
    assert_eq!(cpu.cpl(), 0);
    assert_eq!(cpu.cs, 0x08);
    assert_eq!(cpu.ss, 0x10);
    assert_eq!(cpu.rip, 0xC000_1000);
    assert_eq!(cpu.rsp, 0xC000_2000);

    // Setup return state for SYSEXIT.
    cpu.rcx = 0xB800_0000; // user EIP
    cpu.rdx = 0x0012_3400; // user ESP
    cpu.sysexit().unwrap();

    assert_eq!(cpu.cpl(), 3);
    assert_eq!(cpu.cs, 0x1B); // 0x08 + 16 + RPL3
    assert_eq!(cpu.ss, 0x23); // 0x08 + 24 + RPL3
    assert_eq!(cpu.rip, 0xB800_0000);
    assert_eq!(cpu.rsp, 0x0012_3400);
}

#[test]
fn privilege_violations_raise_gp() {
    let mut cpu = Cpu::default();
    cpu.cs = 0x23; // CPL3

    // MOV to CR is privileged.
    assert_eq!(cpu.mov_to_cr(3, 0x1234).unwrap_err(), Exception::gp0());

    // RDMSR is privileged.
    cpu.rcx = msr::IA32_EFER as u64;
    assert_eq!(cpu.instr_rdmsr().unwrap_err(), Exception::gp0());

    // HLT is privileged.
    assert_eq!(cpu.hlt().unwrap_err(), Exception::gp0());
}

struct MockPortIo {
    pub reads: Vec<(u16, u8)>,
    pub writes: Vec<(u16, u8, u32)>,
    pub next_read: u32,
}

impl PortIo for MockPortIo {
    fn port_read(&mut self, port: u16, size: u8) -> u32 {
        self.reads.push((port, size));
        self.next_read
    }

    fn port_write(&mut self, port: u16, size: u8, val: u32) {
        self.writes.push((port, size, val));
    }
}

#[test]
fn in_out_routes_to_port_io() {
    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.cs = 0x23; // CPL3
                   // Allow user I/O for the test by setting IOPL=3.
    cpu.rflags |= 3u64 << 12;

    let mut io = MockPortIo {
        reads: Vec::new(),
        writes: Vec::new(),
        next_read: 0xAA,
    };

    cpu.instr_in(0x3F8, 1, &mut io).unwrap();
    assert_eq!(cpu.rax & 0xFF, 0xAA);
    assert_eq!(io.reads, vec![(0x3F8, 1)]);

    cpu.rax = 0x55;
    cpu.instr_out(0x3F8, 1, &mut io).unwrap();
    assert_eq!(io.writes, vec![(0x3F8, 1, 0x55)]);
}

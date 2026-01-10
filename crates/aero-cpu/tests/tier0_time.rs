use aero_cpu::msr::{IA32_TSC, IA32_TSC_AUX};
use aero_cpu::tier0::{CpuMode, CpuState, EmuException, Machine, MemoryBus, PortIo, StepOutcome};
use iced_x86::Register;

#[derive(Clone, Debug)]
struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    fn load(&mut self, paddr: u64, bytes: &[u8]) {
        let start = paddr as usize;
        self.data[start..start + bytes.len()].copy_from_slice(bytes);
    }
}

impl MemoryBus for TestMem {
    fn read_u8(&mut self, paddr: u64) -> Result<u8, EmuException> {
        self.data
            .get(paddr as usize)
            .copied()
            .ok_or(EmuException::MemOutOfBounds(paddr))
    }

    fn write_u8(&mut self, paddr: u64, value: u8) -> Result<(), EmuException> {
        let Some(dst) = self.data.get_mut(paddr as usize) else {
            return Err(EmuException::MemOutOfBounds(paddr));
        };
        *dst = value;
        Ok(())
    }
}

#[derive(Default, Debug, Clone)]
struct TestPorts;

impl PortIo for TestPorts {
    fn in_u8(&mut self, _port: u16) -> u8 {
        0
    }

    fn out_u8(&mut self, _port: u16, _value: u8) {}
}

fn tsc_from_edx_eax(cpu: &CpuState) -> u64 {
    let eax = cpu.read_reg(Register::EAX).unwrap() as u32 as u64;
    let edx = cpu.read_reg(Register::EDX).unwrap() as u32 as u64;
    (edx << 32) | eax
}

#[test]
fn rdtsc_is_monotonic() {
    let code = [0x0F, 0x31, 0x0F, 0x31, 0xF4]; // rdtsc; rdtsc; hlt
    let mut mem = TestMem::new(0x1000);
    mem.load(0, &code);
    let cpu = CpuState::new(CpuMode::Protected);
    let mut machine = Machine::new(cpu, mem, TestPorts::default());

    machine.step().unwrap(); // RDTSC
    let tsc1 = tsc_from_edx_eax(&machine.cpu);

    machine.step().unwrap(); // RDTSC
    let tsc2 = tsc_from_edx_eax(&machine.cpu);

    assert!(tsc2 > tsc1, "expected monotonic TSC: {tsc2} <= {tsc1}");
}

#[test]
fn rdtscp_reads_tsc_aux_into_ecx() {
    let code = [0x0F, 0x01, 0xF9, 0xF4]; // rdtscp; hlt
    let mut mem = TestMem::new(0x1000);
    mem.load(0, &code);
    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.msr.insert(IA32_TSC_AUX, 0xAABB_CCDD);
    let mut machine = Machine::new(cpu, mem, TestPorts::default());

    machine.step().unwrap();
    assert_eq!(machine.cpu.read_reg(Register::ECX).unwrap() as u32, 0xAABB_CCDD);
}

#[test]
fn wrmsr_ia32_tsc_updates_subsequent_rdtsc() {
    let tsc = 0x1234_5678_9ABC_DEF0u64;
    let mut code = Vec::new();
    code.extend_from_slice(&[0xB9]); // mov ecx, imm32
    code.extend_from_slice(&IA32_TSC.to_le_bytes());
    code.extend_from_slice(&[0xB8]); // mov eax, imm32
    code.extend_from_slice(&(tsc as u32).to_le_bytes());
    code.extend_from_slice(&[0xBA]); // mov edx, imm32
    code.extend_from_slice(&((tsc >> 32) as u32).to_le_bytes());
    code.extend_from_slice(&[0x0F, 0x30]); // wrmsr
    code.extend_from_slice(&[0x0F, 0x31]); // rdtsc
    code.push(0xF4); // hlt

    let mut mem = TestMem::new(0x1000);
    mem.load(0, &code);
    let cpu = CpuState::new(CpuMode::Protected);
    let mut machine = Machine::new(cpu, mem, TestPorts::default());

    for _ in 0..5 {
        if machine.step().unwrap() == StepOutcome::Halted {
            break;
        }
    }

    let read = tsc_from_edx_eax(&machine.cpu);
    assert!(
        read >= tsc,
        "expected RDTSC to reflect IA32_TSC write: read={read:#x} expected>={tsc:#x}"
    );
}

#[test]
fn fences_and_pause_do_not_modify_registers() {
    let mut code = Vec::new();
    code.extend_from_slice(&[0xB8]); // mov eax, imm32
    code.extend_from_slice(&0x1111_2222u32.to_le_bytes());
    code.extend_from_slice(&[0xBB]); // mov ebx, imm32
    code.extend_from_slice(&0x3333_4444u32.to_le_bytes());
    code.extend_from_slice(&[0x0F, 0xAE, 0xE8]); // lfence
    code.extend_from_slice(&[0x0F, 0xAE, 0xF8]); // sfence
    code.extend_from_slice(&[0x0F, 0xAE, 0xF0]); // mfence
    code.extend_from_slice(&[0xF3, 0x90]); // pause
    code.push(0xF4); // hlt

    let mut mem = TestMem::new(0x1000);
    mem.load(0, &code);
    let cpu = CpuState::new(CpuMode::Protected);
    let mut machine = Machine::new(cpu, mem, TestPorts::default());

    let mut halted = false;
    for _ in 0..32 {
        match machine.step().unwrap() {
            StepOutcome::Continue => {}
            StepOutcome::Halted => {
                halted = true;
                break;
            }
        }
    }
    assert!(halted);

    assert_eq!(machine.cpu.read_reg(Register::EAX).unwrap() as u32, 0x1111_2222);
    assert_eq!(machine.cpu.read_reg(Register::EBX).unwrap() as u32, 0x3333_4444);
}

#[test]
fn cpuid_advertises_rdtscp_and_invariant_tsc() {
    let mut code = Vec::new();
    // mov ecx, 0
    code.extend_from_slice(&[0xB9, 0x00, 0x00, 0x00, 0x00]);
    // mov eax, 0x8000_0000; cpuid
    code.extend_from_slice(&[0xB8]);
    code.extend_from_slice(&0x8000_0000u32.to_le_bytes());
    code.extend_from_slice(&[0x0F, 0xA2]);
    // mov eax, 0x8000_0001; cpuid
    code.extend_from_slice(&[0xB8]);
    code.extend_from_slice(&0x8000_0001u32.to_le_bytes());
    code.extend_from_slice(&[0x0F, 0xA2]);
    // mov eax, 0x8000_0007; cpuid
    code.extend_from_slice(&[0xB8]);
    code.extend_from_slice(&0x8000_0007u32.to_le_bytes());
    code.extend_from_slice(&[0x0F, 0xA2]);
    code.push(0xF4); // hlt

    let mut mem = TestMem::new(0x1000);
    mem.load(0, &code);
    let cpu = CpuState::new(CpuMode::Protected);
    let mut machine = Machine::new(cpu, mem, TestPorts::default());

    // Step through: mov ecx, 0
    machine.step().unwrap();

    // CPUID(0x8000_0000)
    machine.step().unwrap(); // mov eax, leaf
    machine.step().unwrap(); // cpuid
    assert_eq!(
        machine.cpu.read_reg(Register::EAX).unwrap() as u32,
        0x8000_0007
    );

    // CPUID(0x8000_0001)
    machine.step().unwrap(); // mov eax, leaf
    machine.step().unwrap(); // cpuid
    let edx = machine.cpu.read_reg(Register::EDX).unwrap() as u32;
    assert_ne!(edx & (1 << 27), 0, "expected RDTSCP feature bit");

    // CPUID(0x8000_0007)
    machine.step().unwrap(); // mov eax, leaf
    machine.step().unwrap(); // cpuid
    let edx = machine.cpu.read_reg(Register::EDX).unwrap() as u32;
    assert_ne!(edx & (1 << 8), 0, "expected invariant TSC bit");
}

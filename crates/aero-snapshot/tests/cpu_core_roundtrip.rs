use std::io::Cursor;

use aero_cpu_core::interp::tier0;
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{gpr as core_gpr, CpuMode as CoreCpuMode, CpuState as CoreCpuState};
use aero_cpu_core::Exception;
use aero_snapshot::{
    apply_cpu_state_to_cpu_core, apply_mmu_state_to_cpu_core, cpu_core_from_snapshot,
    cpu_state_from_cpu_core, mmu_state_from_cpu_core, restore_snapshot, save_snapshot,
    CpuInternalState, CpuState, MmuState, SaveOptions, SnapshotMeta, SnapshotSource,
    SnapshotTarget,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn assert_core_state_eq(a: &CoreCpuState, b: &CoreCpuState) {
    fn assert_seg_eq(
        name: &str,
        a: &aero_cpu_core::state::Segment,
        b: &aero_cpu_core::state::Segment,
    ) {
        assert_eq!(a.selector, b.selector, "{name} selector differs");
        assert_eq!(a.base, b.base, "{name} base differs");
        assert_eq!(a.limit, b.limit, "{name} limit differs");
        assert_eq!(a.access, b.access, "{name} access differs");
    }

    assert_eq!(a.gpr, b.gpr, "gprs differ");
    assert_eq!(a.rip, b.rip, "rip differs");
    assert_eq!(a.rflags_snapshot(), b.rflags_snapshot(), "rflags differ");
    assert_eq!(a.mode, b.mode, "mode differs");
    assert_eq!(a.halted, b.halted, "halted differs");
    assert_eq!(
        a.pending_bios_int, b.pending_bios_int,
        "pending_bios_int differs"
    );
    assert_eq!(
        a.pending_bios_int_valid, b.pending_bios_int_valid,
        "pending_bios_int_valid differs"
    );
    assert_eq!(a.a20_enabled, b.a20_enabled, "a20_enabled differs");
    assert_eq!(a.irq13_pending, b.irq13_pending, "irq13_pending differs");

    assert_seg_eq("cs", &a.segments.cs, &b.segments.cs);
    assert_seg_eq("ds", &a.segments.ds, &b.segments.ds);
    assert_seg_eq("es", &a.segments.es, &b.segments.es);
    assert_seg_eq("fs", &a.segments.fs, &b.segments.fs);
    assert_seg_eq("gs", &a.segments.gs, &b.segments.gs);
    assert_seg_eq("ss", &a.segments.ss, &b.segments.ss);

    assert_eq!(a.tables.gdtr.base, b.tables.gdtr.base, "gdtr base");
    assert_eq!(a.tables.gdtr.limit, b.tables.gdtr.limit, "gdtr limit");
    assert_eq!(a.tables.idtr.base, b.tables.idtr.base, "idtr base");
    assert_eq!(a.tables.idtr.limit, b.tables.idtr.limit, "idtr limit");
    assert_seg_eq("ldtr", &a.tables.ldtr, &b.tables.ldtr);
    assert_seg_eq("tr", &a.tables.tr, &b.tables.tr);

    assert_eq!(a.control.cr0, b.control.cr0, "cr0 differs");
    assert_eq!(a.control.cr2, b.control.cr2, "cr2 differs");
    assert_eq!(a.control.cr3, b.control.cr3, "cr3 differs");
    assert_eq!(a.control.cr4, b.control.cr4, "cr4 differs");
    assert_eq!(a.control.cr8, b.control.cr8, "cr8 differs");

    assert_eq!(a.debug.dr, b.debug.dr, "dr0-3 differs");
    assert_eq!(a.debug.dr6, b.debug.dr6, "dr6 differs");
    assert_eq!(a.debug.dr7, b.debug.dr7, "dr7 differs");

    assert_eq!(a.msr.efer, b.msr.efer, "msr.efer differs");
    assert_eq!(a.msr.star, b.msr.star, "msr.star differs");
    assert_eq!(a.msr.lstar, b.msr.lstar, "msr.lstar differs");
    assert_eq!(a.msr.cstar, b.msr.cstar, "msr.cstar differs");
    assert_eq!(a.msr.fmask, b.msr.fmask, "msr.fmask differs");
    assert_eq!(
        a.msr.sysenter_cs, b.msr.sysenter_cs,
        "msr.sysenter_cs differs"
    );
    assert_eq!(
        a.msr.sysenter_eip, b.msr.sysenter_eip,
        "msr.sysenter_eip differs"
    );
    assert_eq!(
        a.msr.sysenter_esp, b.msr.sysenter_esp,
        "msr.sysenter_esp differs"
    );
    assert_eq!(a.msr.fs_base, b.msr.fs_base, "msr.fs_base differs");
    assert_eq!(a.msr.gs_base, b.msr.gs_base, "msr.gs_base differs");
    assert_eq!(
        a.msr.kernel_gs_base, b.msr.kernel_gs_base,
        "msr.kernel_gs_base differs"
    );
    assert_eq!(a.msr.apic_base, b.msr.apic_base, "msr.apic_base differs");
    assert_eq!(a.msr.tsc, b.msr.tsc, "msr.tsc differs");
    assert_eq!(a.fpu, b.fpu, "fpu differs");
    assert_eq!(a.sse, b.sse, "sse differs");
}

fn random_core_state(rng: &mut impl Rng) -> CoreCpuState {
    let mut core = CoreCpuState::default();
    for reg in &mut core.gpr {
        *reg = rng.gen();
    }
    core.rip = rng.gen();
    core.set_rflags(rng.gen());
    core.mode = match rng.gen_range(0..4) {
        0 => CoreCpuMode::Real,
        1 => CoreCpuMode::Protected,
        2 => CoreCpuMode::Long,
        _ => CoreCpuMode::Vm86,
    };
    core.halted = rng.gen();
    core.pending_bios_int = rng.gen();
    core.pending_bios_int_valid = rng.gen();
    core.a20_enabled = rng.gen();
    core.irq13_pending = rng.gen();

    fn fill_seg(rng: &mut impl Rng, s: &mut aero_cpu_core::state::Segment) {
        s.selector = rng.gen();
        s.base = rng.gen();
        s.limit = rng.gen();
        s.access = rng.gen();
    }

    fill_seg(rng, &mut core.segments.es);
    fill_seg(rng, &mut core.segments.cs);
    fill_seg(rng, &mut core.segments.ss);
    fill_seg(rng, &mut core.segments.ds);
    fill_seg(rng, &mut core.segments.fs);
    fill_seg(rng, &mut core.segments.gs);

    core.tables.gdtr.base = rng.gen();
    core.tables.gdtr.limit = rng.gen();
    core.tables.idtr.base = rng.gen();
    core.tables.idtr.limit = rng.gen();
    fill_seg(rng, &mut core.tables.ldtr);
    fill_seg(rng, &mut core.tables.tr);

    core.control.cr0 = rng.gen();
    core.control.cr2 = rng.gen();
    core.control.cr3 = rng.gen();
    core.control.cr4 = rng.gen();
    core.control.cr8 = rng.gen();

    for dr in &mut core.debug.dr {
        *dr = rng.gen();
    }
    core.debug.dr6 = rng.gen();
    core.debug.dr7 = rng.gen();

    core.msr.efer = rng.gen();
    core.msr.star = rng.gen();
    core.msr.lstar = rng.gen();
    core.msr.cstar = rng.gen();
    core.msr.fmask = rng.gen();
    core.msr.sysenter_cs = rng.gen();
    core.msr.sysenter_eip = rng.gen();
    core.msr.sysenter_esp = rng.gen();
    core.msr.fs_base = rng.gen();
    core.msr.gs_base = rng.gen();
    core.msr.kernel_gs_base = rng.gen();
    core.msr.apic_base = rng.gen();
    core.msr.tsc = rng.gen();

    core.fpu.fcw = rng.gen();
    core.fpu.fsw = rng.gen();
    core.fpu.ftw = rng.gen();
    core.fpu.top = rng.gen_range(0..8);
    core.fpu.fop = rng.gen();
    core.fpu.fip = rng.gen();
    core.fpu.fdp = rng.gen();
    core.fpu.fcs = rng.gen();
    core.fpu.fds = rng.gen();
    for st in &mut core.fpu.st {
        *st = rng.gen();
    }

    core.sse.mxcsr = rng.gen();
    for xmm in &mut core.sse.xmm {
        *xmm = rng.gen();
    }

    core
}

#[test]
fn cpu_core_roundtrips_through_v2_cpu_mmu_sections() {
    let mut rng = StdRng::seed_from_u64(0xD00D_F00D);

    for _ in 0..128 {
        let core = random_core_state(&mut rng);
        let mut expected = core.clone();
        expected.commit_lazy_flags();

        let (cpu, mmu) = aero_snapshot::snapshot_from_cpu_core(&core);

        let mut cpu_bytes = Vec::new();
        cpu.encode_v2(&mut cpu_bytes).unwrap();
        let cpu2 = CpuState::decode_v2(&mut Cursor::new(&cpu_bytes)).unwrap();
        assert_eq!(cpu, cpu2);

        let mut mmu_bytes = Vec::new();
        mmu.encode_v2(&mut mmu_bytes).unwrap();
        let mmu2 = MmuState::decode_v2(&mut Cursor::new(&mmu_bytes)).unwrap();
        assert_eq!(mmu, mmu2);

        let restored = cpu_core_from_snapshot(&cpu2, &mmu2);
        assert_core_state_eq(&expected, &restored);
    }
}

#[derive(Clone)]
struct CoreHarness {
    cpu: CoreCpuState,
    mem: Vec<u8>,
    cpu_internal: CpuInternalState,
    next_snapshot_id: u64,
}

impl CoreHarness {
    fn new(cpu: CoreCpuState, mem_len: usize) -> Self {
        Self {
            cpu,
            mem: vec![0u8; mem_len],
            cpu_internal: CpuInternalState::default(),
            next_snapshot_id: 1,
        }
    }

    fn load(&mut self, addr: u64, bytes: &[u8]) {
        let start = addr as usize;
        let end = start + bytes.len();
        self.mem[start..end].copy_from_slice(bytes);
    }
}

struct FlatBus<'a> {
    mem: &'a mut [u8],
}

impl<'a> FlatBus<'a> {
    fn new(mem: &'a mut [u8]) -> Self {
        Self { mem }
    }
}

impl CpuBus for FlatBus<'_> {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.mem
            .get(vaddr as usize)
            .copied()
            .ok_or(Exception::MemoryFault)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let lo = self.read_u8(vaddr)? as u16;
        let hi = self.read_u8(vaddr + 1)? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.read_u8(vaddr + i)? as u32) << (i * 8);
        }
        Ok(v)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut v = 0u64;
        for i in 0..8 {
            v |= (self.read_u8(vaddr + i)? as u64) << (i * 8);
        }
        Ok(v)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut v = 0u128;
        for i in 0..16 {
            v |= (self.read_u8(vaddr + i)? as u128) << (i * 8);
        }
        Ok(v)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        let slot = self
            .mem
            .get_mut(vaddr as usize)
            .ok_or(Exception::MemoryFault)?;
        *slot = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_u8(vaddr, (val & 0xFF) as u8)?;
        self.write_u8(vaddr + 1, (val >> 8) as u8)?;
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        for i in 0..4 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        for i in 0..8 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        for i in 0..16 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = self.read_u8(vaddr + i as u64)?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}

impl SnapshotSource for CoreHarness {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        let id = self.next_snapshot_id;
        self.next_snapshot_id += 1;
        SnapshotMeta {
            snapshot_id: id,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        }
    }

    fn cpu_state(&self) -> CpuState {
        cpu_state_from_cpu_core(&self.cpu)
    }

    fn mmu_state(&self) -> MmuState {
        mmu_state_from_cpu_core(&self.cpu)
    }

    fn device_states(&self) -> Vec<aero_snapshot::DeviceState> {
        vec![self
            .cpu_internal
            .to_device_state()
            .expect("CpuInternalState encode cannot fail")]
    }

    fn disk_overlays(&self) -> aero_snapshot::DiskOverlayRefs {
        aero_snapshot::DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.mem.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset = offset as usize;
        buf.copy_from_slice(&self.mem[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl SnapshotTarget for CoreHarness {
    fn restore_cpu_state(&mut self, state: CpuState) {
        apply_cpu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_mmu_state(&mut self, state: MmuState) {
        apply_mmu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_device_states(&mut self, states: Vec<aero_snapshot::DeviceState>) {
        for state in states {
            if state.id == aero_snapshot::DeviceId::CPU_INTERNAL
                && state.version == CpuInternalState::VERSION
            {
                if let Ok(decoded) = CpuInternalState::from_device_state(&state) {
                    self.cpu_internal = decoded;
                }
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: aero_snapshot::DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.mem.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        let offset = offset as usize;
        self.mem[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

#[test]
fn cpu_core_execute_snapshot_restore_continue() {
    // Program (32-bit, protected mode) that uses FS base and flags across a snapshot boundary:
    //
    //   mov eax, fs:[0]
    //   cmp eax, 4
    //   jne else
    //   mov dword ptr fs:[8], 0x11111111
    //   jmp end
    // else:
    //   mov dword ptr fs:[8], 0x22222222
    // end:
    //   hlt
    //
    // Snapshot is taken after `cmp`, so resuming requires correct RFLAGS + FS base.
    const CODE: &[u8] = &[
        0x64, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // mov eax, fs:[0]
        0x83, 0xF8, 0x04, // cmp eax, 4
        0x75, 0x0D, // jne else (+0x0D)
        0x64, 0xC7, 0x05, 0x08, 0x00, 0x00, 0x00, 0x11, 0x11, 0x11,
        0x11, // mov fs:[8], 0x11111111
        0xEB, 0x0B, // jmp end (+0x0B)
        0x64, 0xC7, 0x05, 0x08, 0x00, 0x00, 0x00, 0x22, 0x22, 0x22,
        0x22, // mov fs:[8], 0x22222222
        0xF4, // hlt
    ];

    const MEM_LEN: usize = 0x4000;
    const FS_BASE: u64 = 0x2000;

    let mut cpu = CoreCpuState::new(CoreCpuMode::Protected);
    cpu.rip = 0;
    cpu.segments.cs.base = 0;
    cpu.segments.ds.base = 0;
    cpu.segments.es.base = 0;
    cpu.segments.ss.base = 0;
    cpu.segments.fs.base = FS_BASE;
    cpu.segments.gs.base = 0;

    let mut baseline = CoreHarness::new(cpu.clone(), MEM_LEN);
    baseline.load(0, CODE);
    baseline.load(FS_BASE, &4u32.to_le_bytes());

    // Baseline: run program to HLT without snapshots.
    while !baseline.cpu.halted {
        let (cpu, mem) = (&mut baseline.cpu, &mut baseline.mem);
        let mut bus = FlatBus::new(mem);
        tier0::exec::step(cpu, &mut bus).unwrap();
    }
    let expected_mem = baseline.mem.clone();

    // Snapshot path: execute first 2 instructions, snapshot, restore, continue.
    let mut snap_vm = CoreHarness::new(cpu, MEM_LEN);
    snap_vm.load(0, CODE);
    snap_vm.load(FS_BASE, &4u32.to_le_bytes());
    snap_vm.cpu_internal.interrupt_inhibit = 1;
    snap_vm.cpu_internal.pending_external_interrupts = vec![0x20, 0x21];

    for _ in 0..2 {
        let (cpu, mem) = (&mut snap_vm.cpu, &mut snap_vm.mem);
        let mut bus = FlatBus::new(mem);
        tier0::exec::step(cpu, &mut bus).unwrap();
        assert!(!snap_vm.cpu.halted);
    }

    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut snap_vm, SaveOptions::default()).unwrap();
    let bytes = cursor.into_inner();

    let mut resumed = CoreHarness::new(CoreCpuState::default(), MEM_LEN);
    restore_snapshot(&mut Cursor::new(&bytes), &mut resumed).unwrap();

    while !resumed.cpu.halted {
        let (cpu, mem) = (&mut resumed.cpu, &mut resumed.mem);
        let mut bus = FlatBus::new(mem);
        tier0::exec::step(cpu, &mut bus).unwrap();
    }

    assert_eq!(resumed.mem, expected_mem);
    assert_eq!(resumed.cpu_internal.interrupt_inhibit, 1);
    assert_eq!(
        resumed.cpu_internal.pending_external_interrupts,
        vec![0x20, 0x21]
    );
    assert_eq!(
        resumed.cpu.gpr[core_gpr::RAX] as u32,
        4,
        "eax should contain loaded value"
    );
    assert_eq!(
        u32::from_le_bytes(
            resumed.mem[(FS_BASE as usize + 8)..(FS_BASE as usize + 12)]
                .try_into()
                .unwrap()
        ),
        0x1111_1111,
        "branch should follow ZF=1 path after restore"
    );
}

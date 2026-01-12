use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::interrupts::{CpuCore, CpuExit};
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{gpr, CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::Exception;
use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::i8042::{I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_pc_platform::{PcCpuBus, PcPlatform};
use aero_platform::interrupts::InterruptInput;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;

fn write_u64(mem: &mut aero_platform::memory::MemoryBus, paddr: u64, value: u64) {
    mem.write_physical(paddr, &value.to_le_bytes());
}

fn setup_long4_4k(
    mem: &mut aero_platform::memory::MemoryBus,
    pml4_base: u64,
    pdpt_base: u64,
    pd_base: u64,
    pt_base: u64,
    pte0: u64,
    pte1: u64,
) {
    // PML4E[0] -> PDPT
    write_u64(mem, pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    // PDPTE[0] -> PD
    write_u64(mem, pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    // PDE[0] -> PT
    write_u64(mem, pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    // PTE[0] and PTE[1]
    write_u64(mem, pt_base, pte0);
    write_u64(mem, pt_base + 8, pte1);
}

fn long_state(pml4_base: u64, cpl: u8) -> CpuState {
    let mut state = CpuState::new(CpuMode::Long);
    state.segments.cs.selector = (state.segments.cs.selector & !0b11) | (cpl as u16 & 0b11);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pml4_base;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = EFER_LME;
    state.update_mode();
    state
}

#[test]
fn pc_cpu_bus_does_not_panic_on_wrapping_linear_addresses() {
    // Map the final 4KiB page in the canonical address space (0xffff...f000),
    // but leave the low page unmapped. Reading 2 bytes at `u64::MAX` should:
    //  - read the first byte from the high page,
    //  - wrap to address 0 for the second byte and fault there,
    //  - never panic from debug overflow checks.
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let high_page = 0x5000u64;

    // PML4E[511] -> PDPT[511] -> PD[511] -> PT[511] -> high_page
    let idx = 0x1ffu64;
    let flags = PTE_P | PTE_RW | PTE_US;
    write_u64(
        &mut bus.platform.memory,
        pml4_base + idx * 8,
        pdpt_base | flags,
    );
    write_u64(
        &mut bus.platform.memory,
        pdpt_base + idx * 8,
        pd_base | flags,
    );
    write_u64(&mut bus.platform.memory, pd_base + idx * 8, pt_base | flags);
    write_u64(
        &mut bus.platform.memory,
        pt_base + idx * 8,
        high_page | flags,
    );

    // Place a distinguishable byte at the final address.
    bus.platform.memory.write_u8(high_page + 0xfff, 0x90);

    let state = long_state(pml4_base, 0);
    bus.sync(&state);

    assert_eq!(
        bus.fetch(u64::MAX, 2),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 4, // I/D=1, P=0, W=0, U=0
        })
    );
    assert_eq!(
        bus.read_u16(u64::MAX),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 0,
        })
    );
    assert_eq!(
        bus.atomic_rmw::<u16, _>(u64::MAX, |old| (old, old)),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );
    assert_eq!(
        bus.write_u16(u64::MAX, 0x1234),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );
    assert_eq!(bus.platform.memory.read_u8(high_page + 0xfff), 0x90);
}

#[test]
fn cpu_core_bus_routes_port_io_to_toggle_a20() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // mov ax, 0; mov ds, ax; mov al, 0x11; mov [0], al
    // mov ax, 0xffff; mov ds, ax; mov al, 0x22; mov [0x10], al   (A20 disabled => aliases to 0)
    // mov al, 0x02; out 0x92, al                                  (enable A20)
    // mov al, 0x33; mov [0x10], al                                (A20 enabled => 0x100000)
    // hlt
    let code = [
        0x31,
        0xC0, // xor ax,ax
        0x8E,
        0xD8, // mov ds,ax
        0xB0,
        0x11, // mov al,0x11
        0xA2,
        0x00,
        0x00, // mov [0],al
        0xB8,
        0xFF,
        0xFF, // mov ax,0xffff
        0x8E,
        0xD8, // mov ds,ax
        0xB0,
        0x22, // mov al,0x22
        0xA2,
        0x10,
        0x00, // mov [0x10],al
        0xB0,
        0x02, // mov al,0x02
        0xE6,
        A20_GATE_PORT as u8, // out 0x92,al
        0xB0,
        0x33, // mov al,0x33
        0xA2,
        0x10,
        0x00, // mov [0x10],al
        0xF4, // hlt
    ];
    let code_base = 0x200u64;
    bus.platform.memory.write_physical(code_base, &code);

    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.set_stack_ptr(0x1000);
    cpu.state.segments.cs.selector = 0;
    cpu.state.segments.cs.base = 0;
    cpu.state.set_rip(code_base);

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 1024);
    assert_eq!(res.exit, BatchExit::Halted);

    assert_eq!(bus.platform.memory.read_u8(0), 0x22);
    assert_eq!(bus.platform.memory.read_u8(0x1_00000), 0x33);
}

#[test]
fn cpu_core_bus_routes_i8042_output_port_to_toggle_a20() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // 1) Write 0x11 to [0] with DS=0.
    // 2) Write 0x22 to [0x10] with DS=0xffff (physical 0x100000). With A20 disabled this aliases
    //    to [0] and overwrites 0x11.
    // 3) Enable A20 via the i8042 output port: write command 0xD1 to port 0x64, then output-port
    //    value 0x03 to port 0x60 (reset deasserted + A20 enabled).
    // 4) Write 0x33 to [0x10] again; with A20 enabled this should reach 0x100000.
    // 5) Disable A20 via the i8042 output port (value 0x01), then write 0x44 to [0x10]; this
    //    aliases back to [0].
    // 6) HLT.
    let code = [
        0x31,
        0xC0, // xor ax,ax
        0x8E,
        0xD8, // mov ds,ax
        0xB0,
        0x11, // mov al,0x11
        0xA2,
        0x00,
        0x00, // mov [0],al
        0xB8,
        0xFF,
        0xFF, // mov ax,0xffff
        0x8E,
        0xD8, // mov ds,ax
        0xB0,
        0x22, // mov al,0x22
        0xA2,
        0x10,
        0x00, // mov [0x10],al
        0xB0,
        0xD1, // mov al,0xD1
        0xE6,
        I8042_STATUS_PORT as u8, // out 0x64,al
        0xB0,
        0x03, // mov al,0x03
        0xE6,
        I8042_DATA_PORT as u8, // out 0x60,al
        0xB0,
        0x33, // mov al,0x33
        0xA2,
        0x10,
        0x00, // mov [0x10],al
        0xB0,
        0xD1, // mov al,0xD1
        0xE6,
        I8042_STATUS_PORT as u8, // out 0x64,al
        0xB0,
        0x01, // mov al,0x01
        0xE6,
        I8042_DATA_PORT as u8, // out 0x60,al
        0xB0,
        0x44, // mov al,0x44
        0xA2,
        0x10,
        0x00, // mov [0x10],al
        0xF4, // hlt
    ];
    let code_base = 0x200u64;
    bus.platform.memory.write_physical(code_base, &code);

    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.set_stack_ptr(0x1000);
    cpu.segments.cs.selector = 0;
    cpu.segments.cs.base = 0;
    cpu.set_rip(code_base);

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 1024);
    assert_eq!(res.exit, BatchExit::Halted);

    // The program disabled A20 before the final write, so reads in the 0x100000 region should
    // alias to 0x0.
    assert!(!bus.platform.chipset.a20().enabled());
    assert_eq!(bus.platform.memory.read_u8(0), 0x44);
    assert_eq!(bus.platform.memory.read_u8(0x1_00000), 0x44);

    // Re-enable A20 to observe that the earlier write to 0x100000 was preserved and was not
    // clobbered by the final aliased write.
    bus.platform.chipset.a20().set_enabled(true);
    assert_eq!(bus.platform.memory.read_u8(0), 0x44);
    assert_eq!(bus.platform.memory.read_u8(0x1_00000), 0x33);
}

fn write_idt_gate32(
    mem: &mut impl CpuBus,
    idt_base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let entry_addr = idt_base + (vector as u64) * 8;
    mem.write_u16(entry_addr, (offset & 0xffff) as u16).unwrap();
    mem.write_u16(entry_addr + 2, selector).unwrap();
    mem.write_u8(entry_addr + 4, 0).unwrap();
    mem.write_u8(entry_addr + 5, type_attr).unwrap();
    mem.write_u16(entry_addr + 6, (offset >> 16) as u16)
        .unwrap();
}

#[test]
fn cpu_core_can_deliver_pic_interrupt_through_pc_platform_bus() -> Result<(), CpuExit> {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);
    let mut ctrl = bus.interrupt_controller();

    {
        let mut ints = bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.raise_irq(InterruptInput::IsaIrq(1));
    }

    let mut cpu = CpuCore::new(CpuMode::Protected);
    let idt_base = 0x1000;
    let handler = 0x6000;
    write_idt_gate32(&mut bus, idt_base, 0x21, 0x08, handler, 0x8e);

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7ff;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.cs.base = 0;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.segments.ss.base = 0;
    cpu.state.write_gpr32(gpr::RSP, 0x9000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    cpu.poll_and_deliver_external_interrupt(&mut bus, &mut ctrl)?;
    assert_eq!(cpu.state.rip(), handler as u64);

    Ok(())
}

#[test]
fn pc_cpu_bus_multi_byte_writes_are_atomic_wrt_page_faults() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page0 = 0x5000u64;

    // Only the first page is present; the next page is not present.
    setup_long4_4k(
        &mut bus.platform.memory,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    // Sentinel values at the end of the first page.
    bus.platform.memory.write_u8(data_page0 + 0xffe, 0xaa);
    bus.platform.memory.write_u8(data_page0 + 0xfff, 0xbb);

    let state = long_state(pml4_base, 0);
    bus.sync(&state);

    // Writing a 16-bit value at 0xFFF crosses into the unmapped page at 0x1000.
    // The write must not partially commit to the mapped page.
    assert_eq!(
        bus.write_u16(0xfff, 0x1234),
        Err(Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );

    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xffe), 0xaa);
    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xfff), 0xbb);

    // Same property for `write_bytes`.
    assert_eq!(
        bus.write_bytes(0xffe, &[1, 2, 3]),
        Err(Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 1,
        })
    );
    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xffe), 0xaa);
    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xfff), 0xbb);
}

#[test]
fn pc_cpu_bus_atomic_rmw_faults_on_user_read_only_pages() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    // User-accessible but read-only page.
    setup_long4_4k(
        &mut bus.platform.memory,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_US,
        0,
    );

    bus.platform.memory.write_u8(data_page, 0x7b);

    let state = long_state(pml4_base, 3);
    bus.sync(&state);

    // Atomic RMWs are write-intent operations, even if the update is a no-op.
    assert_eq!(
        bus.atomic_rmw::<u8, _>(0, |old| (old, old)),
        Err(Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 1) | (1 << 2),
        })
    );
}

#[test]
fn pc_cpu_bus_bulk_copy_memmove_overlap_semantics() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);
    bus.platform.chipset.a20().set_enabled(true);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE;
    state.update_mode();
    bus.sync(&state);

    let data: Vec<u8> = (0u8..16).collect();
    bus.write_bytes(0, &data).unwrap();

    assert!(bus.supports_bulk_copy());
    assert!(bus.bulk_copy(2, 0, 8).unwrap());

    let mut out = [0u8; 10];
    bus.read_bytes(0, &mut out).unwrap();

    let expected = [0u8, 1, 0, 1, 2, 3, 4, 5, 6, 7];
    assert_eq!(out, expected);
}

#[test]
fn pc_cpu_bus_bulk_set_repeats_pattern() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);
    bus.platform.chipset.a20().set_enabled(true);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE;
    state.update_mode();
    bus.sync(&state);

    assert!(bus.supports_bulk_set());
    assert!(bus.bulk_set(0x40, &[0xDE, 0xAD, 0xBE, 0xEF], 4).unwrap());

    let mut out = [0u8; 16];
    bus.read_bytes(0x40, &mut out).unwrap();

    let expected = [0xDE, 0xAD, 0xBE, 0xEF]
        .iter()
        .copied()
        .cycle()
        .take(16)
        .collect::<Vec<_>>();
    assert_eq!(out.as_slice(), expected.as_slice());
}

#[test]
fn pc_cpu_bus_bulk_set_preserves_pattern_alignment_across_chunks() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);
    bus.platform.chipset.a20().set_enabled(true);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE;
    state.update_mode();
    bus.sync(&state);

    // Use a non-power-of-two pattern length so `BUF_SIZE` is not a multiple of
    // `pattern.len()`. Ensure we span multiple chunks.
    let pattern = [0x10u8, 0x20, 0x30];
    let repeat = 2000usize; // 6000 bytes
    let total = pattern.len() * repeat;

    assert!(bus.supports_bulk_set());
    assert!(bus.bulk_set(0x8000, &pattern, repeat).unwrap());

    let mut out = vec![0u8; total];
    bus.read_bytes(0x8000, &mut out).unwrap();

    let expected: Vec<u8> = pattern.iter().copied().cycle().take(total).collect();
    assert_eq!(out, expected);
}

#[test]
fn pc_cpu_bus_bulk_copy_is_atomic_wrt_page_faults() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page0 = 0x5000u64;

    // Only the first page is present; the next page is not present.
    setup_long4_4k(
        &mut bus.platform.memory,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    // Source data fully within the first mapped page.
    bus.platform
        .memory
        .write_physical(data_page0 + 0x800, &[1, 2, 3, 4]);

    // Sentinel values at the end of the first page.
    bus.platform.memory.write_u8(data_page0 + 0xffe, 0xaa);
    bus.platform.memory.write_u8(data_page0 + 0xfff, 0xbb);

    let state = long_state(pml4_base, 0);
    bus.sync(&state);

    // Destination crosses into the unmapped page at 0x1000.
    assert_eq!(
        bus.bulk_copy(0xffe, 0x800, 4),
        Err(Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );

    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xffe), 0xaa);
    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xfff), 0xbb);
}

#[test]
fn pc_cpu_bus_bulk_set_is_atomic_wrt_page_faults() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page0 = 0x5000u64;

    // Only the first page is present; the next page is not present.
    setup_long4_4k(
        &mut bus.platform.memory,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    // Sentinel values at the end of the first page.
    bus.platform.memory.write_u8(data_page0 + 0xffe, 0xaa);
    bus.platform.memory.write_u8(data_page0 + 0xfff, 0xbb);

    let state = long_state(pml4_base, 0);
    bus.sync(&state);

    // Fill crosses into the unmapped page at 0x1000.
    assert_eq!(
        bus.bulk_set(0xffe, &[0x11], 3),
        Err(Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 1,
        })
    );

    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xffe), 0xaa);
    assert_eq!(bus.platform.memory.read_u8(data_page0 + 0xfff), 0xbb);
}

#[test]
fn cpu_core_bus_translates_legacy32_paging_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Map a virtual address above the available RAM to a physical page in low memory so we can
    // detect whether paging translation is being applied.
    let vaddr = 0x0040_0000u64;
    let pd_phys = 0x1000u64;
    let pt_phys = 0x2000u64;
    let page_phys = 0x3000u64;

    let pd_index = (vaddr >> 22) & 0x3ff;
    let pt_index = (vaddr >> 12) & 0x3ff;
    let pde_addr = pd_phys + pd_index * 4;
    let pte_addr = pt_phys + pt_index * 4;

    // PDE: present + writable -> points at PT.
    memory::MemoryBus::write_u32(&mut bus.platform.memory, pde_addr, (pt_phys as u32) | 0x3);
    // PTE: present + writable -> points at the target physical page.
    memory::MemoryBus::write_u32(&mut bus.platform.memory, pte_addr, (page_phys as u32) | 0x3);

    bus.platform.memory.write_u8(page_phys, 0xAA);

    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = pd_phys;
    cpu.control.cr4 = 0;
    cpu.segments.cs.selector = 0x08;
    cpu.segments.cs.base = 0;
    cpu.segments.ss.selector = 0x10;
    cpu.segments.ss.base = 0;

    bus.sync(&cpu);

    // First access should walk page tables and observe the mapped physical value.
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    let pte_after_read = memory::MemoryBus::read_u32(&mut bus.platform.memory, pte_addr);
    assert_ne!(
        pte_after_read & (1 << 5),
        0,
        "PTE accessed bit should be set"
    );
    assert_eq!(pte_after_read & 0xffff_f000, page_phys as u32);

    // Write through the mapping; the MMU should mark the PTE dirty.
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(page_phys), 0xBB);

    let pte_after_write = memory::MemoryBus::read_u32(&mut bus.platform.memory, pte_addr);
    assert_ne!(pte_after_write & (1 << 6), 0, "PTE dirty bit should be set");
}

#[test]
fn cpu_core_bus_translates_legacy32_4mb_page_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Exercise 4MB pages (CR4.PSE + PDE.PS) in legacy 32-bit paging mode.
    //
    // Map linear 0x0040_0000..0x0080_0000 (PDE index 1) to physical 0x0000_0000..0x0040_0000.
    let vaddr = 0x0040_5000u64;
    let pd_phys = 0x1000u64;
    let phys_addr = 0x5000u64;

    let pde_index = (vaddr >> 22) & 0x3ff;
    let pde_addr = pd_phys + pde_index * 4;

    const CR4_PSE: u64 = 1 << 4;
    const PDE_PS: u32 = 1 << 7;
    const PDE_P: u32 = 1 << 0;
    const PDE_RW: u32 = 1 << 1;
    const PDE_US: u32 = 1 << 2;

    memory::MemoryBus::write_u32(
        &mut bus.platform.memory,
        pde_addr,
        PDE_P | PDE_RW | PDE_US | PDE_PS,
    );
    bus.platform.memory.write_u8(phys_addr, 0xAA);

    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = pd_phys;
    cpu.control.cr4 = CR4_PSE;
    cpu.segments.cs.selector = 3;
    cpu.segments.cs.base = 0;
    cpu.segments.ss.selector = 3;
    cpu.segments.ss.base = 0;
    bus.sync(&cpu);

    // Read should populate the TLB and set accessed on the leaf PDE, but not dirty.
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);
    let pde_after_read = memory::MemoryBus::read_u32(&mut bus.platform.memory, pde_addr);
    assert_ne!(
        pde_after_read & (1 << 5),
        0,
        "PDE accessed bit should be set"
    );
    assert_eq!(
        pde_after_read & (1 << 6),
        0,
        "PDE dirty bit should not be set on read"
    );

    // Write should set the dirty bit even on a TLB hit.
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(phys_addr), 0xBB);
    let pde_after_write = memory::MemoryBus::read_u32(&mut bus.platform.memory, pde_addr);
    assert_ne!(pde_after_write & (1 << 6), 0, "PDE dirty bit should be set");
}

#[test]
fn cpu_core_bus_translates_pae_paging_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Map a virtual address above the available RAM to a physical page in low memory so we can
    // detect whether paging translation is being applied (but use IA-32 PAE paging structures).
    let vaddr = 0x0040_0000u64;
    let pdpt_phys = 0x1000u64;
    let pd_phys = 0x2000u64;
    let pt_phys = 0x3000u64;
    let page_phys = 0x4000u64;

    let pdpt_index = (vaddr >> 30) & 0x3;
    let pd_index = (vaddr >> 21) & 0x1ff;
    let pt_index = (vaddr >> 12) & 0x1ff;

    let pdpte_addr = pdpt_phys + pdpt_index * 8;
    let pde_addr = pd_phys + pd_index * 8;
    let pte_addr = pt_phys + pt_index * 8;

    // PDPTE: present -> points at PD.
    //
    // Note: In IA-32 PAE, PDPTE bits 1..=2 are reserved, so do *not* set RW/US here.
    memory::MemoryBus::write_u64(&mut bus.platform.memory, pdpte_addr, pd_phys | PTE_P);
    // PDE: present + writable -> points at PT.
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_addr,
        pt_phys | PTE_P | PTE_RW | PTE_US,
    );
    // PTE: present + writable -> points at the target physical page.
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pte_addr,
        page_phys | PTE_P | PTE_RW | PTE_US,
    );

    bus.platform.memory.write_u8(page_phys, 0xAA);

    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = pdpt_phys;
    cpu.control.cr4 = CR4_PAE;
    cpu.segments.cs.selector = 0x08;
    cpu.segments.cs.base = 0;
    cpu.segments.ss.selector = 0x10;
    cpu.segments.ss.base = 0;
    cpu.update_mode();

    bus.sync(&cpu);

    // First access should walk the PAE page tables and observe the mapped physical value.
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    let pdpte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pdpte_addr);
    assert_eq!(
        pdpte_after_read & (1 << 5),
        0,
        "PDPT entries do not have an accessed bit in IA-32 PAE paging"
    );

    let pde_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pde_addr);
    assert_ne!(
        pde_after_read & (1 << 5),
        0,
        "PDE accessed bit should be set"
    );

    let pte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pte_addr);
    assert_ne!(
        pte_after_read & (1 << 5),
        0,
        "PTE accessed bit should be set"
    );
    assert_eq!(
        pte_after_read & (1 << 6),
        0,
        "PTE dirty bit should not be set on read"
    );
    assert_eq!(pte_after_read & 0x000f_ffff_ffff_f000, page_phys);

    // Write through the mapping; the MMU should mark the PTE dirty (even if the translation hits
    // in the TLB).
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(page_phys), 0xBB);

    let pte_after_write = memory::MemoryBus::read_u64(&mut bus.platform.memory, pte_addr);
    assert_ne!(pte_after_write & (1 << 6), 0, "PTE dirty bit should be set");
}

#[test]
fn cpu_core_bus_translates_pae_2mb_page_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Exercise 2MB pages (CR4.PSE + PDE.PS) in IA-32 PAE paging mode.
    //
    // Map linear 0x0000_0000..0x0020_0000 (PDE index 0) to physical 0x0000_0000..0x0020_0000.
    let vaddr = 0x0000_5000u64;
    let pdpt_phys = 0x1000u64;
    let pd_phys = 0x2000u64;
    let phys_addr = 0x5000u64;

    const CR4_PSE: u64 = 1 << 4;
    const PTE_PS: u64 = 1 << 7;

    let pdpte_addr = pdpt_phys; // index 0
    let pde_addr = pd_phys; // index 0

    // PDPTE: present -> points at PD (PDPT entries have no accessed bit in IA-32 PAE).
    memory::MemoryBus::write_u64(&mut bus.platform.memory, pdpte_addr, pd_phys | PTE_P);
    // PDE: present + writable + PS -> maps a 2MB page with base 0.
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_addr,
        PTE_P | PTE_RW | PTE_US | PTE_PS,
    );

    bus.platform.memory.write_u8(phys_addr, 0xAA);

    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = pdpt_phys;
    cpu.control.cr4 = CR4_PAE | CR4_PSE;
    cpu.segments.cs.selector = 3;
    cpu.segments.cs.base = 0;
    cpu.segments.ss.selector = 3;
    cpu.segments.ss.base = 0;
    cpu.update_mode();

    bus.sync(&cpu);

    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    let pdpte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pdpte_addr);
    assert_eq!(
        pdpte_after_read & (1 << 5),
        0,
        "PDPT entries do not have an accessed bit in IA-32 PAE paging"
    );

    let pde_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pde_addr);
    assert_ne!(
        pde_after_read & (1 << 5),
        0,
        "PDE accessed bit should be set"
    );
    assert_eq!(
        pde_after_read & (1 << 6),
        0,
        "PDE dirty bit should not be set on read"
    );

    // Write should set dirty even on a TLB hit.
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(phys_addr), 0xBB);
    let pde_after_write = memory::MemoryBus::read_u64(&mut bus.platform.memory, pde_addr);
    assert_ne!(pde_after_write & (1 << 6), 0, "PDE dirty bit should be set");
}

#[test]
fn cpu_core_bus_translates_long4_paging_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Map a virtual address above the available RAM to a physical page in low memory so we can
    // detect whether paging translation is being applied (in 4-level long-mode paging).
    let vaddr = 0x0040_0000u64;
    let pml4_phys = 0x1000u64;
    let pdpt_phys = 0x2000u64;
    let pd_phys = 0x3000u64;
    let pt_phys = 0x4000u64;
    let page_phys = 0x5000u64;

    let pml4_index = (vaddr >> 39) & 0x1ff;
    let pdpt_index = (vaddr >> 30) & 0x1ff;
    let pd_index = (vaddr >> 21) & 0x1ff;
    let pt_index = (vaddr >> 12) & 0x1ff;

    let pml4e_addr = pml4_phys + pml4_index * 8;
    let pdpte_addr = pdpt_phys + pdpt_index * 8;
    let pde_addr = pd_phys + pd_index * 8;
    let pte_addr = pt_phys + pt_index * 8;

    // PML4E -> PDPT
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pml4e_addr,
        pdpt_phys | PTE_P | PTE_RW | PTE_US,
    );
    // PDPTE -> PD
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pdpte_addr,
        pd_phys | PTE_P | PTE_RW | PTE_US,
    );
    // PDE -> PT
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_addr,
        pt_phys | PTE_P | PTE_RW | PTE_US,
    );
    // PTE -> page
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pte_addr,
        page_phys | PTE_P | PTE_RW | PTE_US,
    );

    bus.platform.memory.write_u8(page_phys, 0xAA);

    let state = long_state(pml4_phys, 3);
    bus.sync(&state);

    // First access should walk page tables and observe the mapped physical value.
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    let pml4e_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pml4e_addr);
    assert_ne!(
        pml4e_after_read & (1 << 5),
        0,
        "PML4E accessed bit should be set"
    );

    let pdpte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pdpte_addr);
    assert_ne!(
        pdpte_after_read & (1 << 5),
        0,
        "PDPTE accessed bit should be set"
    );

    let pde_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pde_addr);
    assert_ne!(
        pde_after_read & (1 << 5),
        0,
        "PDE accessed bit should be set"
    );

    let pte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pte_addr);
    assert_ne!(
        pte_after_read & (1 << 5),
        0,
        "PTE accessed bit should be set"
    );
    assert_eq!(
        pte_after_read & (1 << 6),
        0,
        "PTE dirty bit should not be set on read"
    );
    assert_eq!(pte_after_read & 0x000f_ffff_ffff_f000, page_phys);

    // Write through the mapping; the MMU should mark the PTE dirty.
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(page_phys), 0xBB);

    let pte_after_write = memory::MemoryBus::read_u64(&mut bus.platform.memory, pte_addr);
    assert_ne!(pte_after_write & (1 << 6), 0, "PTE dirty bit should be set");
}

#[test]
fn cpu_core_bus_translates_long4_2mb_page_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Exercise 2MB pages (CR4.PSE + PDE.PS) in 4-level long-mode paging.
    let vaddr = 0x0000_5000u64;
    let pml4_phys = 0x1000u64;
    let pdpt_phys = 0x2000u64;
    let pd_phys = 0x3000u64;
    let phys_addr = 0x5000u64;

    const CR4_PSE: u64 = 1 << 4;
    const PTE_PS: u64 = 1 << 7;

    let pml4_index = (vaddr >> 39) & 0x1ff;
    let pdpt_index = (vaddr >> 30) & 0x1ff;
    let pd_index = (vaddr >> 21) & 0x1ff;

    let pml4e_addr = pml4_phys + pml4_index * 8;
    let pdpte_addr = pdpt_phys + pdpt_index * 8;
    let pde_addr = pd_phys + pd_index * 8;

    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pml4e_addr,
        pdpt_phys | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pdpte_addr,
        pd_phys | PTE_P | PTE_RW | PTE_US,
    );
    // Leaf PDE (2MB page) with base 0.
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_addr,
        PTE_P | PTE_RW | PTE_US | PTE_PS,
    );

    bus.platform.memory.write_u8(phys_addr, 0xAA);

    let mut state = long_state(pml4_phys, 3);
    state.control.cr4 |= CR4_PSE;
    bus.sync(&state);

    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    let pml4e_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pml4e_addr);
    assert_ne!(
        pml4e_after_read & (1 << 5),
        0,
        "PML4E accessed bit should be set"
    );
    let pdpte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pdpte_addr);
    assert_ne!(
        pdpte_after_read & (1 << 5),
        0,
        "PDPTE accessed bit should be set"
    );
    let pde_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pde_addr);
    assert_ne!(
        pde_after_read & (1 << 5),
        0,
        "PDE accessed bit should be set"
    );
    assert_eq!(
        pde_after_read & (1 << 6),
        0,
        "PDE dirty bit should not be set on read"
    );

    // Write should set dirty even on a TLB hit.
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(phys_addr), 0xBB);
    let pde_after_write = memory::MemoryBus::read_u64(&mut bus.platform.memory, pde_addr);
    assert_ne!(pde_after_write & (1 << 6), 0, "PDE dirty bit should be set");
}

#[test]
fn cpu_core_bus_translates_long4_1gb_page_via_mmu() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // Exercise 1GB pages (CR4.PSE + PDPTE.PS) in 4-level long-mode paging.
    let vaddr = 0x0000_5000u64;
    let pml4_phys = 0x1000u64;
    let pdpt_phys = 0x2000u64;
    let phys_addr = 0x5000u64;

    const CR4_PSE: u64 = 1 << 4;
    const PTE_PS: u64 = 1 << 7;

    let pml4_index = (vaddr >> 39) & 0x1ff;
    let pdpt_index = (vaddr >> 30) & 0x1ff;

    let pml4e_addr = pml4_phys + pml4_index * 8;
    let pdpte_addr = pdpt_phys + pdpt_index * 8;

    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pml4e_addr,
        pdpt_phys | PTE_P | PTE_RW | PTE_US,
    );
    // Leaf PDPTE (1GB page) with base 0.
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pdpte_addr,
        PTE_P | PTE_RW | PTE_US | PTE_PS,
    );

    bus.platform.memory.write_u8(phys_addr, 0xAA);

    let mut state = long_state(pml4_phys, 3);
    state.control.cr4 |= CR4_PSE;
    bus.sync(&state);

    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    let pml4e_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pml4e_addr);
    assert_ne!(
        pml4e_after_read & (1 << 5),
        0,
        "PML4E accessed bit should be set"
    );
    let pdpte_after_read = memory::MemoryBus::read_u64(&mut bus.platform.memory, pdpte_addr);
    assert_ne!(
        pdpte_after_read & (1 << 5),
        0,
        "PDPTE accessed bit should be set"
    );
    assert_eq!(
        pdpte_after_read & (1 << 6),
        0,
        "PDPTE dirty bit should not be set on read"
    );

    // Write should set dirty even on a TLB hit.
    bus.write_u8(vaddr, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(phys_addr), 0xBB);
    let pdpte_after_write = memory::MemoryBus::read_u64(&mut bus.platform.memory, pdpte_addr);
    assert_ne!(
        pdpte_after_write & (1 << 6),
        0,
        "PDPTE dirty bit should be set"
    );
}

#[test]
fn pc_cpu_bus_invlpg_flushes_long_mode_translation() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let vaddr = 0x0040_0000u64;
    let pml4_phys = 0x1000u64;
    let pdpt_phys = 0x2000u64;
    let pd_phys = 0x3000u64;
    let pt_phys = 0x4000u64;
    let page0_phys = 0x5000u64;
    let page1_phys = 0x6000u64;

    let pml4_index = (vaddr >> 39) & 0x1ff;
    let pdpt_index = (vaddr >> 30) & 0x1ff;
    let pd_index = (vaddr >> 21) & 0x1ff;
    let pt_index = (vaddr >> 12) & 0x1ff;

    let pml4e_addr = pml4_phys + pml4_index * 8;
    let pdpte_addr = pdpt_phys + pdpt_index * 8;
    let pde_addr = pd_phys + pd_index * 8;
    let pte_addr = pt_phys + pt_index * 8;

    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pml4e_addr,
        pdpt_phys | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pdpte_addr,
        pd_phys | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_addr,
        pt_phys | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pte_addr,
        page0_phys | PTE_P | PTE_RW | PTE_US,
    );

    bus.platform.memory.write_u8(page0_phys, 0xAA);
    bus.platform.memory.write_u8(page1_phys, 0xBB);

    let state = long_state(pml4_phys, 3);
    bus.sync(&state);

    // Prime the TLB (and set accessed bits).
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    // Change the leaf mapping in memory.
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pte_addr,
        page1_phys | PTE_P | PTE_RW | PTE_US,
    );

    // Without `INVLPG`, the cached translation should still point at the old physical page.
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    // Flush and retry.
    bus.invlpg(vaddr);
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xBB);
}

#[test]
fn pc_cpu_bus_sync_cr3_flushes_long_mode_translation() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let vaddr = 0x0040_0000u64;
    let page0_phys = 0x5000u64;
    let page1_phys = 0x6000u64;

    // Two distinct page-table roots so we can validate that CR3 writes flush cached translations.
    let pml4_0 = 0x1000u64;
    let pdpt_0 = 0x2000u64;
    let pd_0 = 0x3000u64;
    let pt_0 = 0x4000u64;

    let pml4_1 = 0x7000u64;
    let pdpt_1 = 0x8000u64;
    let pd_1 = 0x9000u64;
    let pt_1 = 0xA000u64;

    let pml4_index = (vaddr >> 39) & 0x1ff;
    let pdpt_index = (vaddr >> 30) & 0x1ff;
    let pd_index = (vaddr >> 21) & 0x1ff;
    let pt_index = (vaddr >> 12) & 0x1ff;

    let pml4e_0 = pml4_0 + pml4_index * 8;
    let pdpte_0 = pdpt_0 + pdpt_index * 8;
    let pde_0 = pd_0 + pd_index * 8;
    let pte_0 = pt_0 + pt_index * 8;

    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pml4e_0,
        pdpt_0 | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pdpte_0,
        pd_0 | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_0,
        pt_0 | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pte_0,
        page0_phys | PTE_P | PTE_RW | PTE_US,
    );

    let pml4e_1 = pml4_1 + pml4_index * 8;
    let pdpte_1 = pdpt_1 + pdpt_index * 8;
    let pde_1 = pd_1 + pd_index * 8;
    let pte_1 = pt_1 + pt_index * 8;

    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pml4e_1,
        pdpt_1 | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pdpte_1,
        pd_1 | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pde_1,
        pt_1 | PTE_P | PTE_RW | PTE_US,
    );
    memory::MemoryBus::write_u64(
        &mut bus.platform.memory,
        pte_1,
        page1_phys | PTE_P | PTE_RW | PTE_US,
    );

    bus.platform.memory.write_u8(page0_phys, 0xAA);
    bus.platform.memory.write_u8(page1_phys, 0xBB);

    let mut state = long_state(pml4_0, 3);
    bus.sync(&state);
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xAA);

    // Switch address space.
    state.control.cr3 = pml4_1;
    bus.sync(&state);
    assert_eq!(bus.read_u8(vaddr).unwrap(), 0xBB);
}

#[test]
fn pc_cpu_bus_non_canonical_long_mode_is_gp0() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let state = long_state(0x1000, 0);
    bus.sync(&state);

    // Non-canonical for 48-bit canonical addressing (bit 48 set but upper bits are not sign
    // extended).
    let non_canonical = 0x0001_0000_0000_0000u64;
    assert_eq!(bus.read_u8(non_canonical), Err(Exception::gp0()));
}

#[test]
fn pc_cpu_bus_paging_disabled_truncates_linear_addresses_to_32bit() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    bus.platform.memory.write_u8(0, 0xAA);
    bus.platform.memory.write_u8(0x1234, 0xBB);

    let mut state = CpuState::new(CpuMode::Protected);
    // Paging disabled.
    state.control.cr0 = CR0_PE;
    state.update_mode();
    bus.sync(&state);

    assert_eq!(bus.read_u8(0).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x1234).unwrap(), 0xBB);

    // With paging disabled, x86 linear addresses are 32-bit.
    assert_eq!(bus.read_u8(0x1_0000_0000).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x1_0000_0000 + 0x1234).unwrap(), 0xBB);
}

#[test]
fn pc_cpu_bus_supervisor_write_to_read_only_page_succeeds_when_wp0() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    // Read-only leaf PTE (R/W=0), but user-accessible (U/S=1) so we can test supervisor write
    // behavior under CR0.WP.
    setup_long4_4k(
        &mut bus.platform.memory,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_US,
        0,
    );

    bus.platform.memory.write_u8(data_page, 0xAA);

    let state = long_state(pml4_base, 0);
    bus.sync(&state);

    // With CR0.WP=0 (default), supervisor writes ignore the read-only bit.
    bus.write_u8(0, 0xBB).unwrap();
    assert_eq!(bus.platform.memory.read_u8(data_page), 0xBB);

    let pte_after = memory::MemoryBus::read_u64(&mut bus.platform.memory, pt_base);
    assert_eq!(pte_after & PTE_RW, 0, "PTE should remain read-only");
    assert_ne!(pte_after & (1 << 6), 0, "PTE dirty bit should be set");
}

#[test]
fn pc_cpu_bus_supervisor_write_to_read_only_page_faults_when_wp1() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    setup_long4_4k(
        &mut bus.platform.memory,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_US,
        0,
    );
    bus.platform.memory.write_u8(data_page, 0xAA);

    const CR0_WP: u64 = 1 << 16;
    let mut state = long_state(pml4_base, 0);
    state.control.cr0 |= CR0_WP;
    bus.sync(&state);

    assert_eq!(
        bus.write_u8(0, 0xBB),
        Err(Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 1), // P=1, W=1, U=0
        })
    );
    assert_eq!(bus.platform.memory.read_u8(data_page), 0xAA);

    let pte_after = memory::MemoryBus::read_u64(&mut bus.platform.memory, pt_base);
    assert_eq!(pte_after & (1 << 6), 0, "PTE dirty bit should not be set");
}

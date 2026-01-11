use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::interrupts::{CpuCore, CpuExit};
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{gpr, CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::Exception;
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
    write_u64(mem, pt_base + 0 * 8, pte0);
    write_u64(mem, pt_base + 1 * 8, pte1);
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
fn cpu_core_bus_routes_port_io_to_toggle_a20() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // mov ax, 0; mov ds, ax; mov al, 0x11; mov [0], al
    // mov ax, 0xffff; mov ds, ax; mov al, 0x22; mov [0x10], al   (A20 disabled => aliases to 0)
    // mov al, 0x02; out 0x92, al                                  (enable A20)
    // mov al, 0x33; mov [0x10], al                                (A20 enabled => 0x100000)
    // hlt
    let code = [
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD8, // mov ds,ax
        0xB0, 0x11, // mov al,0x11
        0xA2, 0x00, 0x00, // mov [0],al
        0xB8, 0xFF, 0xFF, // mov ax,0xffff
        0x8E, 0xD8, // mov ds,ax
        0xB0, 0x22, // mov al,0x22
        0xA2, 0x10, 0x00, // mov [0x10],al
        0xB0, 0x02, // mov al,0x02
        0xE6, 0x92, // out 0x92,al
        0xB0, 0x33, // mov al,0x33
        0xA2, 0x10, 0x00, // mov [0x10],al
        0xF4, // hlt
    ];
    let code_base = 0x200u64;
    bus.platform.memory.write_physical(code_base, &code);

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_stack_ptr(0x1000);
    cpu.segments.cs.selector = 0;
    cpu.segments.cs.base = 0;
    cpu.set_rip(code_base);

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 1024);
    assert_eq!(res.exit, BatchExit::Halted);

    assert_eq!(bus.platform.memory.read_u8(0), 0x22);
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

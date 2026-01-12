#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_machine::pc::PcMachine;
use aero_machine::RunExit;
use aero_net_backend::NetworkBackend;
use memory::MemoryBus as _;

#[derive(Default)]
struct BackendState {
    tx_frames: Vec<Vec<u8>>,
    rx_queue: VecDeque<Vec<u8>>,
}

struct LoopbackBackend {
    state: Rc<RefCell<BackendState>>,
}

impl NetworkBackend for LoopbackBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        let mut state = self.state.borrow_mut();
        state.tx_frames.push(frame.clone());
        state.rx_queue.push_back(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.state.borrow_mut().rx_queue.pop_front()
    }
}

fn install_hlt_loop(pc: &mut PcMachine, code_base: u64) {
    // hlt; jmp short $-3 (back to hlt)
    let code = [0xF4u8, 0xEB, 0xFD];
    pc.bus.platform.memory.write_physical(code_base, &code);
}

fn setup_real_mode_cpu(pc: &mut PcMachine, entry_ip: u64) {
    pc.cpu = aero_cpu_core::CpuCore::new(aero_cpu_core::state::CpuMode::Real);

    // Real-mode segments: base = selector<<4, limit = 0xFFFF.
    for seg in [
        &mut pc.cpu.state.segments.cs,
        &mut pc.cpu.state.segments.ds,
        &mut pc.cpu.state.segments.es,
        &mut pc.cpu.state.segments.ss,
        &mut pc.cpu.state.segments.fs,
        &mut pc.cpu.state.segments.gs,
    ] {
        seg.selector = 0;
        seg.base = 0;
        seg.limit = 0xFFFF;
        seg.access = 0;
    }

    pc.cpu.state.set_stack_ptr(0x8000);
    pc.cpu.state.set_rip(entry_ip);
    pc.cpu.state.set_rflags(0x202); // IF=1
    pc.cpu.state.halted = false;
}

fn build_tx_desc(buffer_addr: u64, len: u16) -> [u8; 16] {
    const TXD_CMD_EOP: u8 = 1 << 0;
    const TXD_CMD_RS: u8 = 1 << 3;

    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&buffer_addr.to_le_bytes());
    bytes[8..10].copy_from_slice(&len.to_le_bytes());
    bytes[10] = 0; // cso
    bytes[11] = TXD_CMD_EOP | TXD_CMD_RS;
    bytes[12] = 0; // status (device will set DD)
    bytes[13] = 0; // css
    bytes[14..16].copy_from_slice(&0u16.to_le_bytes());
    bytes
}

fn build_rx_desc(buffer_addr: u64) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&buffer_addr.to_le_bytes());
    bytes
}

#[test]
fn pc_machine_e1000_network_backend_loopback() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    let mut pc = PcMachine::new_with_e1000(RAM_SIZE, None);
    assert!(pc.bus.platform.has_e1000());

    // Keep the CPU in a deterministic HLT loop so we can call `run_slice` without executing
    // uninitialized memory at the reset vector.
    let code_base = 0x2000u64;
    install_hlt_loop(&mut pc, code_base);
    setup_real_mode_cpu(&mut pc, code_base);
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    let backend_state = Rc::new(RefCell::new(BackendState::default()));
    pc.set_network_backend(Box::new(LoopbackBackend {
        state: backend_state.clone(),
    }));

    // Locate BAR0 for the E1000 MMIO window (assigned during BIOS POST).
    let (bar0_base, command) = {
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("E1000 config function must exist");
        (cfg.bar_range(0).expect("E1000 BAR0 must exist").base, cfg.command())
    };
    assert_ne!(bar0_base, 0, "E1000 BAR0 should be assigned during BIOS POST");

    // Enable Bus Mastering so `PcPlatform::process_e1000` will allow DMA.
    {
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .write_config(bdf, 0x04, 2, u32::from(command | (1 << 2)));
    }

    // --- Guest RAM layout for descriptor rings and buffers. ---
    let tx_desc_base = 0x10_000u64;
    let tx_buf_addr = 0x11_000u64;
    let rx_desc_base = 0x12_000u64;
    let rx_buf_addr = 0x13_000u64;

    // Build a small Ethernet frame: dst MAC = device MAC, src MAC = fixed, ethertype = IPv4.
    let dst_mac = pc.bus.platform.e1000_mac_addr().expect("e1000 enabled");
    let src_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let mut frame = Vec::new();
    frame.extend_from_slice(&dst_mac);
    frame.extend_from_slice(&src_mac);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(&[0xAB; 46]);
    assert_eq!(frame.len(), 60);

    // Write TX frame payload into guest memory.
    pc.bus.platform.memory.write_physical(tx_buf_addr, &frame);

    // TX ring: 2 descriptors, with a single packet in entry 0 (TDH=0, TDT=1).
    pc.bus
        .platform
        .memory
        .write_physical(tx_desc_base, &build_tx_desc(tx_buf_addr, frame.len() as u16));
    pc.bus
        .platform
        .memory
        .write_physical(tx_desc_base + 16, &[0u8; 16]);

    // RX ring: 2 descriptors, with a single buffer available at entry 0 (RDH=0, RDT=1).
    pc.bus
        .platform
        .memory
        .write_physical(rx_desc_base, &build_rx_desc(rx_buf_addr));
    pc.bus
        .platform
        .memory
        .write_physical(rx_desc_base + 16, &build_rx_desc(rx_buf_addr + 2048));

    // --- Program minimal E1000 state via BAR0 MMIO registers. ---
    const REG_RCTL: u64 = 0x0100;
    const REG_TCTL: u64 = 0x0400;

    const REG_RDBAL: u64 = 0x2800;
    const REG_RDBAH: u64 = 0x2804;
    const REG_RDLEN: u64 = 0x2808;
    const REG_RDH: u64 = 0x2810;
    const REG_RDT: u64 = 0x2818;

    const REG_TDBAL: u64 = 0x3800;
    const REG_TDBAH: u64 = 0x3804;
    const REG_TDLEN: u64 = 0x3808;
    const REG_TDH: u64 = 0x3810;
    const REG_TDT: u64 = 0x3818;

    const RCTL_EN: u32 = 1 << 1;
    const TCTL_EN: u32 = 1 << 1;

    // RX.
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_RDBAL, rx_desc_base as u32);
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_RDBAH, (rx_desc_base >> 32) as u32);
    pc.bus.platform.memory.write_u32(bar0_base + REG_RDLEN, 32);
    pc.bus.platform.memory.write_u32(bar0_base + REG_RDH, 0);
    pc.bus.platform.memory.write_u32(bar0_base + REG_RDT, 1);
    pc.bus.platform.memory.write_u32(bar0_base + REG_RCTL, RCTL_EN);

    // TX.
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_TDBAL, tx_desc_base as u32);
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_TDBAH, (tx_desc_base >> 32) as u32);
    pc.bus.platform.memory.write_u32(bar0_base + REG_TDLEN, 32);
    pc.bus.platform.memory.write_u32(bar0_base + REG_TDH, 0);
    pc.bus.platform.memory.write_u32(bar0_base + REG_TDT, 1);
    pc.bus.platform.memory.write_u32(bar0_base + REG_TCTL, TCTL_EN);

    // Drive one network poll to process TX + loop back RX.
    pc.poll_network();

    // Host observed the guest TX frame.
    {
        let state = backend_state.borrow();
        assert_eq!(state.tx_frames.len(), 1);
        assert_eq!(state.tx_frames[0], frame);
    }

    // RX frame was delivered into guest memory (descriptor 0).
    let mut rx_readback = vec![0u8; frame.len()];
    pc.bus
        .platform
        .memory
        .read_physical(rx_buf_addr, &mut rx_readback);
    assert_eq!(rx_readback, frame);

    let mut rx_desc_bytes = [0u8; 16];
    pc.bus
        .platform
        .memory
        .read_physical(rx_desc_base, &mut rx_desc_bytes);
    let rx_len = u16::from_le_bytes(rx_desc_bytes[8..10].try_into().unwrap()) as usize;
    let rx_status = rx_desc_bytes[12];
    assert_eq!(rx_len, frame.len());
    assert_ne!(rx_status & 0x1, 0, "RX descriptor DD should be set");
    assert_ne!(rx_status & 0x2, 0, "RX descriptor EOP should be set");
}

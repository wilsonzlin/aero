#![cfg(not(target_arch = "wasm32"))]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::pc::PcMachine;
use aero_machine::RunExit;
use aero_net_backend::NetworkBackend;
use memory::MemoryBus as _;

const ICR_TXDW: u32 = 1 << 0;
const ICR_RXT0: u32 = 1 << 7;
const MIN_L2_FRAME_LEN: usize = 14;

#[derive(Clone)]
struct TestNetworkBackend {
    tx_frames: Arc<Mutex<Vec<Vec<u8>>>>,
    rx_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl TestNetworkBackend {
    fn new() -> Self {
        Self {
            tx_frames: Arc::new(Mutex::new(Vec::new())),
            rx_queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn push_rx(&self, frame: Vec<u8>) {
        self.rx_queue.lock().unwrap().push_back(frame);
    }
}

impl NetworkBackend for TestNetworkBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.tx_frames.lock().unwrap().push(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.rx_queue.lock().unwrap().pop_front()
    }
}

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcMachine, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.bus.platform.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.bus.platform.io.read(PCI_CFG_DATA_PORT, 4)
}

fn write_cfg_u16(pc: &mut PcMachine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.bus.platform.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.bus
        .platform
        .io
        .write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn read_e1000_bar0_base(pc: &mut PcMachine) -> u64 {
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

fn write_u64_le(pc: &mut PcMachine, addr: u64, v: u64) {
    pc.bus
        .platform
        .memory
        .write_physical(addr, &v.to_le_bytes());
}

/// Minimal legacy TX descriptor layout (16 bytes).
fn write_tx_desc(pc: &mut PcMachine, addr: u64, buf_addr: u64, len: u16, cmd: u8, status: u8) {
    write_u64_le(pc, addr, buf_addr);
    pc.bus
        .platform
        .memory
        .write_physical(addr + 8, &len.to_le_bytes());
    pc.bus.platform.memory.write_physical(addr + 10, &[0u8]); // cso
    pc.bus.platform.memory.write_physical(addr + 11, &[cmd]);
    pc.bus.platform.memory.write_physical(addr + 12, &[status]);
    pc.bus.platform.memory.write_physical(addr + 13, &[0u8]); // css
    pc.bus
        .platform
        .memory
        .write_physical(addr + 14, &0u16.to_le_bytes()); // special
}

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(pc: &mut PcMachine, addr: u64, buf_addr: u64, status: u8) {
    write_u64_le(pc, addr, buf_addr);
    pc.bus
        .platform
        .memory
        .write_physical(addr + 8, &0u16.to_le_bytes()); // length
    pc.bus
        .platform
        .memory
        .write_physical(addr + 10, &0u16.to_le_bytes()); // checksum
    pc.bus.platform.memory.write_physical(addr + 12, &[status]);
    pc.bus.platform.memory.write_physical(addr + 13, &[0u8]); // errors
    pc.bus
        .platform
        .memory
        .write_physical(addr + 14, &0u16.to_le_bytes()); // special
}

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(MIN_L2_FRAME_LEN + payload.len());
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
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

#[test]
fn pc_machine_pumps_e1000_frames_through_network_backend() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    // Create the PC machine, but swap in a platform that includes the E1000 device.
    let mut pc = PcMachine::new(RAM_SIZE);
    pc.bus =
        aero_pc_platform::PcCpuBus::new(aero_pc_platform::PcPlatform::new_with_e1000(RAM_SIZE));

    // Park the CPU in a halted loop so `run_slice` continues to poll devices without executing
    // arbitrary guest memory.
    let code_base = 0x7000u64;
    install_hlt_loop(&mut pc, code_base);
    setup_real_mode_cpu(&mut pc, code_base);
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Attach a test backend and preload a single host->guest RX frame.
    let backend = TestNetworkBackend::new();
    let tx_log = backend.tx_frames.clone();
    let rx_frame = build_test_frame(b"host->guest");
    backend.push_rx(rx_frame.clone());
    pc.set_network_backend(Box::new(backend));

    // Locate BAR0 for the E1000 MMIO window (assigned during BIOS POST).
    let bar0_base = read_e1000_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned during BIOS POST");

    // Enable IO + MEM decoding and Bus Mastering (required for DMA in `process_e1000`).
    let bdf = NIC_E1000_82540EM.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0007);

    // Enable interrupts for both RX and TX (not strictly required for frame pumping, but it ensures
    // the device asserts INTx like a real guest driver configuration would).
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + 0x00D0, ICR_RXT0 | ICR_TXDW); // IMS

    // Configure TX ring: 4 descriptors at 0x1000.
    pc.bus.platform.memory.write_u32(bar0_base + 0x3800, 0x1000); // TDBAL
    pc.bus.platform.memory.write_u32(bar0_base + 0x3804, 0); // TDBAH
    pc.bus.platform.memory.write_u32(bar0_base + 0x3808, 4 * 16); // TDLEN
    pc.bus.platform.memory.write_u32(bar0_base + 0x3810, 0); // TDH
    pc.bus.platform.memory.write_u32(bar0_base + 0x3818, 0); // TDT
    pc.bus.platform.memory.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN

    // Configure RX ring: 2 descriptors at 0x2000.
    pc.bus.platform.memory.write_u32(bar0_base + 0x2800, 0x2000); // RDBAL
    pc.bus.platform.memory.write_u32(bar0_base + 0x2804, 0); // RDBAH
    pc.bus.platform.memory.write_u32(bar0_base + 0x2808, 2 * 16); // RDLEN
    pc.bus.platform.memory.write_u32(bar0_base + 0x2810, 0); // RDH
    pc.bus.platform.memory.write_u32(bar0_base + 0x2818, 1); // RDT
    pc.bus.platform.memory.write_u32(bar0_base + 0x0100, 1 << 1); // RCTL.EN

    // Populate RX descriptors with guest buffers.
    write_rx_desc(&mut pc, 0x2000, 0x3000, 0);
    write_rx_desc(&mut pc, 0x2010, 0x3400, 0);

    // Guest TX: descriptor 0 points at packet buffer 0x4000.
    let pkt_out = build_test_frame(b"guest->host");
    pc.bus.platform.memory.write_physical(0x4000, &pkt_out);
    write_tx_desc(
        &mut pc,
        0x1000,
        0x4000,
        pkt_out.len() as u16,
        0b0000_1001, // EOP|RS
        0,
    );

    // Trigger TX by updating tail via MMIO (register-only write). DMA is deferred until the next
    // `process_e1000()` call in the PcMachine run loop.
    pc.bus.platform.memory.write_u32(bar0_base + 0x3818, 1); // TDT = 1

    for _ in 0..10 {
        let _ = pc.run_slice(128);
        if !tx_log.lock().unwrap().is_empty() {
            break;
        }
    }

    let frames = tx_log.lock().unwrap().clone();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], pkt_out);

    // Verify RX delivery into guest memory (buffer and descriptor status bits).
    for _ in 0..10 {
        let _ = pc.run_slice(128);
        let mut status = [0u8; 1];
        pc.bus
            .platform
            .memory
            .read_physical(0x2000 + 12, &mut status);
        if (status[0] & 0x01) != 0 {
            break;
        }
    }

    // Descriptor 0 should be completed with DD|EOP.
    let mut status = [0u8; 1];
    pc.bus
        .platform
        .memory
        .read_physical(0x2000 + 12, &mut status);
    assert_ne!(status[0] & 0x01, 0, "RX desc DD should be set");
    assert_ne!(status[0] & 0x02, 0, "RX desc EOP should be set");

    // Packet should be written to the guest buffer.
    let mut buf = vec![0u8; rx_frame.len()];
    pc.bus.platform.memory.read_physical(0x3000, &mut buf);
    assert_eq!(buf, rx_frame);
}

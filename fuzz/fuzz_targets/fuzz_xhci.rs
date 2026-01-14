#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::hid::{UsbHidKeyboardHandle, UsbHidMouseHandle};
use aero_usb::hub::UsbHubDevice;
use aero_usb::memory::MemoryBus;
use aero_usb::UsbDeviceModel;
use aero_usb::xhci::context::{EndpointContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::{regs, trb::*, XhciController};

const MEM_SIZE: usize = 256 * 1024;
const MAX_OPS: usize = 1024;

// -----------------------------------------------------------------------------
// Minimal xHCI ring seed layout (all within MEM_SIZE).
// -----------------------------------------------------------------------------

const DCBAA_BASE: u64 = 0x1000;
const DEV_CTX_BASE: u64 = 0x2000;
const INPUT_CTX_BASE: u64 = 0x3000;
const CONFIG_INPUT_CTX_BASE: u64 = 0x3800;
const CMD_RING_BASE: u64 = 0x4000;
const EVENT_RING_BASE: u64 = 0x5000;
const ERST_BASE: u64 = 0x6000;
const EP0_TR_BASE: u64 = 0x7000;
const EP1_TR_BASE: u64 = 0x8000;
const EP1_BUF_BASE: u64 = 0x9000;
const EP0_BUF_BASE: u64 = 0xa000;

const ROUTE_KEYBOARD: u32 = 0x1;
const ROUTE_MOUSE: u32 = 0x2;
const ROUTE_HUB: u32 = 0x0;

// xHCI transfer TRB control bits.
const TRB_CTRL_IDT: u32 = 1 << 6;

/// Bounded guest-physical memory for xHCI ring fuzzing.
///
/// Reads outside the provided buffer return `0xff` bytes (unmapped/open-bus), writes are dropped.
#[derive(Clone)]
struct FuzzBus {
    data: Vec<u8>,
    dma: bool,
}

impl FuzzBus {
    fn new(size: usize, init: &[u8]) -> Self {
        let mut data = vec![0u8; size];
        let n = init.len().min(size);
        data[..n].copy_from_slice(&init[..n]);
        Self { data, dma: true }
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        let Ok(addr) = usize::try_from(addr) else {
            return;
        };
        if addr.checked_add(4).is_none() || addr + 4 > self.data.len() {
            return;
        }
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        let Ok(addr) = usize::try_from(addr) else {
            return;
        };
        if addr.checked_add(8).is_none() || addr + 8 > self.data.len() {
            return;
        }
        self.data[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_trb(&mut self, addr: u64, trb: Trb) {
        self.write_physical(addr, &trb.to_bytes());
    }
}

impl MemoryBus for FuzzBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        // Unmapped DMA reads on PC platforms often return all-ones; model that so the controller's
        // InvalidDmaRead (all-0xFF) paths are reachable.
        buf.fill(0xff);
        if buf.is_empty() {
            return;
        }
        if paddr.checked_add(buf.len() as u64).is_none() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let avail = self.data.len() - start;
        let n = avail.min(buf.len());
        buf[..n].copy_from_slice(&self.data[start..start + n]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        if paddr.checked_add(buf.len() as u64).is_none() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let avail = self.data.len() - start;
        let n = avail.min(buf.len());
        self.data[start..start + n].copy_from_slice(&buf[..n]);
    }

    fn dma_enabled(&self) -> bool {
        self.dma
    }
}

fn decode_size(bits: u8) -> usize {
    match bits % 4 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    }
}

fn biased_offset(u: &mut Unstructured<'_>, port_count: usize) -> u64 {
    let sel: u8 = u.arbitrary().unwrap_or(0);
    // ~75% pick a known register; otherwise pick any offset in the MMIO window.
    if sel & 0b11 != 0 {
        match sel % 16 {
            0 => regs::REG_USBCMD,
            1 => regs::REG_USBSTS,
            2 => regs::REG_CRCR_LO,
            3 => regs::REG_DCBAAP_LO,
            4 => regs::REG_CONFIG,
            5 => regs::REG_MFINDEX,
            6 => regs::REG_INTR0_IMAN,
            7 => regs::REG_INTR0_ERSTSZ,
            8 => regs::REG_INTR0_ERSTBA_LO,
            9 => regs::REG_INTR0_ERDP_LO,
            10 => regs::port::portsc_offset(0),
            11 => regs::port::portsc_offset(port_count.saturating_sub(1)),
            12 => u64::from(regs::DBOFF_VALUE), // doorbell 0 (command ring)
            13 => u64::from(regs::DBOFF_VALUE) + u64::from(regs::doorbell::DOORBELL_STRIDE), // slot 1
            14 => {
                u64::from(regs::DBOFF_VALUE) + 2 * u64::from(regs::doorbell::DOORBELL_STRIDE)
            } // slot 2
            _ => regs::port::portsc_offset((sel as usize) % port_count.max(1)),
        }
    } else {
        u.int_in_range(0u64..=(XhciController::MMIO_SIZE as u64).saturating_sub(1))
            .unwrap_or(0)
    }
}

fn biased_paddr(u: &mut Unstructured<'_>) -> u64 {
    let sel: u8 = u.arbitrary().unwrap_or(0);
    // ~75% pick a known structure base; otherwise allow arbitrary offsets in RAM.
    let base = if sel & 0b11 != 0 {
        match sel % 11 {
            0 => DCBAA_BASE,
            1 => DEV_CTX_BASE,
            2 => INPUT_CTX_BASE,
            3 => CONFIG_INPUT_CTX_BASE,
            4 => CMD_RING_BASE,
            5 => EVENT_RING_BASE,
            6 => ERST_BASE,
            7 => EP0_TR_BASE,
            8 => EP1_TR_BASE,
            9 => EP0_BUF_BASE,
            _ => EP1_BUF_BASE,
        }
    } else {
        u.int_in_range(0u64..=(MEM_SIZE as u64).saturating_sub(1))
            .unwrap_or(0)
    };

    let off: u16 = u.arbitrary().unwrap_or(0);
    let paddr = base.saturating_add(u64::from(off) & 0x1ff);
    paddr.min((MEM_SIZE as u64).saturating_sub(1))
}

fn seed_controller_state(bus: &mut FuzzBus, xhci: &mut XhciController) {
    // --- Guest memory structures ---
    //
    // Device context EP0 starts with MPS=8 (matches real device default).
    bus.write_u32(DEV_CTX_BASE + 0x20 + 4, 8u32 << 16);

    // Input control context: Drop=0, Add = Slot + EP0.
    bus.write_u32(INPUT_CTX_BASE, 0);
    bus.write_u32(INPUT_CTX_BASE + 0x04, (1 << 0) | (1 << 1));

    // Input Slot Context: bind to roothub port 1 (the 0th port in the controller model).
    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    // Route string 0x1 == hub port 1 behind the root port. This matches the hub topology we attach
    // in the fuzzer harness (root port 1 -> hub -> port 1 -> keyboard).
    slot_ctx.set_route_string(ROUTE_KEYBOARD);
    slot_ctx.set_context_entries(1);
    slot_ctx.write_to(bus, INPUT_CTX_BASE + CONTEXT_SIZE as u64);

    // Input EP0 context requests MPS=64 and Interval=5.
    bus.write_u32(INPUT_CTX_BASE + 0x40, 5u32 << 16);
    // Endpoint type=Control, MPS=64.
    bus.write_u32(INPUT_CTX_BASE + 0x40 + 4, (4u32 << 3) | (64u32 << 16));
    // TR Dequeue Pointer (masked to 16-byte alignment) + DCS=1 so an all-zero ring reads as empty.
    let tr_raw = EP0_TR_BASE | 1;
    bus.write_u32(INPUT_CTX_BASE + 0x40 + 8, tr_raw as u32);
    bus.write_u32(INPUT_CTX_BASE + 0x40 + 12, (tr_raw >> 32) as u32);

    // Configure Endpoint input context (separate region so Address Device's ICC flags remain valid).
    // Add Slot Context + EP1 IN (endpoint_id=3).
    bus.write_u32(CONFIG_INPUT_CTX_BASE, 0);
    bus.write_u32(CONFIG_INPUT_CTX_BASE + 0x04, (1 << 0) | (1 << 3));
    {
        let mut slot_ctx = SlotContext::default();
        slot_ctx.set_root_hub_port_number(1);
        slot_ctx.set_route_string(ROUTE_KEYBOARD);
        // EP1 IN is context index 3, so ContextEntries should be >= 3.
        slot_ctx.set_context_entries(3);
        slot_ctx.write_to(bus, CONFIG_INPUT_CTX_BASE + CONTEXT_SIZE as u64);
    }
    {
        // Endpoint context for EP1 IN (endpoint_id=3) lives at (endpoint_id + 1) * CONTEXT_SIZE
        // within the input context (see xHCI spec / `cmd_configure_endpoint`).
        const EP1_IN_ENDPOINT_ID: u8 = 3;
        let input_off = (u64::from(EP1_IN_ENDPOINT_ID) + 1) * CONTEXT_SIZE as u64;
        let mut ep_ctx = EndpointContext::default();
        ep_ctx.set_interval(1);
        // Endpoint type=Interrupt IN (7), MPS=8 (boot keyboard report size).
        ep_ctx.set_dword(1, (7u32 << 3) | (8u32 << 16));
        ep_ctx.set_tr_dequeue_pointer(EP1_TR_BASE, true);
        ep_ctx.write_to(bus, CONFIG_INPUT_CTX_BASE + input_off);
    }

    // Seed an EP0 control TD (SET_CONFIGURATION(1)) so interrupt transfers can return data.
    {
        let setup_param = u64::from_le_bytes([0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let mut setup = Trb::new(setup_param, 0, 0);
        setup.set_trb_type(TrbType::SetupStage);
        setup.set_cycle(true);
        bus.write_trb(EP0_TR_BASE, setup);
    }
    {
        let mut status = Trb::new(0, 0, 0);
        status.set_trb_type(TrbType::StatusStage);
        status.set_dir_in(true);
        status.control |= Trb::CONTROL_IOC_BIT;
        status.set_cycle(true);
        bus.write_trb(EP0_TR_BASE + TRB_LEN as u64, status);
    }
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOp);
        stop.set_cycle(false);
        bus.write_trb(EP0_TR_BASE + 2 * TRB_LEN as u64, stop);
    }

    // Leave EP1 IN transfer ring empty by default (cycle=0 sentinel).
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOp);
        stop.set_cycle(false);
        bus.write_trb(EP1_TR_BASE, stop);
    }

    // Command ring:
    //  - TRB0: Enable Slot (cycle=1)
    //  - TRB1: cycle=0 sentinel (ring empty after TRB0)
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        bus.write_trb(CMD_RING_BASE, trb0);
    }
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.set_cycle(false);
        bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, stop);
    }

    // Event Ring Segment Table (ERST) with a single segment pointing at EVENT_RING_BASE.
    bus.write_u64(ERST_BASE, EVENT_RING_BASE);
    bus.write_u32(ERST_BASE + 8, 16); // segment size in TRBs
    bus.write_u32(ERST_BASE + 12, 0);

    // --- Controller MMIO programming ---
    xhci.mmio_write(regs::REG_DCBAAP_LO, 4, DCBAA_BASE);
    xhci.mmio_write(regs::REG_DCBAAP_HI, 4, DCBAA_BASE >> 32);
    xhci.mmio_write(regs::REG_CONFIG, 4, 8); // MaxSlotsEn

    // Command ring base + RCS=1.
    xhci.mmio_write(regs::REG_CRCR_LO, 4, CMD_RING_BASE | 1);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, CMD_RING_BASE >> 32);

    // Program interrupter 0 event ring.
    xhci.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(regs::IMAN_IE));
    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, ERST_BASE);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, ERST_BASE >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, EVENT_RING_BASE >> 32);

    // Start controller and run one tick to service the synthetic RUN-transition DMA/IRQ.
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
}

#[derive(Clone, Copy, Debug)]
enum CommandRingSeed {
    EnableSlot,
    AddressDeviceAndEvaluateContext,
    AddressDeviceAndEvaluateContextMouse,
    AddressDeviceAndEvaluateContextHub,
    ConfigureEndpointEp1In,
    EndpointCommandsEp1In,
    EndpointCommandsEp0,
}

fn rearm_command_ring(bus: &mut FuzzBus, xhci: &mut XhciController, seed: CommandRingSeed) {
    // Reset command ring state back to TRB0 with cycle=1.
    match seed {
        CommandRingSeed::EnableSlot => {
            {
                let mut trb0 = Trb::new(0, 0, 0);
                trb0.set_trb_type(TrbType::EnableSlotCommand);
                trb0.set_cycle(true);
                bus.write_trb(CMD_RING_BASE, trb0);
            }
            {
                let mut stop = Trb::new(0, 0, 0);
                stop.set_trb_type(TrbType::NoOpCommand);
                stop.set_cycle(false);
                bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, stop);
            }
        }
        CommandRingSeed::AddressDeviceAndEvaluateContext => {
            // Reinstate input Slot Context fields so Address Device has a valid topology binding.
            let mut slot_ctx = SlotContext::default();
            slot_ctx.set_root_hub_port_number(1);
            slot_ctx.set_route_string(ROUTE_KEYBOARD);
            slot_ctx.set_context_entries(1);
            slot_ctx.write_to(bus, INPUT_CTX_BASE + CONTEXT_SIZE as u64);

            // Keep DCBAA[1] pointing at our device context.
            bus.write_u64(DCBAA_BASE + 8, DEV_CTX_BASE);

            // Program Address Device then Evaluate Context for slot 1.
            {
                let mut trb0 = Trb::new(INPUT_CTX_BASE, 0, 0);
                trb0.set_trb_type(TrbType::AddressDeviceCommand);
                trb0.set_slot_id(1);
                trb0.set_address_device_bsr(false);
                trb0.set_cycle(true);
                bus.write_trb(CMD_RING_BASE, trb0);
            }
            {
                let mut trb1 = Trb::new(INPUT_CTX_BASE, 0, 0);
                trb1.set_trb_type(TrbType::EvaluateContextCommand);
                trb1.set_slot_id(1);
                trb1.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, trb1);
            }
            {
                let mut stop = Trb::new(0, 0, 0);
                stop.set_trb_type(TrbType::NoOpCommand);
                stop.set_cycle(false);
                bus.write_trb(CMD_RING_BASE + 2 * TRB_LEN as u64, stop);
            }
        }
        CommandRingSeed::AddressDeviceAndEvaluateContextMouse => {
            let mut slot_ctx = SlotContext::default();
            slot_ctx.set_root_hub_port_number(1);
            // Route string 0x2 == hub port 2 behind the root port (mouse).
            slot_ctx.set_route_string(ROUTE_MOUSE);
            slot_ctx.set_context_entries(1);
            slot_ctx.write_to(bus, INPUT_CTX_BASE + CONTEXT_SIZE as u64);

            bus.write_u64(DCBAA_BASE + 8, DEV_CTX_BASE);

            {
                let mut trb0 = Trb::new(INPUT_CTX_BASE, 0, 0);
                trb0.set_trb_type(TrbType::AddressDeviceCommand);
                trb0.set_slot_id(1);
                trb0.set_address_device_bsr(false);
                trb0.set_cycle(true);
                bus.write_trb(CMD_RING_BASE, trb0);
            }
            {
                let mut trb1 = Trb::new(INPUT_CTX_BASE, 0, 0);
                trb1.set_trb_type(TrbType::EvaluateContextCommand);
                trb1.set_slot_id(1);
                trb1.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, trb1);
            }
            {
                let mut stop = Trb::new(0, 0, 0);
                stop.set_trb_type(TrbType::NoOpCommand);
                stop.set_cycle(false);
                bus.write_trb(CMD_RING_BASE + 2 * TRB_LEN as u64, stop);
            }
        }
        CommandRingSeed::AddressDeviceAndEvaluateContextHub => {
            // Bind slot 1 to the external hub itself (route string empty).
            let mut slot_ctx = SlotContext::default();
            slot_ctx.set_root_hub_port_number(1);
            slot_ctx.set_route_string(ROUTE_HUB);
            slot_ctx.set_context_entries(1);
            slot_ctx.write_to(bus, INPUT_CTX_BASE + CONTEXT_SIZE as u64);

            bus.write_u64(DCBAA_BASE + 8, DEV_CTX_BASE);

            {
                let mut trb0 = Trb::new(INPUT_CTX_BASE, 0, 0);
                trb0.set_trb_type(TrbType::AddressDeviceCommand);
                trb0.set_slot_id(1);
                trb0.set_address_device_bsr(false);
                trb0.set_cycle(true);
                bus.write_trb(CMD_RING_BASE, trb0);
            }
            {
                let mut trb1 = Trb::new(INPUT_CTX_BASE, 0, 0);
                trb1.set_trb_type(TrbType::EvaluateContextCommand);
                trb1.set_slot_id(1);
                trb1.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, trb1);
            }
            {
                let mut stop = Trb::new(0, 0, 0);
                stop.set_trb_type(TrbType::NoOpCommand);
                stop.set_cycle(false);
                bus.write_trb(CMD_RING_BASE + 2 * TRB_LEN as u64, stop);
            }
        }
        CommandRingSeed::ConfigureEndpointEp1In => {
            // Program Configure Endpoint (slot 1, config input context) to enable EP1 IN.
            let mut trb0 = Trb::new(CONFIG_INPUT_CTX_BASE, 0, 0);
            trb0.set_trb_type(TrbType::ConfigureEndpointCommand);
            trb0.set_slot_id(1);
            trb0.set_cycle(true);
            bus.write_trb(CMD_RING_BASE, trb0);

            let mut stop = Trb::new(0, 0, 0);
            stop.set_trb_type(TrbType::NoOpCommand);
            stop.set_cycle(false);
            bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, stop);
        }
        CommandRingSeed::EndpointCommandsEp1In => {
            // Stop/Reset/SetTRDP for EP1 IN (slot 1, endpoint_id=3) to exercise endpoint-management
            // commands.
            const SLOT_ID: u8 = 1;
            const EP_ID: u8 = 3;
            {
                let mut trb0 = Trb::new(0, 0, 0);
                trb0.set_trb_type(TrbType::StopEndpointCommand);
                trb0.set_slot_id(SLOT_ID);
                trb0.set_endpoint_id(EP_ID);
                trb0.set_cycle(true);
                bus.write_trb(CMD_RING_BASE, trb0);
            }
            {
                let mut trb1 = Trb::new(0, 0, 0);
                trb1.set_trb_type(TrbType::ResetEndpointCommand);
                trb1.set_slot_id(SLOT_ID);
                trb1.set_endpoint_id(EP_ID);
                trb1.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, trb1);
            }
            {
                let mut trb2 = Trb::new(EP1_TR_BASE | 1, 0, 0);
                trb2.set_trb_type(TrbType::SetTrDequeuePointerCommand);
                trb2.set_slot_id(SLOT_ID);
                trb2.set_endpoint_id(EP_ID);
                trb2.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + 2 * TRB_LEN as u64, trb2);
            }
            {
                let mut stop = Trb::new(0, 0, 0);
                stop.set_trb_type(TrbType::NoOpCommand);
                stop.set_cycle(false);
                bus.write_trb(CMD_RING_BASE + 3 * TRB_LEN as u64, stop);
            }
        }
        CommandRingSeed::EndpointCommandsEp0 => {
            // Stop/Reset/SetTRDP for EP0 (slot 1, endpoint_id=1). This helps fuzzing recover from
            // halted/stopped control endpoint state and exercises endpoint-management commands.
            const SLOT_ID: u8 = 1;
            const EP_ID: u8 = 1;
            {
                let mut trb0 = Trb::new(0, 0, 0);
                trb0.set_trb_type(TrbType::StopEndpointCommand);
                trb0.set_slot_id(SLOT_ID);
                trb0.set_endpoint_id(EP_ID);
                trb0.set_cycle(true);
                bus.write_trb(CMD_RING_BASE, trb0);
            }
            {
                let mut trb1 = Trb::new(0, 0, 0);
                trb1.set_trb_type(TrbType::ResetEndpointCommand);
                trb1.set_slot_id(SLOT_ID);
                trb1.set_endpoint_id(EP_ID);
                trb1.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, trb1);
            }
            {
                let mut trb2 = Trb::new(EP0_TR_BASE | 1, 0, 0);
                trb2.set_trb_type(TrbType::SetTrDequeuePointerCommand);
                trb2.set_slot_id(SLOT_ID);
                trb2.set_endpoint_id(EP_ID);
                trb2.set_cycle(true);
                bus.write_trb(CMD_RING_BASE + 2 * TRB_LEN as u64, trb2);
            }
            {
                let mut stop = Trb::new(0, 0, 0);
                stop.set_trb_type(TrbType::NoOpCommand);
                stop.set_cycle(false);
                bus.write_trb(CMD_RING_BASE + 3 * TRB_LEN as u64, stop);
            }
        }
    }
    xhci.mmio_write(regs::REG_CRCR_LO, 4, CMD_RING_BASE | 1);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, CMD_RING_BASE >> 32);
    xhci.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);
}

fn seed_ep0_control_td(
    bus: &mut FuzzBus,
    trb_ptr: u64,
    cycle: bool,
    setup_bytes: [u8; 8],
    data: Option<(bool /*dir_in*/, bool /*idt*/, &[u8])>,
) {
    let mut offset = 0u64;

    // Setup Stage.
    let setup_param = u64::from_le_bytes(setup_bytes);
    let mut setup = Trb::new(setup_param, 0, 0);
    setup.set_trb_type(TrbType::SetupStage);
    setup.set_cycle(cycle);
    bus.write_trb(trb_ptr + offset, setup);
    offset = offset.saturating_add(TRB_LEN as u64);

    // Optional Data Stage.
    let mut status_dir_in = (setup_bytes[0] & 0x80) == 0;
    if let Some((dir_in, idt, payload)) = data {
        let mut len = payload.len().min(256);
        if idt {
            len = len.min(8);
        }
        let len_u32 = (len as u32).min(Trb::STATUS_TRANSFER_LEN_MASK);

        let mut data_trb = Trb::new(EP0_BUF_BASE, len_u32, 0);
        data_trb.set_trb_type(TrbType::DataStage);
        data_trb.set_cycle(cycle);
        data_trb.set_dir_in(dir_in);
        if idt {
            data_trb.control |= TRB_CTRL_IDT;
            // When IDT is set, the payload lives in the TRB parameter field.
            let mut imm = [0u8; 8];
            imm[..len.min(8)].copy_from_slice(&payload[..len.min(8)]);
            data_trb.parameter = u64::from_le_bytes(imm);
        } else {
            bus.write_physical(EP0_BUF_BASE, &payload[..len]);
        }
        bus.write_trb(trb_ptr + offset, data_trb);
        offset = offset.saturating_add(TRB_LEN as u64);
        // Status stage direction is opposite of the data stage direction.
        status_dir_in = !dir_in;
    }

    // Status Stage (always IOC so we post an event).
    let mut status = Trb::new(0, 0, 0);
    status.set_trb_type(TrbType::StatusStage);
    status.set_dir_in(status_dir_in);
    status.control |= Trb::CONTROL_IOC_BIT;
    status.set_cycle(cycle);
    bus.write_trb(trb_ptr + offset, status);
    offset = offset.saturating_add(TRB_LEN as u64);

    // Sentinel (cycle mismatch => ring empty).
    let mut stop = Trb::new(0, 0, 0);
    stop.set_trb_type(TrbType::NoOp);
    stop.set_cycle(!cycle);
    bus.write_trb(trb_ptr + offset, stop);
}

fn seed_ep1_in_td(bus: &mut FuzzBus, trb_ptr: u64, cycle: bool, len: u32) {
    let len = len.min(Trb::STATUS_TRANSFER_LEN_MASK);

    let mut trb0 = Trb::new(EP1_BUF_BASE, len, 0);
    trb0.set_trb_type(TrbType::Normal);
    trb0.control |= Trb::CONTROL_IOC_BIT;
    trb0.set_cycle(cycle);
    bus.write_trb(trb_ptr, trb0);

    let mut stop = Trb::new(0, 0, 0);
    stop.set_trb_type(TrbType::NoOp);
    stop.set_cycle(!cycle);
    bus.write_trb(trb_ptr + TRB_LEN as u64, stop);
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let mut bus = FuzzBus::new(MEM_SIZE, data);

    let mut xhci = XhciController::new();

    // Attach a USB HID keyboard so port snapshots include nested device trees and we can inject key
    // events during fuzzing.
    let kbd = UsbHidKeyboardHandle::new();
    let mouse = UsbHidMouseHandle::new();
    // Mirror the browser runtime topology: an external hub on root port 0 with a synthetic keyboard
    // on hub port 1 and a synthetic mouse on hub port 2.
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(kbd.clone()));
    hub.attach(2, Box::new(mouse.clone()));
    xhci.attach_device(0, Box::new(hub));

    seed_controller_state(&mut bus, &mut xhci);

    // Tick once to execute the deferred DMA-on-RUN probe and drive port timers.
    xhci.tick_1ms(&mut bus);

    // Clear the synthetic RUN-transition IRQ so later command/event interrupts are easier to
    // distinguish.
    xhci.mmio_write(regs::REG_USBSTS, 4, u64::from(regs::USBSTS_EINT));

    // Ring doorbell 0 to process the command ring (Enable Slot).
    xhci.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);
    xhci.tick_1ms(&mut bus);

    // Enable Slot clears DCBAA[1] to 0; install the device context pointer after it completes so
    // subsequent commands have a valid output context target.
    bus.write_u64(DCBAA_BASE + 8, DEV_CTX_BASE);

    let db1 = u64::from(regs::DBOFF_VALUE) + u64::from(regs::doorbell::DOORBELL_STRIDE);
    let setup_set_configuration = [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];

    // Bind slot 1 to the hub itself and poll its interrupt endpoint. This exercises the USB hub
    // device model (port-change bitmap) and xHCI route-string length=0 topology.
    rearm_command_ring(&mut bus, &mut xhci, CommandRingSeed::AddressDeviceAndEvaluateContextHub);
    xhci.tick_1ms(&mut bus);
    rearm_command_ring(&mut bus, &mut xhci, CommandRingSeed::ConfigureEndpointEp1In);
    xhci.tick_1ms(&mut bus);
    if let Some(ep0) = xhci
        .slot_state(1)
        .and_then(|slot| slot.transfer_ring(1))
    {
        seed_ep0_control_td(
            &mut bus,
            ep0.dequeue_ptr(),
            ep0.cycle_state(),
            setup_set_configuration,
            None,
        );
        xhci.mmio_write(db1, 4, 1);
        xhci.tick_1ms(&mut bus);
    }
    if let Some(ep1) = xhci
        .slot_state(1)
        .and_then(|slot| slot.transfer_ring(3))
    {
        seed_ep1_in_td(&mut bus, ep1.dequeue_ptr(), ep1.cycle_state(), 8);
        xhci.mmio_write(db1, 4, 3);
        xhci.tick_1ms(&mut bus);
    }

    // Bind slot 1 to the keyboard and perform one interrupt IN transfer.
    rearm_command_ring(&mut bus, &mut xhci, CommandRingSeed::AddressDeviceAndEvaluateContext);
    xhci.tick_1ms(&mut bus);
    rearm_command_ring(&mut bus, &mut xhci, CommandRingSeed::ConfigureEndpointEp1In);
    xhci.tick_1ms(&mut bus);
    if let Some(ep0) = xhci
        .slot_state(1)
        .and_then(|slot| slot.transfer_ring(1))
    {
        seed_ep0_control_td(
            &mut bus,
            ep0.dequeue_ptr(),
            ep0.cycle_state(),
            setup_set_configuration,
            None,
        );
        xhci.mmio_write(db1, 4, 1);
        xhci.tick_1ms(&mut bus);
    }
    kbd.key_event(0x04, true); // 'A'
    if let Some(ring) = xhci
        .slot_state(1)
        .and_then(|slot| slot.transfer_ring(3))
    {
        seed_ep1_in_td(&mut bus, ring.dequeue_ptr(), ring.cycle_state(), 8);
        xhci.mmio_write(db1, 4, 3);
        xhci.tick_1ms(&mut bus);
    }

    // Switch the slot binding to the mouse and perform one interrupt IN transfer. This exercises
    // xHCI topology rebinding (Route String changes) and the mouse HID report path.
    rearm_command_ring(
        &mut bus,
        &mut xhci,
        CommandRingSeed::AddressDeviceAndEvaluateContextMouse,
    );
    xhci.tick_1ms(&mut bus);
    rearm_command_ring(&mut bus, &mut xhci, CommandRingSeed::ConfigureEndpointEp1In);
    xhci.tick_1ms(&mut bus);
    if let Some(ep0) = xhci
        .slot_state(1)
        .and_then(|slot| slot.transfer_ring(1))
    {
        seed_ep0_control_td(
            &mut bus,
            ep0.dequeue_ptr(),
            ep0.cycle_state(),
            setup_set_configuration,
            None,
        );
        xhci.mmio_write(db1, 4, 1);
        xhci.tick_1ms(&mut bus);
    }

    // Queue one mouse report and poll it via EP1 IN.
    mouse.movement(1, -1);
    if let Some(ring) = xhci
        .slot_state(1)
        .and_then(|slot| slot.transfer_ring(3))
    {
        seed_ep1_in_td(&mut bus, ring.dequeue_ptr(), ring.cycle_state(), 8);
        xhci.mmio_write(db1, 4, 3);
        xhci.tick_1ms(&mut bus);
    }

    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    let port_count = usize::from(xhci.port_count());

    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 13 {
            0 | 1 | 2 => {
                let offset = biased_offset(&mut u, port_count);
                let size = decode_size(tag >> 3);
                let _ = xhci.mmio_read(offset, size);
            }
            3 | 4 | 5 => {
                let offset = biased_offset(&mut u, port_count);
                let size_bits: u8 = u.arbitrary().unwrap_or(0);
                let size = decode_size(size_bits);
                let value: u64 = u.arbitrary().unwrap_or(0);
                xhci.mmio_write(offset, size, value);
            }
            6 => {
                // Run one 1ms tick (drives port timers, command ring, transfer rings, event ring).
                xhci.tick_1ms(&mut bus);
            }
            7 => {
                // Rearm the command ring back to a known, small sequence and ring DB0.
                let mode: u8 = u.arbitrary().unwrap_or(0);
                let seed = match mode % 7 {
                    0 => CommandRingSeed::EnableSlot,
                    1 => CommandRingSeed::AddressDeviceAndEvaluateContext,
                    2 => CommandRingSeed::AddressDeviceAndEvaluateContextMouse,
                    3 => CommandRingSeed::AddressDeviceAndEvaluateContextHub,
                    4 => CommandRingSeed::ConfigureEndpointEp1In,
                    5 => CommandRingSeed::EndpointCommandsEp1In,
                    _ => CommandRingSeed::EndpointCommandsEp0,
                };
                rearm_command_ring(&mut bus, &mut xhci, seed);
                xhci.tick_1ms(&mut bus);
            }
            8 => {
                // Snapshot roundtrip to stress TLV encode/decode and nested device snapshots.
                let snap = xhci.save_state();
                let mut fresh = XhciController::new();
                // Pre-attach the hub+keyboard topology so restored snapshots apply onto the same
                // shared keyboard handle (so key_event injection keeps affecting the attached
                // device after restore).
                let mut hub = UsbHubDevice::new();
                hub.attach(1, Box::new(kbd.clone()));
                hub.attach(2, Box::new(mouse.clone()));
                fresh.attach_device(0, Box::new(hub));
                let _ = fresh.load_state(&snap);
                xhci = fresh;
            }
            9 => {
                let sub: u8 = u.arbitrary().unwrap_or(0);
                if (sub & 1) == 0 {
                    // Toggle DMA availability and inject keyboard events.
                    bus.dma = (sub & 2) != 0;
                    if (sub & 4) == 0 {
                        let usage: u8 = u.arbitrary().unwrap_or(0);
                        let pressed: bool = u.arbitrary().unwrap_or(false);
                        kbd.key_event(usage, pressed);
                    } else {
                        let dx: i8 = u.arbitrary().unwrap_or(0);
                        let dy: i8 = u.arbitrary().unwrap_or(0);
                        mouse.movement(i32::from(dx), i32::from(dy));
                        let wheel: i8 = u.arbitrary().unwrap_or(0);
                        mouse.wheel(i32::from(wheel));
                        let button = 1u8 << (u.arbitrary::<u8>().unwrap_or(0) % 5);
                        let pressed: bool = u.arbitrary().unwrap_or(false);
                        mouse.button_event(button, pressed);
                    }
                } else {
                    // Rearm EP1 IN with a single Normal TRB and ring slot 1's doorbell.
                    if let Some(ring) = xhci
                        .slot_state(1)
                        .and_then(|slot| slot.transfer_ring(3))
                    {
                        let len: u32 = u.arbitrary::<u8>().unwrap_or(0).into();
                        seed_ep1_in_td(&mut bus, ring.dequeue_ptr(), ring.cycle_state(), len);
                        xhci.mmio_write(db1, 4, 3);
                        xhci.tick_1ms(&mut bus);
                    }
                }
            }
            10 => {
                // Rearm EP0 with a randomized control TD (Setup + optional Data + Status), then
                // ring the endpoint doorbell. This exercises the xHCI control-transfer engine and
                // the `AttachedUsbDevice` control state machine.
                if let Some(ring) = xhci
                    .slot_state(1)
                    .and_then(|slot| slot.transfer_ring(1))
                {
                    let setup_bytes: [u8; 8] = u.arbitrary().unwrap_or([0; 8]);
                    let include_data: bool = u.arbitrary().unwrap_or(false);
                    let dir_in = (setup_bytes[0] & 0x80) != 0;
                    let idt: bool = u.arbitrary().unwrap_or(false);
                    let len: usize = u.int_in_range(0usize..=64).unwrap_or(0);
                    let payload = u.bytes(len).unwrap_or(&[]);
                    let data = include_data.then_some((dir_in, idt, payload));
                    seed_ep0_control_td(&mut bus, ring.dequeue_ptr(), ring.cycle_state(), setup_bytes, data);
                    xhci.mmio_write(db1, 4, 1);
                    xhci.tick_1ms(&mut bus);
                }
            }
            11 => {
                // Mutate guest physical memory backing the rings/contexts. This approximates guest
                // software updating descriptors/TRBs between ticks.
                let addr = biased_paddr(&mut u);
                let len: usize = u.int_in_range(0usize..=64).unwrap_or(0);
                let bytes = u.bytes(len).unwrap_or(&[]);
                bus.write_physical(addr, bytes);
            }
            12 => {
                // Detach/attach devices behind the external hub to exercise hub port change state
                // and xHCI topology errors.
                let which: u8 = u.arbitrary().unwrap_or(0);
                let attach = (which & 1) != 0;
                let port = if (which & 2) == 0 { 1u8 } else { 2u8 };

                if let Some(root) = xhci.port_device_mut(0) {
                    let hub_model = root.model_mut();
                    let _ = hub_model.hub_detach_device(port);
                    if attach {
                        if port == 1 {
                            let mut dev = kbd.clone();
                            dev.reset();
                            let _ = hub_model.hub_attach_device(port, Box::new(dev));
                        } else {
                            let mut dev = mouse.clone();
                            dev.reset();
                            let _ = hub_model.hub_attach_device(port, Box::new(dev));
                        }
                    }
                }
            }
            _ => {}
        }
    }
});

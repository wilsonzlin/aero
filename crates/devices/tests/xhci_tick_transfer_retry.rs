use aero_devices::pci::PciDevice;
use aero_devices::usb::xhci::XhciPciDevice;
use aero_platform::address_filter::AddressFilter;
use aero_platform::{memory::MemoryBus, ChipsetState};
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::regs;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::{ControlResponse, MemoryBus as UsbMemoryBus, SetupPacket, UsbDeviceModel};

struct UsbMem<'a>(&'a mut MemoryBus);

impl UsbMemoryBus for UsbMem<'_> {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.0.read_physical(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.0.write_physical(paddr, buf);
    }
}

#[derive(Clone, Copy)]
struct Alloc {
    next: u64,
}

impl Alloc {
    fn new(start: u64) -> Self {
        Self { next: start }
    }

    fn alloc(&mut self, len: u64, align: u64) -> u64 {
        assert!(align.is_power_of_two());
        let addr = (self.next + (align - 1)) & !(align - 1);
        self.next = addr
            .checked_add(len)
            .expect("alloc end overflow")
            .max(addr);
        addr
    }
}

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(buf_ptr, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(cycle);
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + (endpoint_id as u64) * 0x20
}

fn write_endpoint_context(
    mem: &mut impl UsbMemoryBus,
    dev_ctx_base: u64,
    endpoint_id: u8,
    ep_type_raw: u8,
    max_packet_size: u16,
    ring_base: u64,
    dcs: bool,
) {
    let base = endpoint_ctx_addr(dev_ctx_base, endpoint_id);
    // Endpoint state: running (1).
    mem.write_u32(base, 1);
    // Endpoint type + max packet size.
    let dw1 = ((ep_type_raw as u32) << 3) | (u32::from(max_packet_size) << 16);
    mem.write_u32(base + 4, dw1);
    let tr_dequeue_raw = (ring_base & !0x0f) | u64::from(dcs as u8);
    mem.write_u32(base + 8, tr_dequeue_raw as u32);
    mem.write_u32(base + 12, (tr_dequeue_raw >> 32) as u32);
}

fn configure_event_ring(
    ctrl: &mut aero_usb::xhci::XhciController,
    mem: &mut impl UsbMemoryBus,
    erstba: u64,
    ring_base: u64,
    ring_size_trbs: u32,
) {
    // Single ERST entry.
    mem.write_u64(erstba, ring_base);
    mem.write_u32(erstba + 8, ring_size_trbs);
    mem.write_u32(erstba + 12, 0);

    ctrl.mmio_write(mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);
}

#[test]
fn xhci_tick_1ms_retries_active_interrupt_in_without_extra_doorbells() {
    // Configure the HID device so it is allowed to emit interrupt reports.
    let keyboard = UsbHidKeyboardHandle::new();
    let mut kb_cfg = keyboard.clone();
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        kb_cfg.handle_control_request(setup, None),
        ControlResponse::Ack
    );

    let mut dev = XhciPciDevice::default();
    // Enable bus mastering so `tick_1ms` is allowed to DMA (transfer rings + event ring).
    dev.config_mut().set_command((1 << 1) | (1 << 2)); // MEM | BME

    dev.controller_mut().attach_device(0, Box::new(keyboard.clone()));
    // Drain the initial port status change event so the guest event ring starts empty.
    while dev.controller_mut().pop_pending_event().is_some() {}

    let chipset = ChipsetState::new(true);
    let filter = AddressFilter::new(chipset.a20());
    let mut mem = MemoryBus::new(filter, 0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x800, 0x40);
    let dev_ctx = alloc.alloc(0x800, 0x40);
    dev.controller_mut().set_dcbaap(dcbaa);

    let (slot_id, erstba, event_ring, ring_base, buf) = {
        let mut bus = UsbMem(&mut mem);

        let enable = dev.controller_mut().enable_slot(&mut bus);
        assert_eq!(
            enable.completion_code,
            aero_usb::xhci::CommandCompletionCode::Success
        );
        let slot_id = enable.slot_id;
        assert_ne!(slot_id, 0);

        // Populate DCBAA[slot_id] with the device context pointer.
        bus.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

        let mut slot_ctx = SlotContext::default();
        slot_ctx.set_root_hub_port_number(1);
        let addr = dev.controller_mut().address_device(slot_id, slot_ctx);
        assert_eq!(
            addr.completion_code,
            aero_usb::xhci::CommandCompletionCode::Success
        );

        let erstba = alloc.alloc(0x40, 0x10);
        let event_ring = alloc.alloc((TRB_LEN * 16) as u64, 0x10);
        configure_event_ring(dev.controller_mut(), &mut bus, erstba, event_ring, 16);

        // Endpoint 1 IN => endpoint id 3.
        const EP_ID: u8 = 3;
        let ring_base = alloc.alloc(TRB_LEN as u64, 0x10);
        let buf = alloc.alloc(8, 0x10);
        write_endpoint_context(&mut bus, dev_ctx, EP_ID, 7, 8, ring_base, true);
        make_normal_trb(buf, 8, true, true).write_to(&mut bus, ring_base);

        (slot_id, erstba, event_ring, ring_base, buf)
    };

    // Ring doorbell once to mark the endpoint active.
    const EP_ID: u8 = 3;
    dev.controller_mut().ring_doorbell(slot_id, EP_ID);

    // First tick: no report available yet, so the device should NAK and the TRB should remain
    // pending (dequeue pointer unchanged).
    dev.tick_1ms(&mut mem);
    {
        let mut bus = UsbMem(&mut mem);
        let ev0 = Trb::read_from(&mut bus, event_ring);
        assert_ne!(ev0.trb_type(), TrbType::TransferEvent);

        assert_eq!(
            bus.read_u64(endpoint_ctx_addr(dev_ctx, EP_ID) + 8),
            (ring_base & !0x0f) | 1,
            "NAK must not advance the Endpoint Context dequeue pointer"
        );
    }

    // Produce a keypress. The active endpoint should be retried on the next tick without requiring
    // the guest to ring the doorbell again.
    keyboard.key_event(0x04, true);
    dev.tick_1ms(&mut mem);

    let mut got = [0u8; 8];
    {
        let mut bus = UsbMem(&mut mem);
        bus.read_physical(buf, &mut got);
        assert_eq!(got, [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]);

        // Dequeue pointer should have advanced by one TRB.
        assert_eq!(
            bus.read_u64(endpoint_ctx_addr(dev_ctx, EP_ID) + 8),
            ((ring_base + TRB_LEN as u64) & !0x0f) | 1,
            "completion should advance the Endpoint Context dequeue pointer"
        );

        let ev = Trb::read_from(&mut bus, event_ring);
        assert_eq!(ev.trb_type(), TrbType::TransferEvent);
        assert!(ev.cycle());
        assert_eq!(ev.slot_id(), slot_id);
        assert_eq!(ev.endpoint_id(), EP_ID);
        assert_eq!(ev.pointer(), ring_base);
        assert_eq!(ev.completion_code_raw(), 1); // Success
        assert_eq!(ev.status & 0x00ff_ffff, 0); // residual
    }

    // Silence unused variables in case future changes inline the helper.
    let _ = erstba;
}

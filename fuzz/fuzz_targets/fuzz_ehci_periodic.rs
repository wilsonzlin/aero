#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::ehci::{regs::*, EhciController};
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::memory::MemoryBus;

const MEM_SIZE: usize = 256 * 1024;
const MAX_OPS: usize = 1024;

// -----------------------------------------------------------------------------
// Minimal EHCI periodic schedule seed layout (all within MEM_SIZE).
// -----------------------------------------------------------------------------

const PERIODICLIST_BASE: u32 = 0x1000;

const QH0_ADDR: u32 = 0x2000; // dev_addr=0, ep0 (GET_DESCRIPTOR + SET_ADDRESS)
const QH1_ADDR: u32 = 0x2040; // dev_addr=1, ep0 (SET_CONFIGURATION)
const QH2_ADDR: u32 = 0x2080; // dev_addr=1, ep1 (interrupt IN)

const QTD_GET_DESC_SETUP: u32 = 0x3000;
const QTD_GET_DESC_DATA: u32 = 0x3020;
const QTD_GET_DESC_STATUS: u32 = 0x3040;
const QTD_SET_ADDR_SETUP: u32 = 0x3060;
const QTD_SET_ADDR_STATUS: u32 = 0x3080;
const QTD_SET_CONF_SETUP: u32 = 0x30a0;
const QTD_SET_CONF_STATUS: u32 = 0x30c0;
const QTD_INTR_IN: u32 = 0x30e0;

const SETUP_BUF_BASE: u32 = 0x4000;
const SETUP_BUF_GET_DESC: u32 = SETUP_BUF_BASE + 0x00;
const SETUP_BUF_SET_ADDR: u32 = SETUP_BUF_BASE + 0x10;
const SETUP_BUF_SET_CONF: u32 = SETUP_BUF_BASE + 0x20;

const DESC_BUF_ADDR: u32 = 0x5000;
const INTR_BUF_ADDR: u32 = 0x5100;

// EHCI link pointers / token bits (mirrors `aero-usb` internals).
const LINK_TERMINATE: u32 = 1 << 0;
const LINK_TYPE_QH: u32 = 0b01 << 1;
const LINK_ADDR_MASK: u32 = 0xffff_ffe0;

const QH_HORIZ: u32 = 0x00;
const QH_EPCHAR: u32 = 0x04;
const QH_EPCAPS: u32 = 0x08;
const QH_CUR_QTD: u32 = 0x0c;
const QH_NEXT_QTD: u32 = 0x10;
const QH_ALT_NEXT_QTD: u32 = 0x14;

const QTD_NEXT: u32 = 0x00;
const QTD_ALT_NEXT: u32 = 0x04;
const QTD_TOKEN: u32 = 0x08;
const QTD_BUF0: u32 = 0x0c;

const QTD_STS_ACTIVE: u32 = 1 << 7;
const QTD_PID_SHIFT: u32 = 8;
const QTD_PID_OUT: u32 = 0b00 << QTD_PID_SHIFT;
const QTD_PID_IN: u32 = 0b01 << QTD_PID_SHIFT;
const QTD_PID_SETUP: u32 = 0b10 << QTD_PID_SHIFT;
const QTD_IOC: u32 = 1 << 15;
const QTD_TOTAL_BYTES_SHIFT: u32 = 16;

const SPEED_HIGH: u32 = 2;

fn horiz_qh(addr: u32) -> u32 {
    (addr & LINK_ADDR_MASK) | LINK_TYPE_QH
}

fn qtd_ptr(addr: u32) -> u32 {
    addr & LINK_ADDR_MASK
}

fn make_epchar(dev_addr: u8, ep: u8, max_packet: u16) -> u32 {
    (dev_addr as u32)
        | ((ep as u32) << 8)
        | (SPEED_HIGH << 12)
        | ((max_packet as u32) << 16)
}

fn make_token(pid: u32, total_bytes: u16, ioc: bool) -> u32 {
    let mut v = QTD_STS_ACTIVE | pid | ((total_bytes as u32) << QTD_TOTAL_BYTES_SHIFT);
    if ioc {
        v |= QTD_IOC;
    }
    v
}

/// Simple bounded guest-physical memory implementation for EHCI schedule fuzzing.
///
/// Reads outside the provided buffer return zeros; writes are dropped.
#[derive(Clone)]
struct FuzzBus {
    data: Vec<u8>,
}

impl FuzzBus {
    fn new(size: usize, init: &[u8]) -> Self {
        let mut data = vec![0u8; size];
        let n = init.len().min(size);
        data[..n].copy_from_slice(&init[..n]);
        Self { data }
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        let addr = addr as usize;
        if addr.checked_add(4).is_none() || addr + 4 > self.data.len() {
            return;
        }
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_bytes(&mut self, addr: u32, bytes: &[u8]) {
        let addr = addr as usize;
        if bytes.is_empty() {
            return;
        }
        if addr.checked_add(bytes.len()).is_none() || addr + bytes.len() > self.data.len() {
            return;
        }
        self.data[addr..addr + bytes.len()].copy_from_slice(bytes);
    }
}

impl MemoryBus for FuzzBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        buf.fill(0);
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
}

fn decode_size(bits: u8) -> usize {
    match bits % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn biased_offset(u: &mut Unstructured<'_>, num_ports: usize) -> u64 {
    let sel: u8 = u.arbitrary().unwrap_or(0);
    // ~75% of the time, pick from known registers; otherwise allow arbitrary offsets within the
    // EHCI MMIO window.
    if sel & 0b11 != 0 {
        match sel % 11 {
            0 => REG_USBCMD,
            1 => REG_USBSTS,
            2 => REG_USBINTR,
            3 => REG_FRINDEX,
            4 => REG_PERIODICLISTBASE,
            5 => REG_CONFIGFLAG,
            6 => REG_USBLEGSUP,
            7 => REG_USBLEGCTLSTS,
            8 => reg_portsc(0),
            9 => reg_portsc(num_ports.saturating_sub(1)),
            _ => reg_portsc(sel as usize % num_ports.max(1)),
        }
    } else {
        u.int_in_range(0u64..=(MMIO_SIZE as u64).saturating_sub(1))
            .unwrap_or(0)
    }
}

fn seed_periodic_schedule(bus: &mut FuzzBus) {
    // Periodic frame list: all entries point at QH0.
    for i in 0..1024u32 {
        bus.write_u32(PERIODICLIST_BASE + i * 4, horiz_qh(QH0_ADDR));
    }

    // QH chain:
    //   QH0 (addr 0, ep0): GET_DESCRIPTOR(Device) + SET_ADDRESS(1)
    //     -> QH1 (addr 1, ep0): SET_CONFIGURATION(1)
    //       -> QH2 (addr 1, ep1): interrupt IN (boot keyboard report)
    //         -> terminate
    bus.write_u32(QH0_ADDR + QH_HORIZ, horiz_qh(QH1_ADDR));
    bus.write_u32(QH1_ADDR + QH_HORIZ, horiz_qh(QH2_ADDR));
    bus.write_u32(QH2_ADDR + QH_HORIZ, LINK_TERMINATE);

    // Run in every microframe (SMASK=0) and ignore split/LS fields.
    for qh in [QH0_ADDR, QH1_ADDR, QH2_ADDR] {
        bus.write_u32(qh + QH_EPCAPS, 0);
        // Clear fields used by the async schedule overlay to keep state deterministic even if the
        // guest toggles USBCMD.ASE and accidentally runs the async engine.
        bus.write_u32(qh + QH_CUR_QTD, 0);
        bus.write_u32(qh + QH_ALT_NEXT_QTD, LINK_TERMINATE);
    }

    bus.write_u32(QH0_ADDR + QH_EPCHAR, make_epchar(0, 0, 64));
    bus.write_u32(QH1_ADDR + QH_EPCHAR, make_epchar(1, 0, 64));
    bus.write_u32(QH2_ADDR + QH_EPCHAR, make_epchar(1, 1, 8));

    // QH0 qTD chain (dev_addr=0): GET_DESCRIPTOR -> SET_ADDRESS.
    bus.write_u32(QH0_ADDR + QH_NEXT_QTD, qtd_ptr(QTD_GET_DESC_SETUP));
    bus.write_u32(QTD_GET_DESC_SETUP + QTD_NEXT, qtd_ptr(QTD_GET_DESC_DATA));
    bus.write_u32(QTD_GET_DESC_SETUP + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_GET_DESC_SETUP + QTD_TOKEN,
        make_token(QTD_PID_SETUP, 8, false),
    );
    bus.write_u32(QTD_GET_DESC_SETUP + QTD_BUF0, SETUP_BUF_GET_DESC);

    bus.write_u32(QTD_GET_DESC_DATA + QTD_NEXT, qtd_ptr(QTD_GET_DESC_STATUS));
    bus.write_u32(QTD_GET_DESC_DATA + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_GET_DESC_DATA + QTD_TOKEN,
        make_token(QTD_PID_IN, 18, false),
    );
    bus.write_u32(QTD_GET_DESC_DATA + QTD_BUF0, DESC_BUF_ADDR);

    bus.write_u32(QTD_GET_DESC_STATUS + QTD_NEXT, qtd_ptr(QTD_SET_ADDR_SETUP));
    bus.write_u32(QTD_GET_DESC_STATUS + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_GET_DESC_STATUS + QTD_TOKEN,
        make_token(QTD_PID_OUT, 0, false),
    );
    bus.write_u32(QTD_GET_DESC_STATUS + QTD_BUF0, 0);

    bus.write_u32(QTD_SET_ADDR_SETUP + QTD_NEXT, qtd_ptr(QTD_SET_ADDR_STATUS));
    bus.write_u32(QTD_SET_ADDR_SETUP + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_SET_ADDR_SETUP + QTD_TOKEN,
        make_token(QTD_PID_SETUP, 8, false),
    );
    bus.write_u32(QTD_SET_ADDR_SETUP + QTD_BUF0, SETUP_BUF_SET_ADDR);

    bus.write_u32(QTD_SET_ADDR_STATUS + QTD_NEXT, LINK_TERMINATE);
    bus.write_u32(QTD_SET_ADDR_STATUS + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_SET_ADDR_STATUS + QTD_TOKEN,
        make_token(QTD_PID_IN, 0, false),
    );
    bus.write_u32(QTD_SET_ADDR_STATUS + QTD_BUF0, 0);

    // QH1 qTD chain (dev_addr=1): SET_CONFIGURATION(1).
    bus.write_u32(QH1_ADDR + QH_NEXT_QTD, qtd_ptr(QTD_SET_CONF_SETUP));
    bus.write_u32(QTD_SET_CONF_SETUP + QTD_NEXT, qtd_ptr(QTD_SET_CONF_STATUS));
    bus.write_u32(QTD_SET_CONF_SETUP + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_SET_CONF_SETUP + QTD_TOKEN,
        make_token(QTD_PID_SETUP, 8, false),
    );
    bus.write_u32(QTD_SET_CONF_SETUP + QTD_BUF0, SETUP_BUF_SET_CONF);

    bus.write_u32(QTD_SET_CONF_STATUS + QTD_NEXT, LINK_TERMINATE);
    bus.write_u32(QTD_SET_CONF_STATUS + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_SET_CONF_STATUS + QTD_TOKEN,
        make_token(QTD_PID_IN, 0, false),
    );
    bus.write_u32(QTD_SET_CONF_STATUS + QTD_BUF0, 0);

    // QH2 qTD chain (dev_addr=1, ep=1): single interrupt IN qTD. It will NAK until a key is
    // enqueued.
    bus.write_u32(QH2_ADDR + QH_NEXT_QTD, qtd_ptr(QTD_INTR_IN));
    bus.write_u32(QTD_INTR_IN + QTD_NEXT, LINK_TERMINATE);
    bus.write_u32(QTD_INTR_IN + QTD_ALT_NEXT, LINK_TERMINATE);
    bus.write_u32(
        QTD_INTR_IN + QTD_TOKEN,
        make_token(QTD_PID_IN, 8, true),
    );
    bus.write_u32(QTD_INTR_IN + QTD_BUF0, INTR_BUF_ADDR);

    // Standard setup packets.
    bus.write_bytes(
        SETUP_BUF_GET_DESC,
        &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00],
    );
    bus.write_bytes(
        SETUP_BUF_SET_ADDR,
        &[0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    bus.write_bytes(
        SETUP_BUF_SET_CONF,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

fn rearm_interrupt_qtd(bus: &mut FuzzBus) {
    bus.write_u32(QTD_INTR_IN + QTD_TOKEN, make_token(QTD_PID_IN, 8, true));
    bus.write_u32(QTD_INTR_IN + QTD_BUF0, INTR_BUF_ADDR);
    bus.write_u32(QTD_INTR_IN + QTD_NEXT, LINK_TERMINATE);
    bus.write_u32(QTD_INTR_IN + QTD_ALT_NEXT, LINK_TERMINATE);

    // Periodic engine tracks progress by advancing QH.Next qTD Pointer.
    bus.write_u32(QH2_ADDR + QH_NEXT_QTD, qtd_ptr(QTD_INTR_IN));
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let mut bus = FuzzBus::new(MEM_SIZE, data);

    // Seed a tiny, valid periodic schedule so we exercise EHCI periodic qTD processing even when
    // `data` does not contain a well-formed frame list/QH/qTD structure.
    seed_periodic_schedule(&mut bus);

    let mut ctl = EhciController::new();

    let kbd = UsbHidKeyboardHandle::new();
    ctl.hub_mut().attach(0, Box::new(kbd.clone()));

    // Claim ports for EHCI and enable port 0 (keep PORTSC.PP set so we don't power the port off).
    ctl.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ctl.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PED);

    // Point the controller at our periodic frame list and run.
    ctl.mmio_write(REG_PERIODICLISTBASE, 4, PERIODICLIST_BASE);
    ctl.mmio_write(REG_USBCMD, 4, USBCMD_RS | USBCMD_PSE);

    // Tick once to run enumeration and observe an initial NAK on the interrupt IN qTD.
    ctl.tick_1ms(&mut bus);

    // Enqueue a keyboard report after configuration so the interrupt endpoint returns DATA on the
    // next tick (as `UsbHidKeyboard` drops reports while unconfigured).
    kbd.key_event(0x04, true); // 'A' usage ID

    ctl.tick_1ms(&mut bus);

    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    let ports = ctl.hub().num_ports();
    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 9 {
            0 | 1 | 2 => {
                let offset = biased_offset(&mut u, ports);
                let size = decode_size(tag >> 3);
                let _ = ctl.mmio_read(offset, size);
            }
            3 | 4 | 5 => {
                let offset = biased_offset(&mut u, ports);
                let size_bits: u8 = u.arbitrary().unwrap_or(0);
                let size = decode_size(size_bits);
                let value: u32 = u.arbitrary().unwrap_or(0);
                ctl.mmio_write(offset, size, value);
            }
            6 => {
                rearm_interrupt_qtd(&mut bus);
                ctl.tick_1ms(&mut bus);
            }
            7 => {
                // Snapshot roundtrip to stress TLV encode/decode and nested hub snapshots.
                let snap = ctl.save_state();
                let mut fresh = EhciController::new();
                // Pre-attach the keyboard so the restored snapshot applies onto the same Rc handle
                // (preserves out-of-band key-event injection).
                fresh.hub_mut().attach(0, Box::new(kbd.clone()));
                let _ = fresh.load_state(&snap);
                ctl = fresh;
            }
            _ => {
                // Inject key events to generate interrupt IN traffic.
                let usage: u8 = u.arbitrary().unwrap_or(0);
                let pressed: bool = u.arbitrary().unwrap_or(false);
                kbd.key_event(usage, pressed);
            }
        }
    }
});


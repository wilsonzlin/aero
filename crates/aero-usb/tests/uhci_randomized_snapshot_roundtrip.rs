use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::device::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use aero_usb::hid::{UsbHidKeyboardHandle, UsbHidMouseHandle};
use aero_usb::uhci::regs;
use aero_usb::uhci::UhciController;
use aero_usb::{MemoryBus, SetupPacket};

#[derive(Clone)]
struct BoundedMemory {
    data: Vec<u8>,
}

impl BoundedMemory {
    const SIZE: usize = 64 * 1024;

    fn new() -> Self {
        // Fill with non-zero bytes so uninitialized frame list / TD/QH structures default to
        // terminated link pointers (bit0=1) and cannot hang the schedule walker if the guest enables
        // the controller with a garbage FLBASEADD.
        Self {
            data: vec![0x01; Self::SIZE],
        }
    }
}

impl MemoryBus for BoundedMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let len = self.data.len() as u64;
        for (i, out) in buf.iter_mut().enumerate() {
            let addr = paddr.wrapping_add(i as u64);
            if addr < len {
                *out = self.data[addr as usize];
            } else {
                *out = 0;
            }
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let len = self.data.len() as u64;
        for (i, &val) in buf.iter().enumerate() {
            let addr = paddr.wrapping_add(i as u64);
            if addr < len {
                self.data[addr as usize] = val;
            }
        }
    }
}

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// splitmix64
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn gen_usize(&mut self, upper: usize) -> usize {
        if upper == 0 {
            return 0;
        }
        (self.next_u32() as usize) % upper
    }

    fn gen_bool_percent(&mut self, pct: u32) -> bool {
        (self.next_u32() % 100) < pct
    }
}

#[derive(Clone, Debug)]
enum Op {
    Write {
        offset: u16,
        size: usize,
        value: u32,
    },
    Read {
        offset: u16,
        size: usize,
    },
    Tick,
}

fn gen_size(rng: &mut Rng) -> usize {
    match rng.next_u32() % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn overlaps_flbaseadd_high_bytes(offset: u16, size: usize) -> bool {
    // UhciController assumes any nonzero FLBASEADD points to a valid frame list. If randomized
    // register writes accidentally set FLBASEADD high bytes, the controller will read out-of-bounds
    // memory and the schedule walker can spin indefinitely on an all-zero link pointer. Avoid
    // writes that touch bytes 2..=3 of FLBASEADD (offsets 0x0A..=0x0B) unless we're performing a
    // full 32-bit write to the FLBASEADD register itself.
    let start = offset as u32;
    let end = start.saturating_add(size.saturating_sub(1) as u32);
    let high_start = (regs::REG_FLBASEADD + 2) as u32;
    let high_end = (regs::REG_FLBASEADD + 3) as u32;
    start <= high_end && end >= high_start
}

fn gen_offset_biased(rng: &mut Rng, bias: &[u16]) -> u16 {
    if rng.gen_bool_percent(80) {
        bias[rng.gen_usize(bias.len())]
    } else {
        // UHCI I/O space is 0x20 bytes wide; sample a bit beyond that to cover "open bus" reads.
        (rng.next_u32() % 0x40) as u16
    }
}

fn gen_write_op(rng: &mut Rng) -> Op {
    const REG_USBCMD_HI: u16 = regs::REG_USBCMD + 1;
    const REG_USBSTS_HI: u16 = regs::REG_USBSTS + 1;
    const REG_USBINTR_HI: u16 = regs::REG_USBINTR + 1;
    const REG_FRNUM_HI: u16 = regs::REG_FRNUM + 1;
    const REG_FLBASEADD_B1: u16 = regs::REG_FLBASEADD + 1;
    const REG_PORTSC1_HI: u16 = regs::REG_PORTSC1 + 1;
    const REG_PORTSC2_HI: u16 = regs::REG_PORTSC2 + 1;

    const WRITE_BIAS: &[u16] = &[
        regs::REG_USBCMD,
        REG_USBCMD_HI,
        regs::REG_USBSTS,
        REG_USBSTS_HI,
        regs::REG_USBINTR,
        REG_USBINTR_HI,
        regs::REG_FRNUM,
        REG_FRNUM_HI,
        regs::REG_FLBASEADD,
        REG_FLBASEADD_B1,
        regs::REG_SOFMOD,
        regs::REG_PORTSC1,
        REG_PORTSC1_HI,
        regs::REG_PORTSC2,
        REG_PORTSC2_HI,
    ];

    loop {
        let offset = gen_offset_biased(rng, WRITE_BIAS);
        let size = gen_size(rng);

        // Never allow writes that could set the high bytes of FLBASEADD (unless it's a well-formed
        // 32-bit write to REG_FLBASEADD).
        if overlaps_flbaseadd_high_bytes(offset, size)
            && !(offset == regs::REG_FLBASEADD && size == 4)
        {
            continue;
        }

        let value = match offset {
            regs::REG_FLBASEADD => {
                if size != 4 {
                    continue;
                }
                // 64KiB memory contains 16 x 4KiB pages; the UHCI frame list consumes exactly one
                // page. Pick either 0 (disabled) or a 4KiB-aligned in-range base.
                if rng.gen_bool_percent(25) {
                    0
                } else {
                    (rng.gen_usize(16) as u32) * 0x1000
                }
            }
            regs::REG_USBCMD | REG_USBCMD_HI => {
                let mut v: u16 = 0;
                if rng.gen_bool_percent(50) {
                    v |= regs::USBCMD_RS;
                }
                if rng.gen_bool_percent(10) {
                    v |= regs::USBCMD_HCRESET;
                }
                if rng.gen_bool_percent(10) {
                    v |= regs::USBCMD_GRESET;
                }
                if rng.gen_bool_percent(10) {
                    v |= regs::USBCMD_EGSM;
                }
                if rng.gen_bool_percent(10) {
                    v |= regs::USBCMD_FGR;
                }
                if rng.gen_bool_percent(50) {
                    v |= regs::USBCMD_CF;
                }
                if rng.gen_bool_percent(80) {
                    v |= regs::USBCMD_MAXP;
                }
                v as u32
            }
            regs::REG_USBINTR | REG_USBINTR_HI => {
                let mut v: u16 = 0;
                if rng.gen_bool_percent(20) {
                    v |= regs::USBINTR_TIMEOUT_CRC;
                }
                if rng.gen_bool_percent(30) {
                    v |= regs::USBINTR_RESUME;
                }
                if rng.gen_bool_percent(30) {
                    v |= regs::USBINTR_IOC;
                }
                if rng.gen_bool_percent(30) {
                    v |= regs::USBINTR_SHORT_PACKET;
                }
                v as u32
            }
            regs::REG_USBSTS | REG_USBSTS_HI => {
                // Write-1-to-clear bits.
                let mut v: u16 = 0;
                if rng.gen_bool_percent(25) {
                    v |= regs::USBSTS_USBINT;
                }
                if rng.gen_bool_percent(25) {
                    v |= regs::USBSTS_USBERRINT;
                }
                if rng.gen_bool_percent(25) {
                    v |= regs::USBSTS_RESUMEDETECT;
                }
                if rng.gen_bool_percent(10) {
                    v |= regs::USBSTS_HSE;
                }
                if rng.gen_bool_percent(10) {
                    v |= regs::USBSTS_HCPROCESSERR;
                }
                v as u32
            }
            regs::REG_FRNUM | REG_FRNUM_HI => (rng.next_u32() & 0x07ff) as u32,
            regs::REG_SOFMOD => (rng.next_u32() & 0xff) as u32,
            regs::REG_PORTSC1 | REG_PORTSC1_HI | regs::REG_PORTSC2 | REG_PORTSC2_HI => {
                const CSC: u16 = 1 << 1;
                const PED: u16 = 1 << 2;
                const PEDC: u16 = 1 << 3;
                const RD: u16 = 1 << 6;
                const PR: u16 = 1 << 9;
                const SUSP: u16 = 1 << 12;
                const RESUME: u16 = 1 << 13;

                let mut v: u16 = 0;
                if rng.gen_bool_percent(20) {
                    v |= CSC;
                }
                if rng.gen_bool_percent(40) {
                    v |= PED;
                }
                if rng.gen_bool_percent(20) {
                    v |= PEDC;
                }
                if rng.gen_bool_percent(10) {
                    v |= RD;
                }
                if rng.gen_bool_percent(10) {
                    v |= PR;
                }
                if rng.gen_bool_percent(20) {
                    v |= SUSP;
                }
                if rng.gen_bool_percent(20) {
                    v |= RESUME;
                }
                v as u32
            }
            _ => rng.next_u32(),
        };

        return Op::Write {
            offset,
            size,
            value,
        };
    }
}

fn gen_read_op(rng: &mut Rng) -> Op {
    const READ_BIAS: &[u16] = &[
        regs::REG_USBCMD,
        regs::REG_USBCMD + 1,
        regs::REG_USBSTS,
        regs::REG_USBSTS + 1,
        regs::REG_USBINTR,
        regs::REG_USBINTR + 1,
        regs::REG_FRNUM,
        regs::REG_FRNUM + 1,
        regs::REG_FLBASEADD,
        regs::REG_FLBASEADD + 1,
        regs::REG_FLBASEADD + 2,
        regs::REG_FLBASEADD + 3,
        regs::REG_SOFMOD,
        regs::REG_PORTSC1,
        regs::REG_PORTSC1 + 1,
        regs::REG_PORTSC2,
        regs::REG_PORTSC2 + 1,
    ];

    Op::Read {
        offset: gen_offset_biased(rng, READ_BIAS),
        size: gen_size(rng),
    }
}

fn gen_op(rng: &mut Rng) -> Op {
    match rng.next_u32() % 100 {
        0..=44 => gen_write_op(rng),
        45..=79 => gen_read_op(rng),
        _ => Op::Tick,
    }
}

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ZLP status stage"
    );
}

fn prepare_resume_detect_edge_case(
    ctrl: &mut UhciController,
    mem: &mut dyn MemoryBus,
    kb: &UsbHidKeyboardHandle,
) {
    // Ensure any outstanding port reset/resume countdowns have completed so we can enter suspend.
    for _ in 0..60 {
        ctrl.tick_1ms(mem);
    }

    // Ensure resume-detect IRQ generation is enabled so missing edge-tracking shows up as an IRQ
    // mismatch after snapshot restore.
    ctrl.io_write(regs::REG_USBINTR, 2, regs::USBINTR_RESUME as u32);

    // (Re-)configure the device and enable remote wakeup. Global/bus resets during the randomized
    // phase may have reset the device model state.
    {
        let mut dev = ctrl
            .hub_mut()
            .port_device_mut(0)
            .expect("keyboard must remain attached on port 0");

        control_no_data(
            &mut dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
        control_no_data(
            &mut dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 1,      // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    // Clear any prior key state so the next press is guaranteed to be a state change.
    kb.key_event(0x04, false); // 'A'

    // Put the port into enabled+suspended state.
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_SUSP: u16 = 1 << 12;
    ctrl.io_write(regs::REG_PORTSC1, 2, (PORTSC_PED | PORTSC_SUSP) as u32);

    // Generate a remote wake event while suspended.
    kb.key_event(0x04, true);
    ctrl.tick_1ms(mem);

    // Resume Detect should now be latched in the port state and have transitioned the controller's
    // internal edge detector to `true`.
    const PORTSC_RD: u16 = 1 << 6;
    let portsc1 = ctrl.io_read(regs::REG_PORTSC1, 2) as u16;
    assert_ne!(portsc1 & PORTSC_RD, 0, "expected PORTSC.RD to latch");

    // Clear the controller-level status bit while leaving the port-level RD asserted. Correct edge
    // tracking should prevent the status bit from re-latching on the next tick.
    ctrl.io_write(regs::REG_USBSTS, 2, regs::USBSTS_RESUMEDETECT as u32);
    let usbsts = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & regs::USBSTS_RESUMEDETECT,
        0,
        "expected USBSTS.RESUMEDETECT to be cleared pre-checkpoint"
    );
}

fn prepare_usbint_ioc_edge_case(ctrl: &mut UhciController, mem: &mut dyn MemoryBus) {
    // Construct a minimal schedule that deterministically asserts USBSTS.USBINT and sets the
    // internal `usbint_causes` bit (IOC), so snapshot/restore must preserve it for IRQ equivalence.
    //
    // This uses a TD that targets a non-existent address while the root port is suspended, so the
    // schedule walker takes the "no device" error path and latches USBINT + IOC.
    const FL_BASE: u32 = 0x2000;
    const TD_ADDR: u32 = 0x3000;

    // Frame list: point frame 0 at our TD, terminate all other frames.
    for i in 0..1024u32 {
        let ptr = if i == 0 { TD_ADDR } else { 1 };
        mem.write_u32(FL_BASE.wrapping_add(i * 4) as u64, ptr);
    }

    // TD layout: link, ctrl/sts, token, buffer.
    const TD_LINK_TERMINATE: u32 = 1;
    const TD_STATUS_ACTIVE: u32 = 1 << 23;
    const TD_CTRL_IOC: u32 = 1 << 24;
    mem.write_u32(TD_ADDR as u64, TD_LINK_TERMINATE);
    mem.write_u32((TD_ADDR + 4) as u64, TD_STATUS_ACTIVE | TD_CTRL_IOC);
    // PID doesn't matter since we intentionally hit the "device missing" path. Use an OUT token
    // with max_len=0 (field=0x7FF).
    let token = 0xE1u32 | (5u32 << 8) | (0x7FFu32 << 21);
    mem.write_u32((TD_ADDR + 8) as u64, token);
    mem.write_u32((TD_ADDR + 12) as u64, 0);

    // Enable IRQ for IOC (but not timeout/CRC) so IRQ level depends on usbint_causes.
    ctrl.io_write(
        regs::REG_USBINTR,
        2,
        (regs::USBINTR_RESUME | regs::USBINTR_IOC) as u32,
    );
    ctrl.io_write(regs::REG_FLBASEADD, 4, FL_BASE);
    ctrl.io_write(regs::REG_FRNUM, 2, 0);
    ctrl.io_write(
        regs::REG_USBCMD,
        2,
        (regs::USBCMD_RS | regs::USBCMD_CF | regs::USBCMD_MAXP) as u32,
    );

    ctrl.tick_1ms(mem);

    let usbsts = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_ne!(
        usbsts & regs::USBSTS_USBINT,
        0,
        "expected USBSTS.USBINT to be set by IOC TD"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ to be asserted due to USBINT+IOC"
    );
}

fn exec_op(ctrl: &mut UhciController, mem: &mut dyn MemoryBus, op: &Op) -> Option<u32> {
    match *op {
        Op::Write {
            offset,
            size,
            value,
        } => {
            ctrl.io_write(offset, size, value);
            None
        }
        Op::Read { offset, size } => Some(ctrl.io_read(offset, size)),
        Op::Tick => {
            ctrl.tick_1ms(mem);
            None
        }
    }
}

#[test]
fn uhci_randomized_snapshot_roundtrip_is_equivalent_after_restore() {
    const SEED: u64 = 0x0324_0000_5EED_0001;
    const STEPS: usize = 5_000;
    const CHECKPOINT: usize = 2_500;

    let mut rng = Rng::new(SEED);
    let mut ops = Vec::with_capacity(STEPS);
    for _ in 0..STEPS {
        ops.push(gen_op(&mut rng));
    }
    // Make the first operation after restore a tick so missing edge-tracking state (e.g.
    // `prev_port_resume_detect`) is caught immediately.
    ops[CHECKPOINT] = Op::Tick;
    // Follow with a USBSTS read so missed edge tracking is observable even if IRQ is already
    // asserted for other reasons.
    ops[CHECKPOINT + 1] = Op::Read {
        offset: regs::REG_USBSTS,
        size: 2,
    };

    let kb = UsbHidKeyboardHandle::new();
    let mouse = UsbHidMouseHandle::new();

    let mut ctrl_a = UhciController::new();
    ctrl_a.hub_mut().attach(0, Box::new(kb.clone()));
    ctrl_a.hub_mut().attach(1, Box::new(mouse));

    let mut mem_a = BoundedMemory::new();

    // Run the randomized prefix on controller A only.
    let mut _prefix_reads = Vec::new();
    for op in &ops[..CHECKPOINT] {
        if let Some(v) = exec_op(&mut ctrl_a, &mut mem_a, op) {
            _prefix_reads.push(v);
        }
    }

    // Force a specific tricky state at the snapshot point: PORTSC.RD asserted and the controller's
    // prev_port_resume_detect latched, but USBSTS.RESUMEDETECT cleared. If snapshot/restore misses
    // the edge-tracking state, the first tick after restore will spuriously re-latch USBSTS and
    // raise an IRQ.
    prepare_resume_detect_edge_case(&mut ctrl_a, &mut mem_a, &kb);
    // Also ensure USBINT bookkeeping is non-zero so `usbint_causes` must roundtrip correctly.
    prepare_usbint_ioc_edge_case(&mut ctrl_a, &mut mem_a);

    let snapshot = ctrl_a.save_state();
    let mut mem_b = mem_a.clone();

    let mut ctrl_b = UhciController::new();
    ctrl_b
        .load_state(&snapshot)
        .expect("uhci snapshot restore should succeed");
    assert_eq!(
        ctrl_a.irq_level(),
        ctrl_b.irq_level(),
        "irq mismatch immediately after restore"
    );

    // Continue executing the same operations on both controllers and assert equivalence.
    for (idx, op) in ops[CHECKPOINT..].iter().enumerate() {
        let step = CHECKPOINT + idx;
        let ra = exec_op(&mut ctrl_a, &mut mem_a, op);
        let rb = exec_op(&mut ctrl_b, &mut mem_b, op);
        assert_eq!(ra, rb, "read mismatch at step {step}: {op:?}");
        assert_eq!(
            ctrl_a.irq_level(),
            ctrl_b.irq_level(),
            "irq mismatch at step {step}: {op:?}"
        );
    }

    assert_eq!(ctrl_a.save_state(), ctrl_b.save_state());
}

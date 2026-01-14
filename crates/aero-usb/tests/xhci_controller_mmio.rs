use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion};
use aero_usb::xhci::{context::SlotContext, regs, CommandCompletion, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

#[derive(Default)]
struct CountingMem {
    data: Vec<u8>,
    reads: usize,
    writes: usize,
}

impl CountingMem {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            reads: 0,
            writes: 0,
        }
    }
}

impl MemoryBus for CountingMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.reads += 1;
        let start = usize::try_from(paddr).expect("paddr should fit in usize");
        let end = start + buf.len();
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.writes += 1;
        let start = usize::try_from(paddr).expect("paddr should fit in usize");
        let end = start + buf.len();
        self.data[start..end].copy_from_slice(buf);
    }
}

#[derive(Default)]
struct NoDmaCountingMem {
    reads: usize,
    writes: usize,
}

impl MemoryBus for NoDmaCountingMem {
    fn dma_enabled(&self) -> bool {
        false
    }

    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        self.reads += 1;
        buf.fill(0xFF);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        self.writes += 1;
    }
}

#[test]
fn xhci_controller_caplength_hciversion_reads() {
    let mut ctrl = XhciController::new();

    assert_eq!(
        ctrl.mmio_read(regs::REG_CAPLENGTH_HCIVERSION, 4) as u32,
        regs::CAPLENGTH_HCIVERSION
    );

    // Byte/word reads should match the LE layout.
    assert_eq!(
        ctrl.mmio_read(regs::REG_CAPLENGTH_HCIVERSION, 1) as u32,
        u32::from(regs::CAPLENGTH_BYTES)
    );
    assert_eq!(
        ctrl.mmio_read(regs::REG_CAPLENGTH_HCIVERSION + 2, 2) as u32,
        0x0100
    );
    assert_eq!(
        ctrl.mmio_read(regs::REG_CAPLENGTH_HCIVERSION + 3, 1) as u32,
        0x01
    );

    // Cross-dword reads should behave like little-endian memory.
    assert_eq!(
        ctrl.mmio_read(regs::REG_CAPLENGTH_HCIVERSION + 3, 2) as u32,
        0x2001
    );
    assert_eq!(
        ctrl.mmio_read(regs::REG_CAPLENGTH_HCIVERSION + 1, 4) as u32,
        0x2001_0000
    );
}

#[test]
fn xhci_controller_dboff_rtsoff_are_plausible() {
    let mut ctrl = XhciController::new();

    let dboff = ctrl.mmio_read(regs::REG_DBOFF, 4) as u32;
    assert_ne!(dboff, 0, "DBOFF should be non-zero");
    assert_eq!(
        dboff & 0x3,
        0,
        "DBOFF must be 4-byte aligned (bits 1:0 reserved)"
    );
    assert!(
        dboff < XhciController::MMIO_SIZE,
        "DBOFF must point within the MMIO window"
    );
    assert!(
        dboff as u64 >= u64::from(regs::CAPLENGTH_BYTES),
        "DBOFF should not overlap the capability register block"
    );

    let rtsoff = ctrl.mmio_read(regs::REG_RTSOFF, 4) as u32;
    assert_ne!(rtsoff, 0, "RTSOFF should be non-zero");
    assert_eq!(
        rtsoff & 0x1f,
        0,
        "RTSOFF must be 32-byte aligned (bits 4:0 reserved)"
    );
    assert!(
        rtsoff < XhciController::MMIO_SIZE,
        "RTSOFF must point within the MMIO window"
    );
    assert!(
        rtsoff as u64 >= u64::from(regs::CAPLENGTH_BYTES),
        "RTSOFF should not overlap the capability register block"
    );
}

#[test]
fn xhci_controller_pagesize_supports_4k_pages() {
    let mut ctrl = XhciController::new();
    assert_eq!(
        ctrl.mmio_read(regs::REG_PAGESIZE, 4) as u32,
        regs::PAGESIZE_4K
    );
}

#[test]
fn xhci_controller_dnctrl_is_writable_and_snapshots() {
    let mut ctrl = XhciController::new();

    assert_eq!(ctrl.mmio_read(regs::REG_DNCTRL, 4) as u32, 0);

    ctrl.mmio_write(regs::REG_DNCTRL, 4, 0x1234_5678);
    assert_eq!(
        ctrl.mmio_read(regs::REG_DNCTRL, 4) as u32,
        0x1234_5678u32,
        "DNCTRL should roundtrip through MMIO reads/writes"
    );

    let bytes = ctrl.save_state();
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    assert_eq!(
        restored.mmio_read(regs::REG_DNCTRL, 4) as u32,
        0x1234_5678u32,
        "DNCTRL should roundtrip through snapshot restore"
    );
}

#[test]
fn xhci_controller_config_and_dnctrl_roundtrip_and_reset() {
    let mut ctrl = XhciController::new();

    assert_eq!(ctrl.mmio_read(regs::REG_CONFIG, 4) as u32, 0);
    assert_eq!(ctrl.mmio_read(regs::REG_DNCTRL, 4) as u32, 0);

    ctrl.mmio_write(regs::REG_CONFIG, 4, 0xa5a5);
    ctrl.mmio_write(regs::REG_DNCTRL, 4, 0x1234_5678);

    // CONFIG has reserved bits and clamps MaxSlotsEn to HCSPARAMS1.MaxSlots.
    let expected_config = ((0xa5a5u32 & 0x3ff) & !0xff) | u32::from(regs::MAX_SLOTS);
    assert_eq!(ctrl.mmio_read(regs::REG_CONFIG, 4) as u32, expected_config);
    assert_eq!(ctrl.mmio_read(regs::REG_DNCTRL, 4) as u32, 0x1234_5678);

    // Host controller reset should clear operational register state.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_HCRST));
    assert_eq!(ctrl.mmio_read(regs::REG_CONFIG, 4) as u32, 0);
    assert_eq!(ctrl.mmio_read(regs::REG_DNCTRL, 4) as u32, 0);
}

#[test]
fn xhci_controller_hcrst_clears_tick_counters() {
    // Snapshot tags for controller-local time and tick-driven DMA probe state.
    const TAG_TIME_MS: u16 = 27;
    const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    mem.data[0x1000..0x1004].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    // Set RCS=1 so the controller must mask off CRCR flags when reading from the ring pointer.
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000 | 1);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    ctrl.tick_1ms_with_dma(&mut mem);

    let before = ctrl.save_state();
    let r = SnapshotReader::parse(&before, *b"XHCI").expect("parse snapshot");
    assert_eq!(r.u64(TAG_TIME_MS).unwrap().unwrap_or(0), 1);
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD).unwrap().unwrap_or(0),
        0xdead_beef
    );

    // Host controller reset should clear controller-local tick bookkeeping.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_HCRST));

    let after = ctrl.save_state();
    let r2 = SnapshotReader::parse(&after, *b"XHCI").expect("parse snapshot after reset");
    assert_eq!(r2.u64(TAG_TIME_MS).unwrap().unwrap_or(0), 0);
    assert_eq!(r2.u32(TAG_LAST_TICK_DMA_DWORD).unwrap().unwrap_or(0), 0);
}

#[test]
fn xhci_controller_tick_dma_dword_is_snapshotted() {
    // Snapshot tags for controller-local time and last tick DMA dword.
    const TAG_TIME_MS: u16 = 27;
    const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Seed the DMA source for the tick-driven "DMA touch-point" at CRCR.
    // Set the RCS flag bit in CRCR_LO to ensure the controller masks off CRCR flags before using
    // the pointer as a guest physical address.
    mem.data[0x1000..0x1004].copy_from_slice(&0xfeed_beefu32.to_le_bytes());
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000 | 1);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);

    // Enable RUN so `tick_1ms_with_dma` will read from CRCR.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms_with_dma(&mut mem);

    let bytes = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse snapshot");
    assert_eq!(
        r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0),
        1,
        "expected internal time to advance by 1ms"
    );
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0xfeed_beef,
        "expected last_tick_dma_dword to be snapshotted"
    );

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    let restored_bytes = restored.save_state();
    let restored_r =
        SnapshotReader::parse(&restored_bytes, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(
        restored_r
            .u64(TAG_TIME_MS)
            .expect("read time_ms")
            .unwrap_or(0),
        1
    );
    assert_eq!(
        restored_r
            .u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0xfeed_beef
    );
}

#[test]
fn xhci_controller_tick_dma_dword_is_gated_by_dma_enabled() {
    // Snapshot tags for controller-local time and last tick DMA dword.
    const TAG_TIME_MS: u16 = 27;
    const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    mem.data[0x1000..0x1004].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms_with_dma(&mut mem);

    // Now tick with a DMA-disabled bus; the controller should still advance time, but should not
    // touch the memory bus or mutate the last_tick_dma_dword value.
    let mut nodma = NoDmaCountingMem::default();
    ctrl.tick_1ms_with_dma(&mut nodma);
    assert_eq!(nodma.reads, 0);
    assert_eq!(nodma.writes, 0);

    let bytes = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse snapshot");
    assert_eq!(r.u64(TAG_TIME_MS).unwrap().unwrap_or(0), 2);
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD).unwrap().unwrap_or(0),
        0xdead_beef
    );
}

#[test]
fn xhci_controller_tick_dma_dword_masks_crcr_flags() {
    // Snapshot tags for controller-local time and last tick DMA dword.
    const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Write a byte pattern that lets us distinguish an aligned read at 0x1000 from an unaligned
    // read at 0x1001.
    mem.data[0x1000..0x1008].copy_from_slice(&[0, 1, 2, 3, 4, 5, 6, 7]);

    // Set CRCR with the ring cycle-state flag (bit 0). The tick DMA read must mask off low flag
    // bits and use the aligned pointer.
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000 | 1);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms_with_dma(&mut mem);

    let bytes = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse snapshot");
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD).unwrap().unwrap_or(0),
        0x0302_0100,
        "expected tick DMA read to use aligned CRCR pointer"
    );
}

#[test]
fn xhci_mfindex_advances_on_tick_1ms_and_wraps() {
    let mut ctrl = XhciController::new();

    assert_eq!(ctrl.mmio_read(regs::REG_MFINDEX, 4) as u32 & 0x3fff, 0);

    ctrl.tick_1ms_no_dma();
    assert_eq!(ctrl.mmio_read(regs::REG_MFINDEX, 4) as u32 & 0x3fff, 8);

    // MFINDEX is 14 bits and counts microframes; 2048ms == 16384 microframes wraps to 0.
    for _ in 0..2047 {
        ctrl.tick_1ms_no_dma();
    }
    assert_eq!(ctrl.mmio_read(regs::REG_MFINDEX, 4) as u32 & 0x3fff, 0);
}

#[test]
fn xhci_controller_tick_triggers_dma_and_w1c_clears_irq() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Seed the DMA target.
    mem.data[0x1000..0x1004].copy_from_slice(&[1, 2, 3, 4]);

    // Program CRCR and start the controller. DMA happens in `tick_1ms`.
    ctrl.mmio_write(regs::REG_CRCR_LO, 8, 0x1000 | 0x1);
    assert_eq!(mem.reads, 0);

    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // DMA is issued on the next tick.
    ctrl.tick_1ms(&mut mem);
    // One DMA read for the deferred DMA-on-RUN probe + one DMA read for the tick-driven CRCR probe.
    assert_eq!(mem.reads, 2);
    assert!(ctrl.irq_level());
    assert_ne!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_EINT,
        0
    );

    // Further ticks should not re-run the deferred DMA-on-RUN probe (no new "run session"), but
    // should still perform the tick-driven CRCR DMA read.
    ctrl.tick_1ms(&mut mem);
    assert_eq!(mem.reads, 3);

    // USBSTS is RW1C: writing 1 clears the pending interrupt.
    ctrl.mmio_write(regs::REG_USBSTS, 4, u64::from(regs::USBSTS_EINT));
    assert!(!ctrl.irq_level());
    assert_eq!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_EINT,
        0
    );

    // Stop then start again -> second DMA touchpoint.
    ctrl.mmio_write(regs::REG_USBCMD, 4, 0);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms(&mut mem);
    assert_eq!(mem.reads, 5);
}

#[test]
fn xhci_controller_run_does_not_dma_when_dma_disabled() {
    let mut ctrl = XhciController::new();
    let mut mem = NoDmaCountingMem::default();
    assert!(
        !ctrl.irq_level(),
        "controller should not assert IRQ by default"
    );

    // Program CRCR and start the controller. The dma-on-RUN probe should be skipped when DMA is
    // disabled, leaving the memory bus untouched.
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Tick once to ensure the controller does not attempt DMA while `dma_enabled()==false`.
    ctrl.tick_1ms(&mut mem);

    assert_eq!(mem.reads, 0);
    assert_eq!(mem.writes, 0);
    assert!(
        !ctrl.irq_level(),
        "dma-on-RUN interrupt must be gated by dma_enabled()"
    );
}

#[test]
fn xhci_controller_run_does_not_dma_when_crcr_is_zero() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Leave CRCR unset (0) and set RUN. The controller should not touch guest memory at address 0
    // while executing the DMA-on-RUN probe.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms_with_dma(&mut mem);

    assert_eq!(mem.reads, 0, "expected no DMA reads when CRCR is unset");
    assert!(
        !ctrl.irq_level(),
        "DMA-on-RUN probe must not assert an interrupt when CRCR is unset"
    );
    assert_eq!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_EINT,
        0,
        "USBSTS.EINT must remain clear when no interrupt is pending"
    );
}

#[test]
fn xhci_snapshot_preserves_pending_dma_on_run_probe() {
    let mut ctrl = XhciController::new();

    // Put the controller into the running state with DMA disabled so the dma-on-RUN probe is
    // deferred. This should leave `pending_dma_on_run` set internally without raising an interrupt.
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    assert!(!ctrl.irq_level());

    let bytes = ctrl.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    assert!(
        !restored.irq_level(),
        "pending dma-on-RUN probe must not be converted into an asserted interrupt during restore"
    );

    let mut mem = CountingMem::new(0x4000);
    mem.data[0x1000..0x1004].copy_from_slice(&0x1234_5678u32.to_le_bytes());

    // The first DMA-capable tick should execute the deferred probe (1 DMA read) and then perform the
    // tick-driven CRCR read (1 DMA read), asserting an interrupt.
    restored.tick_1ms_with_dma(&mut mem);
    assert_eq!(
        mem.reads, 2,
        "expected deferred dma-on-RUN + tick DMA reads on first tick after restore"
    );
    assert!(restored.irq_level());

    // Subsequent ticks should not re-run the deferred probe, but still perform the tick-driven CRCR
    // read.
    restored.tick_1ms_with_dma(&mut mem);
    assert_eq!(mem.reads, 3);
}

#[test]
fn xhci_snapshot_pending_dma_on_run_defaults_to_false_when_missing() {
    // xHCI snapshot v0.9+ persists `pending_dma_on_run`, but older snapshots do not. Ensure restores
    // default the field to false when absent (even if USBCMD.RUN is set).
    const TAG_USBCMD: u16 = 1;
    const TAG_PENDING_DMA_ON_RUN: u16 = 29;

    let mut w =
        aero_io_snapshot::io::state::SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 8));
    w.field_u32(TAG_USBCMD, regs::USBCMD_RUN);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes).expect("load legacy snapshot");

    let saved = ctrl.save_state();
    let r = SnapshotReader::parse(&saved, *b"XHCI").expect("parse restored snapshot");
    assert!(
        !r.bool(TAG_PENDING_DMA_ON_RUN)
            .expect("read pending_dma_on_run")
            .unwrap_or(false),
        "expected pending_dma_on_run to default to false when omitted"
    );
}

#[test]
fn xhci_snapshot_clears_pending_dma_on_run_when_controller_halted() {
    // `pending_dma_on_run` is only meaningful while RUN is set; dropping RUN cancels the deferred
    // probe. Enforce the same invariant on restore so malformed snapshots can't keep the probe
    // armed while halted.
    const TAG_USBCMD: u16 = 1;
    const TAG_PENDING_DMA_ON_RUN: u16 = 29;

    let mut w =
        aero_io_snapshot::io::state::SnapshotWriter::new(*b"XHCI", XhciController::DEVICE_VERSION);
    w.field_u32(TAG_USBCMD, 0);
    w.field_bool(TAG_PENDING_DMA_ON_RUN, true);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes).expect("load snapshot");

    let saved = ctrl.save_state();
    let r = SnapshotReader::parse(&saved, *b"XHCI").expect("parse restored snapshot");
    assert!(
        !r.bool(TAG_PENDING_DMA_ON_RUN)
            .expect("read pending_dma_on_run")
            .unwrap_or(false),
        "expected pending_dma_on_run to be cleared while halted"
    );
}

#[test]
fn xhci_tick_1ms_does_not_dma_when_dma_disabled() {
    let mut ctrl = XhciController::new();
    let mut mem = NoDmaCountingMem::default();

    // Put the controller into the running state without allowing DMA.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    assert_eq!(mem.reads, 0);
    assert_eq!(mem.writes, 0);

    ctrl.tick_1ms(&mut mem);

    // A tick should still advance internal time/port state, but it must not touch guest memory.
    assert_eq!(mem.reads, 0);
    assert_eq!(mem.writes, 0);
}

#[test]
fn xhci_dma_on_run_probe_is_deferred_until_dma_is_available() {
    let mut ctrl = XhciController::new();

    // Program CRCR so the DMA-on-RUN probe has a target address.
    let mut nodma = NoDmaCountingMem::default();
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);

    // Latch the rising edge of RUN via a no-DMA bus. This must not touch guest memory or assert an
    // interrupt yet, but it should leave the probe pending for a future tick.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms(&mut nodma);
    assert_eq!(
        nodma.reads, 0,
        "setting RUN must not DMA when dma_enabled() is false"
    );
    assert_eq!(
        nodma.writes, 0,
        "setting RUN must not DMA-write when dma_enabled() is false"
    );
    assert!(
        !ctrl.irq_level(),
        "DMA-on-RUN interrupt must be deferred until DMA is available"
    );

    // On the next tick with DMA enabled, the deferred DMA-on-RUN probe should execute and assert
    // an interrupt.
    let mut mem = CountingMem::new(0x4000);
    mem.data[0x1000..0x1004].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    ctrl.tick_1ms_with_dma(&mut mem);
    assert!(
        ctrl.irq_level(),
        "tick should execute deferred DMA-on-RUN probe and assert IRQ"
    );
    assert!(
        mem.reads >= 1,
        "expected at least one DMA read when processing deferred DMA-on-RUN probe"
    );

    // Clear the pending interrupt.
    ctrl.mmio_write(regs::REG_USBSTS, 4, u64::from(regs::USBSTS_EINT));
    assert!(!ctrl.irq_level());

    // Subsequent ticks should not re-run the DMA-on-RUN probe (the probe is one-shot).
    ctrl.tick_1ms_with_dma(&mut mem);
    assert!(
        !ctrl.irq_level(),
        "DMA-on-RUN probe should not re-assert after it has completed"
    );
}

#[test]
fn xhci_doorbell_does_not_process_command_ring_without_dma() {
    let mut ctrl = XhciController::new();

    let mut nodma = NoDmaCountingMem::default();
    ctrl.mmio_write(regs::REG_CRCR_LO, 8, 0x1000 | 1);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);
    ctrl.tick_1ms(&mut nodma);

    assert_eq!(nodma.reads, 0);
    assert_eq!(nodma.writes, 0);
}

#[test]
fn xhci_endpoint_doorbell_does_not_process_transfers_without_dma() {
    struct DummyDevice;

    impl UsbDeviceModel for DummyDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }
    }

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(DummyDevice));

    // Use a small in-memory bus while configuring the slot/endpoint state. We later swap in a
    // dma-disabled bus to validate that doorbells do not touch guest memory when DMA is gated.
    let mut mem = CountingMem::new(0x4000);

    // Enable slot 1 so endpoint doorbells have a valid target.
    ctrl.set_dcbaap(0x1000);
    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion, CommandCompletion::success(1));

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    assert_eq!(
        ctrl.address_device(completion.slot_id, slot_ctx),
        completion
    );

    // Configure a plausible endpoint ring cursor for EP1 IN (device context index 3). Leave DCBAAP
    // cleared so the endpoint-state gating logic falls back to controller-local cursor state.
    ctrl.set_endpoint_ring(completion.slot_id, 3, 0x1800, true);
    ctrl.set_dcbaap(0);

    // Start the controller so future run/stop gating changes don't invalidate this test.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    let doorbell = u64::from(regs::DBOFF_VALUE)
        + u64::from(completion.slot_id) * u64::from(regs::doorbell::DOORBELL_STRIDE);

    // With DMA enabled, ringing an endpoint doorbell should cause the controller to fetch transfer
    // ring state from guest memory.
    let reads_before = mem.reads;
    ctrl.mmio_write(doorbell, 4, 3);
    ctrl.tick(&mut mem);
    assert!(
        mem.reads > reads_before,
        "endpoint doorbell should DMA-read transfer ring state when dma_enabled() is true"
    );

    // With DMA disabled, the doorbell handler still kicks `tick()`, but all DMA must be gated.
    let mut nodma = NoDmaCountingMem::default();
    ctrl.mmio_write(doorbell, 4, 3);
    ctrl.tick(&mut nodma);
    assert_eq!(
        nodma.reads, 0,
        "endpoint doorbell must not DMA-read when dma_enabled() is false"
    );
    assert_eq!(
        nodma.writes, 0,
        "endpoint doorbell must not DMA-write when dma_enabled() is false"
    );
}

#[test]
fn xhci_endpoint_doorbell_defers_transfers_until_run() {
    struct DummyDevice;

    impl UsbDeviceModel for DummyDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }
    }

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(DummyDevice));

    let mut mem = CountingMem::new(0x4000);

    // Enable slot 1 so endpoint doorbells have a valid target.
    ctrl.set_dcbaap(0x1000);
    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion, CommandCompletion::success(1));

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    assert_eq!(
        ctrl.address_device(completion.slot_id, slot_ctx),
        completion
    );

    // Configure a plausible endpoint ring cursor for EP1 IN (device context index 3). Leave DCBAAP
    // cleared so the endpoint-state gating logic falls back to controller-local cursor state.
    ctrl.set_endpoint_ring(completion.slot_id, 3, 0x1800, true);
    ctrl.set_dcbaap(0);

    let doorbell = u64::from(regs::DBOFF_VALUE)
        + u64::from(completion.slot_id) * u64::from(regs::doorbell::DOORBELL_STRIDE);

    let reads_before = mem.reads;

    // Ring the endpoint doorbell while the controller is stopped (USBCMD.RUN=0). The doorbell
    // should be remembered, but transfer-ring execution must not DMA or make progress.
    ctrl.mmio_write(doorbell, 4, 3);
    ctrl.tick(&mut mem);
    assert_eq!(
        mem.reads, reads_before,
        "transfer rings must not be serviced while USBCMD.RUN is clear"
    );

    // Once RUN is set, the previously queued doorbell should allow transfer processing to begin
    // without requiring the guest to ring the doorbell again.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick(&mut mem);
    assert!(
        mem.reads > reads_before,
        "expected queued endpoint doorbell to execute once USBCMD.RUN is set"
    );
}

#[test]
fn xhci_doorbell_ignores_halted_endpoint_without_device_context() {
    use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};

    struct DummyDevice;

    impl UsbDeviceModel for DummyDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }
    }

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(DummyDevice));
    while ctrl.pop_pending_event().is_some() {}
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    let mut mem = CountingMem::new(0x4000);

    // Enable slot 1 so endpoint doorbells have a valid target.
    ctrl.set_dcbaap(0x1000);
    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion, CommandCompletion::success(1));
    let slot_id = completion.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    assert_eq!(ctrl.address_device(slot_id, slot_ctx), completion);

    // Configure a controller-local ring cursor for EP1 IN (DCI=3) but clear DCBAAP so the guest
    // Device Context cannot be read. This mimics harness setups that only configure controller-local
    // cursors.
    let endpoint_id = 3u8;
    let ring_base = 0x1800u64;
    ctrl.set_endpoint_ring(slot_id, endpoint_id, ring_base, true);
    ctrl.set_dcbaap(0);

    // Place a malformed Link TRB (cycle=1) with a null segment pointer. This should trigger a TRB
    // error and halt the endpoint.
    let mut bad = Trb::default();
    bad.set_trb_type(TrbType::Link);
    bad.set_cycle(true);
    bad.write_to(&mut mem, ring_base);

    ctrl.ring_doorbell(slot_id, endpoint_id);
    ctrl.tick(&mut mem);

    assert_eq!(ctrl.pending_event_count(), 1);
    let ev = ctrl.pop_pending_event().expect("expected transfer event");
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev.completion_code_raw(), CompletionCode::TrbError.as_u8());
    assert_eq!(ev.slot_id(), slot_id);
    assert_eq!(ev.endpoint_id(), endpoint_id);

    // Even without a readable Device Context, the controller should remember the halted state in its
    // shadow context and ignore subsequent doorbells.
    let slot = ctrl.slot_state(slot_id).expect("slot state");
    let ep_ctx = slot
        .endpoint_context(usize::from(endpoint_id - 1))
        .expect("endpoint context");
    assert_eq!(ep_ctx.endpoint_state(), 2, "expected endpoint to be halted");

    ctrl.ring_doorbell(slot_id, endpoint_id);
    ctrl.tick(&mut mem);
    assert_eq!(
        ctrl.pending_event_count(),
        0,
        "halted endpoint should ignore subsequent doorbells"
    );
}

#[test]
fn xhci_controller_hchalted_tracks_run_stop_and_reset() {
    let mut ctrl = XhciController::new();

    assert_ne!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_HCHALTED,
        0,
        "controller should begin halted"
    );

    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    assert_eq!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_HCHALTED,
        0,
        "setting RUN should clear HCHalted"
    );

    ctrl.mmio_write(regs::REG_USBCMD, 4, 0);
    assert_ne!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_HCHALTED,
        0,
        "clearing RUN should set HCHalted"
    );

    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_HCRST));
    assert_eq!(
        ctrl.mmio_read(regs::REG_USBCMD, 4) as u32 & regs::USBCMD_HCRST,
        0,
        "HCRST should be self-clearing"
    );
    assert_ne!(
        ctrl.mmio_read(regs::REG_USBSTS, 4) as u32 & regs::USBSTS_HCHALTED,
        0,
        "controller should be halted after reset"
    );
}

#[test]
fn xhci_controller_cross_dword_write_splits_into_bytes() {
    let mut ctrl = XhciController::new();

    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1122_3344);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0x5566_7788);

    // Write a u16 spanning CRCR_LO byte 3 and CRCR_HI byte 0.
    ctrl.mmio_write(regs::REG_CRCR_LO + 3, 2, 0xaaaa);

    assert_eq!(ctrl.mmio_read(regs::REG_CRCR_LO, 4) as u32, 0xaa22_3344);
    assert_eq!(ctrl.mmio_read(regs::REG_CRCR_HI, 4) as u32, 0x5566_77aa);
}

#[test]
fn xhci_controller_snapshot_roundtrip_preserves_regs() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Program a few operational registers.
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, 0x1234);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(regs::REG_DNCTRL, 4, 0x1122_3344);
    ctrl.mmio_write(regs::REG_CONFIG, 4, 0xa5a5);

    // Start the controller and advance one tick to trigger the DMA-on-RUN probe so
    // interrupt-related bits are non-zero in the snapshot.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms(&mut mem);

    let bytes = ctrl.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(
        restored.mmio_read(regs::REG_USBCMD, 4) as u32 & regs::USBCMD_RUN,
        regs::USBCMD_RUN
    );
    // CRCR stores a 64-byte-aligned ring pointer; low bits hold flags/cycle state.
    assert_eq!(
        restored.mmio_read(regs::REG_CRCR_LO, 4) as u32,
        (0x1234u32 & !0x3f) | (0x1234u32 & 0x07)
    );
    assert_eq!(restored.mmio_read(regs::REG_DNCTRL, 4) as u32, 0x1122_3344);
    let expected_config = ((0xa5a5u32 & 0x3ff) & !0xff) | u32::from(regs::MAX_SLOTS);
    assert_eq!(
        restored.mmio_read(regs::REG_CONFIG, 4) as u32,
        expected_config
    );
    assert_eq!(restored.mmio_read(regs::REG_MFINDEX, 4) as u32 & 0x3fff, 8);
    assert!(restored.irq_level());
}

#[test]
fn xhci_controller_snapshot_roundtrip_preserves_dcbaap_and_port_count() {
    // Use a non-default port count so we can validate it roundtrips via the HCSPARAMS1 read.
    let mut ctrl = XhciController::with_port_count(4);

    // Program DCBAAP with a deliberately misaligned value; the controller should mask low bits away.
    ctrl.mmio_write(regs::REG_DCBAAP_LO, 4, 0x1234_5678);
    ctrl.mmio_write(regs::REG_DCBAAP_HI, 4, 0x9abc_def0);

    let expected_dcbaap = 0x9abc_def0_1234_5640u64;
    assert_eq!(ctrl.dcbaap(), Some(expected_dcbaap));
    assert_eq!(
        ctrl.mmio_read(regs::REG_DCBAAP_LO, 4) as u32,
        expected_dcbaap as u32
    );
    assert_eq!(
        ctrl.mmio_read(regs::REG_DCBAAP_HI, 4) as u32,
        (expected_dcbaap >> 32) as u32
    );

    // Port count is exposed via HCSPARAMS1 bits 31..=24.
    let hcsparams1 = ctrl.mmio_read(regs::REG_HCSPARAMS1, 4) as u32;
    assert_eq!((hcsparams1 >> 24) & 0xff, 4);

    let bytes = ctrl.save_state();
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.dcbaap(), Some(expected_dcbaap));
    let restored_hcsparams1 = restored.mmio_read(regs::REG_HCSPARAMS1, 4) as u32;
    assert_eq!((restored_hcsparams1 >> 24) & 0xff, 4);
}

#[test]
fn xhci_controller_config_register_is_writable_and_clamped() {
    let mut ctrl = XhciController::new();

    assert_eq!(ctrl.mmio_read(regs::REG_CONFIG, 4) as u32, 0);

    ctrl.mmio_write(regs::REG_CONFIG, 4, 8);
    assert_eq!(ctrl.mmio_read(regs::REG_CONFIG, 4) as u32 & 0xff, 8);

    // Clamp MaxSlotsEn to HCSPARAMS1.MaxSlots.
    ctrl.mmio_write(regs::REG_CONFIG, 1, 0xff);
    let cfg = ctrl.mmio_read(regs::REG_CONFIG, 4) as u32;
    assert_eq!(cfg & 0xff, u32::from(regs::MAX_SLOTS));
    assert_eq!(cfg & !0x3ff, 0, "reserved CONFIG bits should read as 0");
}

#[test]
fn xhci_controller_mfindex_advances() {
    let mut ctrl = XhciController::new();

    let before = ctrl.mmio_read(regs::REG_MFINDEX, 4) as u32 & 0x3fff;
    ctrl.tick_1ms_no_dma();
    let after = ctrl.mmio_read(regs::REG_MFINDEX, 4) as u32 & 0x3fff;
    assert_eq!(after, (before + 8) & 0x3fff);
}

#[test]
fn xhci_controller_portsc_array_bounds() {
    let mut ctrl = XhciController::with_port_count(2);

    let p0 = ctrl.mmio_read(regs::port::portsc_offset(0), 4) as u32;
    let p1 = ctrl.mmio_read(regs::port::portsc_offset(1), 4) as u32;
    assert_ne!(p0 & regs::PORTSC_PP, 0);
    assert_ne!(p1 & regs::PORTSC_PP, 0);

    // Port index 2 is out-of-range for a 2-port controller and should read as 0 (unimplemented).
    assert_eq!(ctrl.mmio_read(regs::port::portsc_offset(2), 4), 0);

    // Writes to out-of-range ports should be ignored.
    ctrl.mmio_write(regs::port::portsc_offset(2), 4, 0xffff_ffff);
    assert_eq!(ctrl.mmio_read(regs::port::portsc_offset(2), 4), 0);
}

#[test]
fn xhci_controller_doorbell_writes_do_not_alias_operational_regs() {
    let mut ctrl = XhciController::new();

    let dboff = ctrl.mmio_read(regs::REG_DBOFF, 4);
    assert_eq!(dboff, u64::from(regs::DBOFF_VALUE));

    ctrl.mmio_write(dboff, 4, 0x1); // DB0
    ctrl.mmio_write(dboff + u64::from(regs::doorbell::DOORBELL_STRIDE), 4, 0x1); // DB1

    // Doorbell writes should not affect the operational register file directly.
    assert_eq!(ctrl.mmio_read(regs::REG_USBCMD, 4), 0);
}

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

#[derive(Default)]
struct PanicMem;

impl MemoryBus for PanicMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write");
    }
}

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
    let mut mem = PanicMem;

    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 4),
        regs::CAPLENGTH_HCIVERSION
    );

    // Byte/word reads should match the LE layout.
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 1),
        u32::from(regs::CAPLENGTH_BYTES)
    );
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION + 2, 2),
        0x0100
    );
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION + 3, 1),
        0x01
    );

    // Cross-dword reads should behave like little-endian memory.
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION + 3, 2),
        0x2001
    );
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION + 1, 4),
        0x2001_0000
    );
}

#[test]
fn xhci_controller_dboff_rtsoff_are_plausible() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    let dboff = ctrl.mmio_read(&mut mem, regs::REG_DBOFF, 4);
    assert_ne!(dboff, 0, "DBOFF should be non-zero");
    assert_eq!(dboff & 0x3, 0, "DBOFF must be 4-byte aligned (bits 1:0 reserved)");
    assert!(
        dboff < XhciController::MMIO_SIZE,
        "DBOFF must point within the MMIO window"
    );
    assert!(
        dboff as u64 >= u64::from(regs::CAPLENGTH_BYTES),
        "DBOFF should not overlap the capability register block"
    );

    let rtsoff = ctrl.mmio_read(&mut mem, regs::REG_RTSOFF, 4);
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
    let mut mem = PanicMem;

    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_PAGESIZE, 4),
        regs::PAGESIZE_4K
    );
}

#[test]
fn xhci_controller_dnctrl_is_writable_and_snapshots() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_DNCTRL, 4), 0);

    ctrl.mmio_write(&mut mem, regs::REG_DNCTRL, 4, 0x1234_5678);
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_DNCTRL, 4),
        0x1234_5678,
        "DNCTRL should roundtrip through MMIO reads/writes"
    );

    let bytes = ctrl.save_state();
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    assert_eq!(
        restored.mmio_read(&mut mem, regs::REG_DNCTRL, 4),
        0x1234_5678,
        "DNCTRL should roundtrip through snapshot restore"
    );
}

#[test]
fn xhci_mfindex_advances_on_tick_1ms_and_wraps() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_MFINDEX, 4) & 0x3fff, 0);

    ctrl.tick_1ms_no_dma();
    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_MFINDEX, 4) & 0x3fff, 8);

    // MFINDEX is 14 bits and counts microframes; 2048ms == 16384 microframes wraps to 0.
    for _ in 0..2047 {
        ctrl.tick_1ms_no_dma();
    }
    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_MFINDEX, 4) & 0x3fff, 0);
}

#[test]
fn xhci_controller_run_triggers_dma_and_w1c_clears_irq() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Seed the DMA target.
    mem.data[0x1000..0x1004].copy_from_slice(&[1, 2, 3, 4]);

    // Program CRCR and start the controller: first RUN transition should DMA once.
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    assert_eq!(mem.reads, 0);

    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(mem.reads, 1);
    assert!(ctrl.irq_level());
    assert_ne!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_EINT,
        0
    );

    // Writing RUN again should not DMA (no rising edge).
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(mem.reads, 1);

    // Stop then start again -> second rising edge DMA.
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, 0);
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(mem.reads, 2);

    // USBSTS is RW1C: writing 1 clears the pending interrupt.
    ctrl.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!ctrl.irq_level());
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_EINT,
        0
    );
}

#[test]
fn xhci_controller_run_does_not_dma_when_dma_disabled() {
    let mut ctrl = XhciController::new();
    let mut mem = NoDmaCountingMem::default();
    assert!(!ctrl.irq_level(), "controller should not assert IRQ by default");

    // Program CRCR and start the controller. The dma-on-RUN probe should be skipped when DMA is
    // disabled, leaving the memory bus untouched.
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    assert_eq!(mem.reads, 0);
    assert_eq!(mem.writes, 0);
    assert!(!ctrl.irq_level(), "dma-on-RUN interrupt must be gated by dma_enabled()");
}

#[test]
fn xhci_doorbell_does_not_process_command_ring_without_dma() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Point CRCR at a plausible guest physical address and set RCS=1 so the controller would
    // normally attempt to fetch a command TRB when doorbell 0 is rung.
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1000 | 1);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);

    // Start the controller so doorbell processing would run.
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    let mut nodma = NoDmaCountingMem::default();
    ctrl.mmio_write(&mut nodma, u64::from(regs::DBOFF_VALUE), 4, 0);

    assert_eq!(nodma.reads, 0);
    assert_eq!(nodma.writes, 0);
}

#[test]
fn xhci_controller_hchalted_tracks_run_stop_and_reset() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x100);

    assert_ne!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_HCHALTED,
        0,
        "controller should begin halted"
    );

    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_HCHALTED,
        0,
        "setting RUN should clear HCHalted"
    );

    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, 0);
    assert_ne!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_HCHALTED,
        0,
        "clearing RUN should set HCHalted"
    );

    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_HCRST);
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_USBCMD, 4) & regs::USBCMD_HCRST,
        0,
        "HCRST should be self-clearing"
    );
    assert_ne!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_HCHALTED,
        0,
        "controller should be halted after reset"
    );
}

#[test]
fn xhci_controller_cross_dword_write_splits_into_bytes() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1122_3344);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0x5566_7788);

    // Write a u16 spanning CRCR_LO byte 3 and CRCR_HI byte 0.
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO + 3, 2, 0xaaaa);

    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_CRCR_LO, 4), 0xaa22_3344);
    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_CRCR_HI, 4), 0x5566_77aa);
}

#[test]
fn xhci_controller_snapshot_roundtrip_preserves_regs() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1234);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    let bytes = ctrl.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.mmio_read(&mut mem, regs::REG_USBCMD, 4), regs::USBCMD_RUN);
    // CRCR stores a 64-byte-aligned ring pointer; low bits hold flags/cycle state.
    assert_eq!(
        restored.mmio_read(&mut mem, regs::REG_CRCR_LO, 4),
        (0x1234 & !0x3f) | (0x1234 & 0x0f)
    );
    assert!(restored.irq_level());
}

#[test]
fn xhci_controller_snapshot_roundtrip_preserves_dcbaap_and_port_count() {
    // Use a non-default port count so we can validate it roundtrips via the HCSPARAMS1 read.
    let mut ctrl = XhciController::with_port_count(4);
    let mut mem = PanicMem;

    // Program DCBAAP with a deliberately misaligned value; the controller should mask low bits away.
    ctrl.mmio_write(&mut mem, regs::REG_DCBAAP_LO, 4, 0x1234_5678);
    ctrl.mmio_write(&mut mem, regs::REG_DCBAAP_HI, 4, 0x9abc_def0);

    let expected_dcbaap = 0x9abc_def0_1234_5640u64;
    assert_eq!(ctrl.dcbaap(), Some(expected_dcbaap));
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_DCBAAP_LO, 4),
        expected_dcbaap as u32
    );
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_DCBAAP_HI, 4),
        (expected_dcbaap >> 32) as u32
    );

    // Port count is exposed via HCSPARAMS1 bits 31..=24.
    let hcsparams1 = ctrl.mmio_read(&mut mem, regs::REG_HCSPARAMS1, 4);
    assert_eq!((hcsparams1 >> 24) & 0xff, 4);

    let bytes = ctrl.save_state();
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.dcbaap(), Some(expected_dcbaap));
    let restored_hcsparams1 = restored.mmio_read(&mut mem, regs::REG_HCSPARAMS1, 4);
    assert_eq!((restored_hcsparams1 >> 24) & 0xff, 4);
}

#[test]
fn xhci_controller_config_register_is_writable_and_clamped() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_CONFIG, 4), 0);

    ctrl.mmio_write(&mut mem, regs::REG_CONFIG, 4, 8);
    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_CONFIG, 4) & 0xff, 8);

    // Clamp MaxSlotsEn to HCSPARAMS1.MaxSlots.
    ctrl.mmio_write(&mut mem, regs::REG_CONFIG, 1, 0xff);
    let cfg = ctrl.mmio_read(&mut mem, regs::REG_CONFIG, 4);
    assert_eq!(cfg & 0xff, u32::from(regs::MAX_SLOTS));
    assert_eq!(cfg & !0x3ff, 0, "reserved CONFIG bits should read as 0");
}

#[test]
fn xhci_controller_mfindex_advances() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    let before = ctrl.mmio_read(&mut mem, regs::REG_MFINDEX, 4) & 0x3fff;
    ctrl.tick_1ms_no_dma();
    let after = ctrl.mmio_read(&mut mem, regs::REG_MFINDEX, 4) & 0x3fff;
    assert_eq!(after, (before + 8) & 0x3fff);
}

#[test]
fn xhci_controller_portsc_array_bounds() {
    let mut ctrl = XhciController::with_port_count(2);
    let mut mem = PanicMem;

    let p0 = ctrl.mmio_read(&mut mem, regs::port::portsc_offset(0), 4);
    let p1 = ctrl.mmio_read(&mut mem, regs::port::portsc_offset(1), 4);
    assert_ne!(p0 & regs::PORTSC_PP, 0);
    assert_ne!(p1 & regs::PORTSC_PP, 0);

    // Port index 2 is out-of-range for a 2-port controller and should read as 0 (unimplemented).
    assert_eq!(ctrl.mmio_read(&mut mem, regs::port::portsc_offset(2), 4), 0);

    // Writes to out-of-range ports should be ignored.
    ctrl.mmio_write(&mut mem, regs::port::portsc_offset(2), 4, 0xffff_ffff);
    assert_eq!(ctrl.mmio_read(&mut mem, regs::port::portsc_offset(2), 4), 0);
}

#[test]
fn xhci_controller_doorbell_writes_do_not_alias_operational_regs() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    let dboff = ctrl.mmio_read(&mut mem, regs::REG_DBOFF, 4) as u64;
    assert_eq!(dboff, u64::from(regs::DBOFF_VALUE));

    ctrl.mmio_write(&mut mem, dboff, 4, 0x1); // DB0
    ctrl.mmio_write(
        &mut mem,
        dboff + u64::from(regs::doorbell::DOORBELL_STRIDE),
        4,
        0x1,
    ); // DB1

    // Doorbell writes should not affect the operational register file directly.
    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_USBCMD, 4), 0);
}

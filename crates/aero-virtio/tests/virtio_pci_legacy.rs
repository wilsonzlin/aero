use aero_virtio::devices::blk::{MemDisk, VirtioBlk, VIRTIO_BLK_T_FLUSH};
use aero_virtio::memory::{
    read_u16_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
};
use aero_virtio::pci::{
    InterruptSink, VirtioPciDevice, VIRTIO_PCI_LEGACY_GUEST_FEATURES,
    VIRTIO_PCI_LEGACY_HOST_FEATURES, VIRTIO_PCI_LEGACY_ISR, VIRTIO_PCI_LEGACY_ISR_QUEUE,
    VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, VIRTIO_PCI_LEGACY_QUEUE_NUM, VIRTIO_PCI_LEGACY_QUEUE_PFN,
    VIRTIO_PCI_LEGACY_QUEUE_SEL, VIRTIO_PCI_LEGACY_STATUS, VIRTIO_PCI_LEGACY_VRING_ALIGN,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Default)]
struct IrqState {
    asserted: bool,
    raises: u64,
    lowers: u64,
    msix_vectors: Vec<u16>,
}

#[derive(Clone, Default)]
struct SharedIrq(Rc<RefCell<IrqState>>);

impl InterruptSink for SharedIrq {
    fn raise_legacy_irq(&mut self) {
        let mut state = self.0.borrow_mut();
        state.asserted = true;
        state.raises += 1;
    }

    fn lower_legacy_irq(&mut self) {
        let mut state = self.0.borrow_mut();
        state.asserted = false;
        state.lowers += 1;
    }

    fn signal_msix(&mut self, vector: u16) {
        self.0.borrow_mut().msix_vectors.push(vector);
    }
}

fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

fn legacy_vring_addrs(base: u64, qsz: u16) -> (u64, u64, u64) {
    let desc = base;
    let avail = desc + 16 * u64::from(qsz);
    let used_unaligned = avail + 4 + 2 * u64::from(qsz) + 2;
    let used = align_up(used_unaligned, VIRTIO_PCI_LEGACY_VRING_ALIGN);
    (desc, avail, used)
}

fn write_desc(
    mem: &mut GuestRam,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).unwrap();
    write_u32_le(mem, base + 8, len).unwrap();
    write_u16_le(mem, base + 12, flags).unwrap();
    write_u16_le(mem, base + 14, next).unwrap();
}

#[test]
fn virtio_pci_legacy_pfn_queue_and_isr_read_clears() {
    let blk = VirtioBlk::new(MemDisk::new(4096));
    let irq_state = Rc::new(RefCell::new(IrqState::default()));
    let irq = SharedIrq(irq_state.clone());

    // Use a *transitional* device (legacy + modern) but exercise the legacy path.
    let mut dev = VirtioPciDevice::new_transitional(Box::new(blk), Box::new(irq));
    let mut mem = GuestRam::new(0x20000);

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2), including legacy virtqueue processing.
    dev.config_write(0x04, &0x0004u16.to_le_bytes());

    // 1) Read device features and accept them (legacy path exposes only low 32 bits).
    let mut f = [0u8; 4];
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_HOST_FEATURES, &mut f);
    let host_features = u32::from_le_bytes(f);
    // For this test, disable EVENT_IDX so interrupt behavior is purely based on
    // `VIRTQ_AVAIL_F_NO_INTERRUPT` (simplifies the legacy ISR/INTx assertions).
    let guest_features = host_features & !(1u32 << 29);

    dev.legacy_io_write(VIRTIO_PCI_LEGACY_STATUS, &[VIRTIO_STATUS_ACKNOWLEDGE]);
    dev.legacy_io_write(
        VIRTIO_PCI_LEGACY_STATUS,
        &[VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER],
    );
    dev.legacy_io_write(
        VIRTIO_PCI_LEGACY_GUEST_FEATURES,
        &guest_features.to_le_bytes(),
    );

    // 2) Configure queue 0 using PFN.
    dev.legacy_io_write(VIRTIO_PCI_LEGACY_QUEUE_SEL, &0u16.to_le_bytes());
    let mut qsz_bytes = [0u8; 2];
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_QUEUE_NUM, &mut qsz_bytes);
    let qsz = u16::from_le_bytes(qsz_bytes);
    assert!(qsz >= 8);

    let ring_base = 0x4000u64; // 4096-aligned
    let pfn = u32::try_from(ring_base >> 12).unwrap();
    dev.legacy_io_write(VIRTIO_PCI_LEGACY_QUEUE_PFN, &pfn.to_le_bytes());

    let mut pfn_back = [0u8; 4];
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_QUEUE_PFN, &mut pfn_back);
    assert_eq!(u32::from_le_bytes(pfn_back), pfn);

    dev.legacy_io_write(
        VIRTIO_PCI_LEGACY_STATUS,
        &[VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK],
    );

    // 3) Build a minimal FLUSH request chain and kick.
    let (desc, avail, used) = legacy_vring_addrs(ring_base, qsz);

    let header = 0x1000;
    let status = 0x2000;
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, desc, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut mem, desc, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();

    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.legacy_io_write(VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    assert!(irq_state.borrow().asserted);
    assert_eq!(irq_state.borrow().raises, 1);

    // 4) ISR is read-to-clear and should deassert INTx.
    let mut isr = [0u8; 1];
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_ISR, &mut isr);
    assert_eq!(
        isr[0] & VIRTIO_PCI_LEGACY_ISR_QUEUE,
        VIRTIO_PCI_LEGACY_ISR_QUEUE
    );
    assert!(!irq_state.borrow().asserted);

    dev.legacy_io_read(VIRTIO_PCI_LEGACY_ISR, &mut isr);
    assert_eq!(isr[0], 0);

    // 5) Completing another request should assert INTx again (verifies deassert).
    mem.write(status, &[0xff]).unwrap();
    write_u16_le(&mut mem, avail + 4 + 2, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 2).unwrap();
    dev.legacy_io_write(VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 2);
    assert!(irq_state.borrow().asserted);
    assert_eq!(irq_state.borrow().raises, 2);
}

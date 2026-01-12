#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::ide::{IdeChannelId, IdeController};
use aero_devices::irq::NoIrq;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};

const PRIMARY_BASE: u16 = 0x1F0;
const PRIMARY_CTRL: u16 = 0x3F6;
const SECONDARY_BASE: u16 = 0x170;
const SECONDARY_CTRL: u16 = 0x376;

const KNOWN_PORTS: &[u16] = &[
    // Primary taskfile ports.
    PRIMARY_BASE + 0,
    PRIMARY_BASE + 1,
    PRIMARY_BASE + 2,
    PRIMARY_BASE + 3,
    PRIMARY_BASE + 4,
    PRIMARY_BASE + 5,
    PRIMARY_BASE + 6,
    PRIMARY_BASE + 7,
    // Primary control ports (alt status / dev ctl + drive address).
    PRIMARY_CTRL,
    PRIMARY_CTRL + 1,
    // Secondary taskfile ports.
    SECONDARY_BASE + 0,
    SECONDARY_BASE + 1,
    SECONDARY_BASE + 2,
    SECONDARY_BASE + 3,
    SECONDARY_BASE + 4,
    SECONDARY_BASE + 5,
    SECONDARY_BASE + 6,
    SECONDARY_BASE + 7,
    // Secondary control ports.
    SECONDARY_CTRL,
    SECONDARY_CTRL + 1,
];

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Small disk (keep fuzz runs fast).
    let capacity = 32 * SECTOR_SIZE as u64;
    let disk = match RawDisk::create(MemBackend::new(), capacity) {
        Ok(d) => d,
        Err(_) => return,
    };
    let drive = match AtaDrive::new(Box::new(disk) as Box<dyn VirtualDisk>) {
        Ok(d) => d,
        Err(_) => return,
    };

    let mut ctl = IdeController::new();
    ctl.attach_drive(IdeChannelId::Primary, 0, drive);

    let irq14 = NoIrq;
    let irq15 = NoIrq;

    let ops_len: usize = u.int_in_range(0usize..=1024).unwrap_or(0);
    for _ in 0..ops_len {
        let use_known: bool = u.arbitrary().unwrap_or(true);
        let port = if use_known {
            let idx: usize = u
                .int_in_range(0usize..=KNOWN_PORTS.len().saturating_sub(1))
                .unwrap_or(0);
            KNOWN_PORTS[idx]
        } else {
            u.arbitrary::<u16>().unwrap_or(0)
        };

        let is_write: bool = u.arbitrary().unwrap_or(false);
        let width16: bool = u.arbitrary().unwrap_or(false);

        if is_write {
            if width16 {
                let val: u16 = u.arbitrary().unwrap_or(0);
                ctl.write_u16(port, val, &irq14, &irq15);
            } else {
                let val: u8 = u.arbitrary().unwrap_or(0);
                ctl.write_u8(port, val, &irq14, &irq15);
            }
        } else if width16 {
            let _ = ctl.read_u16(port, &irq14, &irq15);
        } else {
            let _ = ctl.read_u8(port, &irq14, &irq15);
        }
    }
});

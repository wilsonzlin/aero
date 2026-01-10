use super::{AerogpuDevice, IrqBits};

// Register offsets (u32 registers).
pub const REG_VERSION: u64 = 0x00;
pub const REG_CAPS: u64 = 0x04;
pub const REG_CMD_RING_DOORBELL: u64 = 0x08;
pub const REG_IRQ_STATUS: u64 = 0x0C;
pub const REG_IRQ_ACK: u64 = 0x10;
pub const REG_CMD_RING_HEAD: u64 = 0x14;
pub const REG_CMD_RING_TAIL: u64 = 0x18;
pub const REG_RESET: u64 = 0x1C;

pub fn read_u32(dev: &AerogpuDevice, offset: u64) -> u32 {
    match offset {
        REG_VERSION => dev.caps.abi_version_u32(),
        REG_CAPS => dev.caps.caps_bits,
        REG_IRQ_STATUS => dev.irq.read(),
        REG_CMD_RING_HEAD => dev.cmd_ring.head_mod(),
        REG_CMD_RING_TAIL => dev.cmd_ring.tail_mod(),
        _ => 0,
    }
}

pub fn write_u32(dev: &mut AerogpuDevice, offset: u64, value: u32) {
    match offset {
        REG_CMD_RING_DOORBELL => {
            // Any write rings the doorbell. The value is currently ignored.
            let _ = value;
            dev.doorbell.ring();
        }
        REG_IRQ_ACK => {
            dev.irq.ack(value);
        }
        REG_RESET => {
            if value != 0 {
                // This is host-side only for now; a real device would likely implement reset
                // by resetting MMIO+shared memory state, and potentially generating an interrupt.
                //
                // For this foundation implementation we restart the worker thread and clear
                // all resources.
                dev.reset();
            }
        }
        _ => {}
    }
}

/// IRQ bits exposed via `REG_IRQ_STATUS`.
pub struct IrqStatus;

impl IrqStatus {
    pub const CMD_PROCESSED: u32 = IrqBits::CMD_PROCESSED;
    pub const PRESENT_DONE: u32 = IrqBits::PRESENT_DONE;
}

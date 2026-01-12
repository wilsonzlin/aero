//! Intel 8259A Programmable Interrupt Controller (PIC) emulation.
//!
//! The core PIC model lives in `aero-interrupts`; this module adds a
//! `PortIoDevice` wrapper so the PIC can be registered on an
//! [`aero_platform::io::IoPortBus`].

use aero_platform::interrupts::PlatformInterrupts;
use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::rc::Rc;

pub use aero_interrupts::pic8259::{DualPic8259, MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};

pub type SharedPic8259 = Rc<RefCell<DualPic8259>>;

/// I/O-port view of a shared [`DualPic8259`].
///
/// `IoPortBus` maps one port to one device instance. A real PIC responds to
/// four ports, so the common pattern is to share the PIC behind `Rc<RefCell<_>>`
/// and register four `Pic8259Port` instances.
pub struct Pic8259Port {
    pic: SharedPic8259,
    port: u16,
}

impl Pic8259Port {
    pub fn new(pic: SharedPic8259, port: u16) -> Self {
        Self { pic, port }
    }
}

impl PortIoDevice for Pic8259Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        debug_assert_eq!(port, self.port);
        let pic = self.pic.borrow();
        match size {
            1 => u32::from(pic.port_read_u8(port)),
            2 => {
                let lo = pic.port_read_u8(port) as u16;
                let hi = pic.port_read_u8(port.wrapping_add(1)) as u16;
                u32::from(lo | (hi << 8))
            }
            4 => {
                let b0 = pic.port_read_u8(port);
                let b1 = pic.port_read_u8(port.wrapping_add(1));
                let b2 = pic.port_read_u8(port.wrapping_add(2));
                let b3 = pic.port_read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => u32::from(pic.port_read_u8(port)),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        if size == 0 {
            return;
        }
        debug_assert_eq!(port, self.port);
        let mut pic = self.pic.borrow_mut();
        match size {
            1 => pic.port_write_u8(port, value as u8),
            2 => {
                let [b0, b1] = (value as u16).to_le_bytes();
                pic.port_write_u8(port, b0);
                pic.port_write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = value.to_le_bytes();
                pic.port_write_u8(port, b0);
                pic.port_write_u8(port.wrapping_add(1), b1);
                pic.port_write_u8(port.wrapping_add(2), b2);
                pic.port_write_u8(port.wrapping_add(3), b3);
            }
            _ => pic.port_write_u8(port, value as u8),
        }
    }
}

/// Convenience helper to register a dual PIC on an [`IoPortBus`].
pub fn register_pic8259(bus: &mut IoPortBus, pic: SharedPic8259) {
    bus.register(
        MASTER_CMD,
        Box::new(Pic8259Port::new(pic.clone(), MASTER_CMD)),
    );
    bus.register(
        MASTER_DATA,
        Box::new(Pic8259Port::new(pic.clone(), MASTER_DATA)),
    );
    bus.register(
        SLAVE_CMD,
        Box::new(Pic8259Port::new(pic.clone(), SLAVE_CMD)),
    );
    bus.register(SLAVE_DATA, Box::new(Pic8259Port::new(pic, SLAVE_DATA)));
}

/// Registers the legacy PIC I/O ports on an [`IoPortBus`], backed by a [`PlatformInterrupts`].
///
/// This is the "machine wiring" helper used by the PC platform builder: it ensures that guest
/// accesses to ports 0x20/0x21/0xA0/0xA1 update the same PIC state queried by
/// [`PlatformInterrupts::get_pending`].
pub fn register_pic8259_on_platform_interrupts(
    bus: &mut IoPortBus,
    interrupts: Rc<RefCell<PlatformInterrupts>>,
) {
    #[derive(Clone)]
    struct PlatformPicPort {
        interrupts: Rc<RefCell<PlatformInterrupts>>,
        port: u16,
    }

    impl PlatformPicPort {
        fn new(interrupts: Rc<RefCell<PlatformInterrupts>>, port: u16) -> Self {
            Self { interrupts, port }
        }
    }

    impl PortIoDevice for PlatformPicPort {
        fn read(&mut self, port: u16, size: u8) -> u32 {
            if size == 0 {
                return 0;
            }
            debug_assert_eq!(port, self.port);
            let ints = self.interrupts.borrow();
            let pic = ints.pic();
            match size {
                1 => u32::from(pic.port_read_u8(port)),
                2 => {
                    let lo = pic.port_read_u8(port) as u16;
                    let hi = pic.port_read_u8(port.wrapping_add(1)) as u16;
                    u32::from(lo | (hi << 8))
                }
                4 => {
                    let b0 = pic.port_read_u8(port);
                    let b1 = pic.port_read_u8(port.wrapping_add(1));
                    let b2 = pic.port_read_u8(port.wrapping_add(2));
                    let b3 = pic.port_read_u8(port.wrapping_add(3));
                    u32::from_le_bytes([b0, b1, b2, b3])
                }
                _ => u32::from(pic.port_read_u8(port)),
            }
        }

        fn write(&mut self, port: u16, size: u8, value: u32) {
            if size == 0 {
                return;
            }
            debug_assert_eq!(port, self.port);
            let mut ints = self.interrupts.borrow_mut();
            let pic = ints.pic_mut();

            match size {
                1 => pic.port_write_u8(port, value as u8),
                2 => {
                    let [b0, b1] = (value as u16).to_le_bytes();
                    pic.port_write_u8(port, b0);
                    pic.port_write_u8(port.wrapping_add(1), b1);
                }
                4 => {
                    let [b0, b1, b2, b3] = value.to_le_bytes();
                    pic.port_write_u8(port, b0);
                    pic.port_write_u8(port.wrapping_add(1), b1);
                    pic.port_write_u8(port.wrapping_add(2), b2);
                    pic.port_write_u8(port.wrapping_add(3), b3);
                }
                _ => pic.port_write_u8(port, value as u8),
            }
        }
    }

    bus.register(
        MASTER_CMD,
        Box::new(PlatformPicPort::new(interrupts.clone(), MASTER_CMD)),
    );
    bus.register(
        MASTER_DATA,
        Box::new(PlatformPicPort::new(interrupts.clone(), MASTER_DATA)),
    );
    bus.register(
        SLAVE_CMD,
        Box::new(PlatformPicPort::new(interrupts.clone(), SLAVE_CMD)),
    );
    bus.register(
        SLAVE_DATA,
        Box::new(PlatformPicPort::new(interrupts, SLAVE_DATA)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_legacy_pc(pic: &mut DualPic8259) {
        // Initialize master PIC: base 0x20, slave on IRQ2, 8086 mode.
        pic.port_write_u8(MASTER_CMD, 0x11);
        pic.port_write_u8(MASTER_DATA, 0x20);
        pic.port_write_u8(MASTER_DATA, 0x04);
        pic.port_write_u8(MASTER_DATA, 0x01);

        // Initialize slave PIC: base 0x28, cascade identity 2, 8086 mode.
        pic.port_write_u8(SLAVE_CMD, 0x11);
        pic.port_write_u8(SLAVE_DATA, 0x28);
        pic.port_write_u8(SLAVE_DATA, 0x02);
        pic.port_write_u8(SLAVE_DATA, 0x01);
    }

    fn init_legacy_pc_level_triggered(pic: &mut DualPic8259) {
        // Same as `init_legacy_pc`, but with ICW1.LTIM set (level triggered).
        pic.port_write_u8(MASTER_CMD, 0x19);
        pic.port_write_u8(MASTER_DATA, 0x20);
        pic.port_write_u8(MASTER_DATA, 0x04);
        pic.port_write_u8(MASTER_DATA, 0x01);

        pic.port_write_u8(SLAVE_CMD, 0x19);
        pic.port_write_u8(SLAVE_DATA, 0x28);
        pic.port_write_u8(SLAVE_DATA, 0x02);
        pic.port_write_u8(SLAVE_DATA, 0x01);
    }

    #[test]
    fn init_sets_vector_bases() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        pic.raise_irq(0);
        assert_eq!(pic.get_pending_vector(), Some(0x20));
        pic.acknowledge(0x20);
        pic.port_write_u8(MASTER_CMD, 0x20);

        pic.raise_irq(8);
        assert_eq!(pic.get_pending_vector(), Some(0x28));
    }

    #[test]
    fn fixed_priority_and_eoi() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        pic.raise_irq(3);
        pic.raise_irq(1);

        let vec1 = pic.get_pending_vector();
        assert_eq!(vec1, Some(0x21));
        pic.acknowledge(vec1.unwrap());

        // IRQ3 is lower priority than IRQ1; it should be blocked until EOI.
        assert_eq!(pic.get_pending_vector(), None);
        pic.port_write_u8(MASTER_CMD, 0x20);

        let vec2 = pic.get_pending_vector();
        assert_eq!(vec2, Some(0x23));
        pic.acknowledge(vec2.unwrap());
    }

    #[test]
    fn masked_irqs_are_not_delivered() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // Mask IRQ1.
        pic.port_write_u8(MASTER_DATA, 0x02);
        pic.raise_irq(1);
        assert_eq!(pic.get_pending_vector(), None);

        // Unmask and ensure the latched request becomes pending.
        pic.port_write_u8(MASTER_DATA, 0x00);
        assert_eq!(pic.get_pending_vector(), Some(0x21));
    }

    #[test]
    fn slave_irqs_route_via_master_irq2() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // Mask the cascade line on the master.
        pic.port_write_u8(MASTER_DATA, 0x04);

        pic.raise_irq(8);
        assert_eq!(pic.get_pending_vector(), None);

        // Unmask and ensure IRQ8 becomes deliverable.
        pic.port_write_u8(MASTER_DATA, 0x00);
        assert_eq!(pic.get_pending_vector(), Some(0x28));
    }

    #[test]
    fn eoi_requires_slave_then_master_for_cascaded_interrupts() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // Queue two slave interrupts.
        pic.raise_irq(8);
        pic.raise_irq(9);

        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x28);
        pic.acknowledge(vec);

        // EOI only to the slave: IRQ9 becomes eligible on the slave, but the
        // master still has the cascade IRQ in service, so nothing should be
        // deliverable yet.
        pic.port_write_u8(SLAVE_CMD, 0x20);
        assert_eq!(pic.get_pending_vector(), None);

        // EOI to the master clears the cascade in-service bit, allowing IRQ9
        // through.
        pic.port_write_u8(MASTER_CMD, 0x20);
        assert_eq!(pic.get_pending_vector(), Some(0x29));
    }

    #[test]
    fn spurious_irq7_does_not_set_isr() {
        let mut pic = DualPic8259::new();
        init_legacy_pc_level_triggered(&mut pic);

        pic.raise_irq(7);
        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x27);

        // Withdraw the request before the CPU acknowledges (level-triggered).
        pic.lower_irq(7);
        assert_eq!(pic.acknowledge(vec), None);

        // Master ISR must remain clear.
        pic.port_write_u8(MASTER_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(MASTER_CMD), 0x00);
    }

    #[test]
    fn spurious_irq15_sets_only_master_cascade_in_service() {
        let mut pic = DualPic8259::new();
        init_legacy_pc_level_triggered(&mut pic);

        pic.raise_irq(15);
        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x2F);

        // Withdraw request before INTA (level-triggered), causing a spurious IRQ15.
        pic.lower_irq(15);
        assert_eq!(pic.acknowledge(vec), None);

        // Slave ISR must remain clear.
        pic.port_write_u8(SLAVE_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(SLAVE_CMD), 0x00);

        // Master should have the cascade IRQ in service.
        pic.port_write_u8(MASTER_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(MASTER_CMD), 1u8 << 2);

        // EOI to the master clears it.
        pic.port_write_u8(MASTER_CMD, 0x20);
        pic.port_write_u8(MASTER_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(MASTER_CMD), 0x00);
    }

    #[test]
    fn rotate_on_eoi_changes_priority_order() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // First, deliver IRQ0.
        pic.raise_irq(0);
        let vec0 = pic.get_pending_vector().unwrap();
        assert_eq!(vec0, 0x20);
        pic.lower_irq(0);
        assert_eq!(pic.acknowledge(vec0), Some(0));

        // Rotate on non-specific EOI: the just-serviced IRQ0 becomes lowest priority.
        pic.port_write_u8(MASTER_CMD, 0xA0);

        // Now, IRQ1 should win over IRQ0 (since IRQ0 is rotated to lowest).
        pic.raise_irq(0);
        pic.raise_irq(1);

        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x21);
    }

    #[test]
    fn ioportbus_word_accesses_span_command_and_data_ports() {
        let pic = Rc::new(RefCell::new(DualPic8259::new()));
        let mut bus = IoPortBus::new();
        register_pic8259(&mut bus, pic.clone());

        // Initialize via the bus to ensure the port device wrapper behaves like the real hardware.
        bus.write_u8(MASTER_CMD, 0x11);
        bus.write_u8(MASTER_DATA, 0x20);
        bus.write_u8(MASTER_DATA, 0x04);
        bus.write_u8(MASTER_DATA, 0x01);

        // Program IMR via a 16-bit write to the command port: low byte goes to 0x20, high byte to
        // 0x21.
        bus.write(MASTER_CMD, 2, 0xAA0A);
        assert_eq!(bus.read_u8(MASTER_DATA), 0xAA);

        // Reading a 16-bit word from 0x20 should return (data_port << 8) | command_port.
        pic.borrow_mut().raise_irq(0);
        let v = bus.read(MASTER_CMD, 2) as u16;
        assert_eq!(v, 0xAA01);
    }
}

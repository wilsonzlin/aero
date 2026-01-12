use std::collections::HashMap;

pub trait PortIoDevice {
    fn read(&mut self, port: u16, size: u8) -> u32;
    fn write(&mut self, port: u16, size: u8, value: u32);

    /// Reset the device back to its power-on state.
    fn reset(&mut self) {}
}

pub struct IoPortBus {
    devices: HashMap<u16, Box<dyn PortIoDevice>>,
}

impl IoPortBus {
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
        }
    }

    pub fn register(&mut self, port: u16, device: Box<dyn PortIoDevice>) {
        self.devices.insert(port, device);
    }

    /// Unregister an I/O port handler, returning the removed device (if any).
    ///
    /// This is useful for PCI devices with I/O BARs whose base address can change
    /// after firmware POST or resets. Platform code can unregister a previously
    /// mapped BAR range and re-register it at the new base without rebuilding the
    /// entire bus.
    pub fn unregister(&mut self, port: u16) -> Option<Box<dyn PortIoDevice>> {
        self.devices.remove(&port)
    }

    /// Unregister a contiguous range of I/O ports.
    ///
    /// Ports are computed using wrapping arithmetic (`start + offset`), matching
    /// x86 I/O port semantics.
    pub fn unregister_range(&mut self, start: u16, len: u16) {
        for offset in 0..len {
            let port = start.wrapping_add(offset);
            self.unregister(port);
        }
    }

    /// Register a device for a contiguous range of I/O ports.
    ///
    /// The provided factory is invoked once per port. It can be used to build
    /// per-port wrapper devices that share a single underlying implementation
    /// (e.g. via `Rc<RefCell<...>>`).
    pub fn register_shared_range<F>(&mut self, start: u16, len: u16, mut make: F)
    where
        F: FnMut(u16) -> Box<dyn PortIoDevice>,
    {
        for offset in 0..len {
            let port = start.wrapping_add(offset);
            self.register(port, make(port));
        }
    }

    pub fn read(&mut self, port: u16, size: u8) -> u32 {
        self.devices
            .get_mut(&port)
            .map(|d| d.read(port, size))
            .unwrap_or_else(|| match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0xFFFF_FFFF,
            })
    }

    pub fn write(&mut self, port: u16, size: u8, value: u32) {
        if let Some(device) = self.devices.get_mut(&port) {
            device.write(port, size, value);
        }
    }

    pub fn read_u8(&mut self, port: u16) -> u8 {
        self.read(port, 1) as u8
    }

    pub fn write_u8(&mut self, port: u16, value: u8) {
        self.write(port, 1, value as u32);
    }

    pub fn reset(&mut self) {
        for dev in self.devices.values_mut() {
            dev.reset();
        }
    }
}

impl Default for IoPortBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug, Default)]
    struct SharedState {
        value: u32,
    }

    #[derive(Debug)]
    struct SharedStatePort {
        state: Rc<RefCell<SharedState>>,
        base: u16,
        port: u16,
    }

    impl PortIoDevice for SharedStatePort {
        fn read(&mut self, port: u16, size: u8) -> u32 {
            debug_assert_eq!(port, self.port);
            debug_assert_eq!(size, 4);
            let state = self.state.borrow();
            // Include the offset so it's easy to spot stale mappings.
            state
                .value
                .wrapping_add(u32::from(port.wrapping_sub(self.base)))
        }

        fn write(&mut self, port: u16, size: u8, value: u32) {
            debug_assert_eq!(port, self.port);
            debug_assert_eq!(size, 4);
            self.state.borrow_mut().value = value;
        }
    }

    #[test]
    fn unregister_range_allows_clean_remap_without_stale_handlers() {
        let mut bus = IoPortBus::new();

        // Map a tiny 4-port window at 0x1000.
        let state = Rc::new(RefCell::new(SharedState::default()));
        bus.register_shared_range(0x1000, 4, {
            let state = state.clone();
            move |port| {
                Box::new(SharedStatePort {
                    state: state.clone(),
                    base: 0x1000,
                    port,
                })
            }
        });

        // Writes should be visible across ports (shared backing state).
        bus.write(0x1000, 4, 0x1234_0000);
        assert_eq!(bus.read(0x1003, 4), 0x1234_0003);

        // Unmap the old window.
        bus.unregister_range(0x1000, 4);
        assert_eq!(bus.read(0x1000, 4), 0xFFFF_FFFF);
        assert_eq!(bus.read(0x1003, 4), 0xFFFF_FFFF);

        // Remap to a new base and ensure the old ports remain unmapped.
        let state2 = Rc::new(RefCell::new(SharedState::default()));
        bus.register_shared_range(0x2000, 4, {
            let state2 = state2.clone();
            move |port| {
                Box::new(SharedStatePort {
                    state: state2.clone(),
                    base: 0x2000,
                    port,
                })
            }
        });

        bus.write(0x2001, 4, 0xDEAD_BEEF);
        assert_eq!(bus.read(0x2002, 4), 0xDEAD_BEEF + 2);
        assert_eq!(bus.read(0x1000, 4), 0xFFFF_FFFF);

        // Single-port unregister should return the removed device.
        assert!(bus.unregister(0x2000).is_some());
        assert_eq!(bus.read(0x2000, 4), 0xFFFF_FFFF);
    }
}

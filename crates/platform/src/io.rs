use std::collections::HashMap;

pub trait PortIoDevice {
    fn read(&mut self, port: u16, size: u8) -> u32;
    fn write(&mut self, port: u16, size: u8, value: u32);

    /// Reset the device back to its power-on state.
    fn reset(&mut self) {}
}

struct RangeDevice {
    start: u16,
    len: u16,
    dev: Box<dyn PortIoDevice>,
}

impl RangeDevice {
    fn end_exclusive(&self) -> u32 {
        u32::from(self.start) + u32::from(self.len)
    }

    fn contains(&self, port: u16) -> bool {
        let p = u32::from(port);
        p >= u32::from(self.start) && p < self.end_exclusive()
    }
}

pub struct IoPortBus {
    devices: HashMap<u16, Box<dyn PortIoDevice>>,
    ranges: Vec<RangeDevice>,
}

impl IoPortBus {
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
            ranges: Vec::new(),
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

    /// Registers a single device over a contiguous I/O port range.
    ///
    /// Range devices are searched only if there is no exact port match. This preserves the
    /// historical behavior for fixed legacy devices registered via [`Self::register`].
    pub fn register_range(&mut self, start: u16, len: u16, dev: Box<dyn PortIoDevice>) {
        assert!(len != 0, "I/O port range length must be non-zero");

        let end_exclusive = u32::from(start) + u32::from(len);
        assert!(
            end_exclusive <= 0x1_0000,
            "I/O port range wraps past 0xFFFF: start={start:#x} len={len:#x}"
        );

        let idx = self
            .ranges
            .partition_point(|r| u32::from(r.start) < u32::from(start));

        // For deterministic dispatch and efficient lookups, disallow overlapping ranges.
        if let Some(prev) = self.ranges.get(idx.wrapping_sub(1)) {
            assert!(
                u32::from(start) >= prev.end_exclusive(),
                "overlapping I/O port ranges: new=[{start:#x}..{end_exclusive:#x}) prev=[{:#x}..{:#x})",
                prev.start,
                prev.end_exclusive()
            );
        }
        if let Some(next) = self.ranges.get(idx) {
            assert!(
                end_exclusive <= u32::from(next.start),
                "overlapping I/O port ranges: new=[{start:#x}..{end_exclusive:#x}) next=[{:#x}..{:#x})",
                next.start,
                next.end_exclusive()
            );
        }

        self.ranges.insert(
            idx,
            RangeDevice {
                start,
                len,
                dev,
            },
        );
    }

    fn find_range_index(&self, port: u16) -> Option<usize> {
        let idx = self.ranges.partition_point(|r| r.start <= port);
        if idx == 0 {
            return None;
        }
        let cand = idx - 1;
        self.ranges.get(cand).is_some_and(|r| r.contains(port)).then_some(cand)
    }

    pub fn read(&mut self, port: u16, size: u8) -> u32 {
        if let Some(dev) = self.devices.get_mut(&port) {
            return dev.read(port, size);
        }

        if let Some(idx) = self.find_range_index(port) {
            return self
                .ranges
                .get_mut(idx)
                .expect("range index disappeared")
                .dev
                .read(port, size);
        }

        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }

    pub fn write(&mut self, port: u16, size: u8, value: u32) {
        if let Some(device) = self.devices.get_mut(&port) {
            device.write(port, size, value);
            return;
        }

        if let Some(idx) = self.find_range_index(port) {
            self.ranges
                .get_mut(idx)
                .expect("range index disappeared")
                .dev
                .write(port, size, value);
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

        for dev in self.ranges.iter_mut() {
            dev.dev.reset();
        }
    }
}

impl Default for IoPortBus {
    fn default() -> Self {
        Self::new()
    }
}

impl aero_cpu_core::paging_bus::IoBus for IoPortBus {
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception> {
        match size {
            1 | 2 | 4 => Ok(u64::from(self.read(port, size as u8))),
            _ => Err(aero_cpu_core::Exception::Unimplemented("io_read size")),
        }
    }

    fn io_write(
        &mut self,
        port: u16,
        size: u32,
        val: u64,
    ) -> Result<(), aero_cpu_core::Exception> {
        match size {
            1 | 2 | 4 => {
                self.write(port, size as u8, val as u32);
                Ok(())
            }
            _ => Err(aero_cpu_core::Exception::Unimplemented("io_write size")),
        }
    }
}

impl aero_cpu_core::paging_bus::IoBus for &mut IoPortBus {
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception> {
        aero_cpu_core::paging_bus::IoBus::io_read(&mut **self, port, size)
    }

    fn io_write(
        &mut self,
        port: u16,
        size: u32,
        val: u64,
    ) -> Result<(), aero_cpu_core::Exception> {
        aero_cpu_core::paging_bus::IoBus::io_write(&mut **self, port, size, val)
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

        const LEN: u16 = 4;
        const BASE1: u16 = 0x1000;
        const BASE2: u16 = 0x2000;

        // Map a tiny 4-port window at BASE1.
        let state = Rc::new(RefCell::new(SharedState::default()));
        bus.register_shared_range(BASE1, LEN, {
            let state = state.clone();
            move |port| {
                Box::new(SharedStatePort {
                    state: state.clone(),
                    base: BASE1,
                    port,
                })
            }
        });

        // Writes should be visible across ports (shared backing state). Touch every port in the
        // window so stale handlers can't hide.
        for off in 0..LEN {
            let port = BASE1.wrapping_add(off);
            bus.write(port, 4, 0x1234_0000);
            assert_eq!(bus.read(port, 4), 0x1234_0000 + u32::from(off));
        }

        // Unmap the old window.
        bus.unregister_range(BASE1, LEN);
        for off in 0..LEN {
            let port = BASE1.wrapping_add(off);
            assert_eq!(bus.read(port, 1), 0xFF);
            assert_eq!(bus.read(port, 2), 0xFFFF);
            assert_eq!(bus.read(port, 4), 0xFFFF_FFFF);
            bus.write(port, 4, 0xFFFF_FFFF);
        }

        // Remap to a new base and ensure the old ports remain unmapped.
        let state2 = Rc::new(RefCell::new(SharedState::default()));
        bus.register_shared_range(BASE2, LEN, {
            let state2 = state2.clone();
            move |port| {
                Box::new(SharedStatePort {
                    state: state2.clone(),
                    base: BASE2,
                    port,
                })
            }
        });

        for off in 0..LEN {
            let port = BASE2.wrapping_add(off);
            bus.write(port, 4, 0xDEAD_BEEF);
            assert_eq!(bus.read(port, 4), 0xDEAD_BEEF + u32::from(off));
        }
        for off in 0..LEN {
            let port = BASE1.wrapping_add(off);
            assert_eq!(bus.read(port, 4), 0xFFFF_FFFF);
        }

        // Single-port unregister should return the removed device.
        assert!(bus.unregister(BASE2).is_some());
        assert_eq!(bus.read(BASE2, 4), 0xFFFF_FFFF);
        assert_eq!(bus.read(BASE2.wrapping_add(1), 4), 0xDEAD_BEEF + 1);
    }
}

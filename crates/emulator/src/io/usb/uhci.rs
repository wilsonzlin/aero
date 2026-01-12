//! UHCI (USB 1.1) controller wired into the emulator's PCI + PortIO framework.
//!
//! The controller implementation itself lives in the shared `aero-usb` crate; this module is just
//! thin integration glue.

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hid::mouse::UsbHidMouseHandle;
pub use aero_usb::uhci::{regs, UhciController};

use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::pci::{PciIntxRouter, PciIntxRouterConfig};

use crate::io::pci::{PciConfigSpace, PciDevice};
use crate::io::PortIO;
use memory::MemoryBus;

enum AeroUsbMemoryBus<'a> {
    Dma(&'a mut dyn MemoryBus),
    NoDma,
}

impl aero_usb::MemoryBus for AeroUsbMemoryBus<'_> {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        match self {
            AeroUsbMemoryBus::Dma(inner) => inner.read_physical(paddr, buf),
            AeroUsbMemoryBus::NoDma => buf.fill(0xFF),
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        match self {
            AeroUsbMemoryBus::Dma(inner) => inner.write_physical(paddr, buf),
            AeroUsbMemoryBus::NoDma => {}
        }
    }
}

/// A PCI wrapper that exposes a UHCI controller as an Intel PIIX3-style device.
pub struct UhciPciDevice {
    config: PciConfigSpace,
    pub io_base: u16,
    io_base_probe: bool,
    pub controller: UhciController,
}

impl UhciPciDevice {
    const IO_BAR_SIZE: u32 = 0x20;

    pub fn new(controller: UhciController, io_base: u16) -> Self {
        let mut config = PciConfigSpace::new();

        // Vendor/device: Intel PIIX3 UHCI.
        config.set_u16(0x00, 0x8086);
        config.set_u16(0x02, 0x7020);

        // Class code: serial bus / USB / UHCI.
        config.write(0x09, 1, 0x00); // prog IF
        config.write(0x0a, 1, 0x03); // subclass
        config.write(0x0b, 1, 0x0c); // class

        // BAR4 (I/O) at 0x20.
        config.set_u32(0x20, (io_base as u32) | 0x1);

        // Interrupt pin/line match the canonical PCI INTx router configuration.
        let pin = USB_UHCI_PIIX3
            .interrupt_pin
            .expect("UHCI profile should provide interrupt pin");
        config.write(0x3d, 1, u32::from(pin.to_config_u8()));

        let router = PciIntxRouter::new(PciIntxRouterConfig::default());
        let gsi = router.gsi_for_intx(USB_UHCI_PIIX3.bdf, pin);
        let line = u8::try_from(gsi).unwrap_or(0xFF);
        config.write(0x3c, 1, u32::from(line));

        Self {
            config,
            io_base,
            io_base_probe: false,
            controller,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn io_space_enabled(&self) -> bool {
        (self.command() & (1 << 0)) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & (1 << 2)) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.controller.irq_level()
    }

    pub fn new_with_hid(io_base: u16) -> (Self, UsbHidKeyboardHandle, UsbHidMouseHandle) {
        let mut controller = UhciController::new();
        let keyboard = UsbHidKeyboardHandle::new();
        let mouse = UsbHidMouseHandle::new();
        controller.hub_mut().attach(0, Box::new(keyboard.clone()));
        controller.hub_mut().attach(1, Box::new(mouse.clone()));
        (Self::new(controller, io_base), keyboard, mouse)
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        // Gate schedule DMA on PCI command Bus Master Enable (bit 2).
        //
        // UHCI schedule processing reads/writes guest memory (frame list + TD/QH chain). When the
        // guest clears COMMAND.BME, the controller must not perform bus-master DMA.
        //
        // Note: even with DMA disabled, the controller continues to advance its internal frame
        // counter and root hub timers (port reset/debounce, remote wakeup). Use a `NoDma` memory
        // adapter rather than returning early so those state machines keep running.
        let mut adapter = if self.bus_master_enabled() {
            AeroUsbMemoryBus::Dma(mem)
        } else {
            AeroUsbMemoryBus::NoDma
        };
        self.controller.tick_1ms(&mut adapter);
    }
}

impl PciDevice for UhciPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x20 && size == 4 {
            return if self.io_base_probe {
                // BAR4: 32-byte I/O window.
                (!(Self::IO_BAR_SIZE - 1) & 0xffff_fffc) | 0x1
            } else {
                u32::from(self.io_base) | 0x1
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x20 && size == 4 {
            if value == 0xffff_ffff {
                self.io_base_probe = true;
                self.io_base = 0;
                self.config.write(offset, size, 0);
                return;
            }

            self.io_base_probe = false;
            let value = value as u16;
            let io_base = value & !0x3 & !((Self::IO_BAR_SIZE as u16) - 1);
            self.io_base = io_base;
            let encoded = u32::from(self.io_base) | 0x1;
            self.config.write(offset, size, encoded);
            return;
        }
        self.config.write(offset, size, value);
    }
}

impl PortIO for UhciPciDevice {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        }
        let Some(offset) = port.checked_sub(self.io_base) else {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        };
        self.controller.io_read(offset, size)
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return;
        }
        let Some(offset) = port.checked_sub(self.io_base) else {
            return;
        };
        self.controller.io_write(offset, size, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::PortIO;

    struct PanicMem;

    impl MemoryBus for PanicMem {
        fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected DMA read");
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
            panic!("unexpected DMA write");
        }
    }

    #[test]
    fn pci_command_io_bit_gates_port_io_access() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // COMMAND.IO is clear by default: reads float high, writes ignored.
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
        assert_eq!(dev.port_read(0x1000 + regs::REG_USBCMD, 2), 0xffff);

        // Enable I/O space decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 0);
        assert_eq!(
            dev.port_read(0x1000 + regs::REG_USBCMD, 2) as u16,
            regs::USBCMD_MAXP,
        );

        // Writes should apply once IO decoding is enabled.
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
        assert_eq!(
            dev.port_read(0x1000 + regs::REG_USBCMD, 2) as u16,
            regs::USBCMD_RS
        );
    }

    #[test]
    fn pci_command_bme_bit_gates_tick_1ms_dma() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable I/O decoding so we can program the controller, but leave BME disabled.
        dev.config_write(0x04, 2, 1 << 0);

        dev.port_write(0x1000 + regs::REG_FLBASEADD, 4, 0x1000);
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

        // With BME clear, no DMA should occur.
        let mut mem = PanicMem;
        dev.tick_1ms(&mut mem);

        // Enable BME and verify the schedule engine attempts to DMA.
        dev.config_write(0x04, 2, (1 << 0) | (1 << 2));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.tick_1ms(&mut mem);
        }));
        assert!(err.is_err());
    }

    #[test]
    fn pci_command_bme_clear_still_advances_frnum_when_running() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable I/O decoding so we can start the controller, but keep BME disabled.
        dev.config_write(0x04, 2, 1 << 0);

        dev.port_write(0x1000 + regs::REG_FRNUM, 2, 0);
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

        let fr0 = dev.port_read(0x1000 + regs::REG_FRNUM, 2) as u16;

        // With BME clear, ticking must not DMA but should still advance the frame counter.
        let mut mem = PanicMem;
        dev.tick_1ms(&mut mem);

        let fr1 = dev.port_read(0x1000 + regs::REG_FRNUM, 2) as u16;
        assert_eq!(fr1, fr0.wrapping_add(1) & 0x07ff);
    }

    #[test]
    fn pci_command_bme_clear_still_advances_root_hub_port_reset_timers() {
        #[derive(Clone)]
        struct DummyDevice;

        impl crate::io::usb::UsbDeviceModel for DummyDevice {
            fn handle_control_request(
                &mut self,
                _setup: crate::io::usb::SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> crate::io::usb::ControlResponse {
                crate::io::usb::ControlResponse::Stall
            }
        }

        const PORTSC_PED: u16 = 1 << 2;
        const PORTSC_PR: u16 = 1 << 9;

        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable I/O decoding so we can manipulate PORTSC, but keep BME disabled.
        dev.config_write(0x04, 2, 1 << 0);

        // Attach a dummy device so the port is connected; when reset completes the simplified root
        // hub model enables the port automatically.
        dev.controller.hub_mut().attach(0, Box::new(DummyDevice));

        // Assert port reset.
        dev.port_write(0x1000 + regs::REG_PORTSC1, 2, u32::from(PORTSC_PR));
        assert_ne!(
            dev.port_read(0x1000 + regs::REG_PORTSC1, 2) as u16 & PORTSC_PR,
            0
        );

        // With BME clear, ticking must not DMA but should still run port reset timers.
        let mut mem = PanicMem;
        for _ in 0..50 {
            dev.tick_1ms(&mut mem);
        }

        let portsc = dev.port_read(0x1000 + regs::REG_PORTSC1, 2) as u16;
        assert_eq!(
            portsc & PORTSC_PR,
            0,
            "port reset should self-clear after 50ms"
        );
        assert_ne!(portsc & PORTSC_PED, 0, "port should be enabled after reset");
    }

    #[test]
    fn pci_command_bme_clear_still_detects_remote_wakeup_resume() {
        use crate::io::usb::core::{UsbInResult, UsbOutResult};
        use crate::io::usb::SetupPacket;

        const PORTSC_PED: u16 = 1 << 2;
        const PORTSC_RD: u16 = 1 << 6;
        const PORTSC_SUSP: u16 = 1 << 12;

        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable I/O decoding so we can program the controller, but keep BME disabled.
        dev.config_write(0x04, 2, 1 << 0);

        // Attach a keyboard and force-enable the port so it is reachable at address 0.
        let keyboard = UsbHidKeyboardHandle::new();
        dev.controller
            .hub_mut()
            .attach(0, Box::new(keyboard.clone()));
        dev.controller.hub_mut().force_enable_for_tests(0);

        // Configure the device and enable remote wakeup.
        {
            let dev0 = dev
                .controller
                .hub_mut()
                .device_mut_for_address(0)
                .expect("keyboard should be reachable at address 0");

            let setup = SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            };
            assert_eq!(dev0.handle_setup(setup), UsbOutResult::Ack);
            assert!(matches!(
                dev0.handle_in(0, 0),
                UsbInResult::Data(data) if data.is_empty()
            ));

            let setup = SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 1,      // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            };
            assert_eq!(dev0.handle_setup(setup), UsbOutResult::Ack);
            assert!(matches!(
                dev0.handle_in(0, 0),
                UsbInResult::Data(data) if data.is_empty()
            ));
        }

        // Enter port suspend and enable resume interrupts.
        dev.port_write(
            0x1000 + regs::REG_USBINTR,
            2,
            u32::from(regs::USBINTR_RESUME),
        );
        dev.port_write(
            0x1000 + regs::REG_PORTSC1,
            2,
            u32::from(PORTSC_PED | PORTSC_SUSP),
        );

        // While suspended, a keypress should create a remote wakeup event on the next tick.
        keyboard.key_event(4, true);

        let mut mem = PanicMem;
        dev.tick_1ms(&mut mem);

        assert_ne!(
            dev.port_read(0x1000 + regs::REG_PORTSC1, 2) as u16 & PORTSC_RD,
            0
        );
        assert_ne!(
            dev.port_read(0x1000 + regs::REG_USBSTS, 2) as u16 & regs::USBSTS_RESUMEDETECT,
            0
        );
        assert!(dev.irq_level());
    }

    #[test]
    fn pci_command_intx_disable_bit_masks_irq_level() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable IO decoding so we can program USBINTR.
        dev.config_write(0x04, 2, 1 << 0);
        dev.port_write(0x1000 + regs::REG_USBINTR, 2, u32::from(regs::USBINTR_IOC));
        dev.controller.set_usbsts_bits(regs::USBSTS_USBINT);

        assert!(dev.controller.irq_level());
        assert!(dev.irq_level());

        // Disable legacy INTx delivery via PCI command bit 10.
        dev.config_write(0x04, 2, (1 << 0) | (1 << 10));
        assert!(dev.controller.irq_level());
        assert!(!dev.irq_level());
    }
}

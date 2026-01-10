//! ACPI Power Management I/O device (PM1 event/control blocks + SMI_CMD).
//!
//! Windows (and many other OSes) expects the platform to start with ACPI
//! disabled and to enable it by writing `ACPI_ENABLE` to the FADT `SMI_CMD`
//! port. Firmware then sets the `SCI_EN` bit in `PM1a_CNT`.
//!
//! We model that handshake here because Windows 7 can hang during ACPI init if
//! `SCI_EN` never becomes 1 after the enable write.

use aero_platform::io::PortIoDevice;

/// `PM1a_CNT.SCI_EN` (ACPI spec).
pub const PM1_CNT_SCI_EN: u16 = 1 << 0;

pub const DEFAULT_PM1A_EVT_BLK: u16 = 0x400;
pub const DEFAULT_PM1A_CNT_BLK: u16 = 0x404;
pub const DEFAULT_SMI_CMD_PORT: u16 = 0xB2;

pub const DEFAULT_ACPI_ENABLE: u8 = 0xA0;
pub const DEFAULT_ACPI_DISABLE: u8 = 0xA1;

#[derive(Debug, Clone, Copy)]
pub struct AcpiPmConfig {
    /// PM1a event block base address (PM1_STS at +0, PM1_EN at +2).
    pub pm1a_evt_blk: u16,
    /// PM1a control block base address (PM1_CNT).
    pub pm1a_cnt_blk: u16,
    /// FADT `SMI_CMD` I/O port.
    pub smi_cmd_port: u16,
    /// Value written to `SMI_CMD` to request ACPI enable.
    pub acpi_enable_cmd: u8,
    /// Value written to `SMI_CMD` to request ACPI disable.
    pub acpi_disable_cmd: u8,
    /// Whether ACPI starts enabled at reset.
    ///
    /// Real PCs commonly start with ACPI disabled and require the OS enable
    /// handshake. We default to that behavior.
    pub start_enabled: bool,
}

impl Default for AcpiPmConfig {
    fn default() -> Self {
        Self {
            pm1a_evt_blk: DEFAULT_PM1A_EVT_BLK,
            pm1a_cnt_blk: DEFAULT_PM1A_CNT_BLK,
            smi_cmd_port: DEFAULT_SMI_CMD_PORT,
            acpi_enable_cmd: DEFAULT_ACPI_ENABLE,
            acpi_disable_cmd: DEFAULT_ACPI_DISABLE,
            start_enabled: false,
        }
    }
}

/// Minimal ACPI PM I/O model sufficient for SCI_EN and SCI interrupt semantics.
#[derive(Debug, Clone)]
pub struct AcpiPmIo {
    cfg: AcpiPmConfig,

    pm1_sts: u16,
    pm1_en: u16,
    pm1_cnt: u16,

    sci_level: bool,
}

impl AcpiPmIo {
    pub fn new(cfg: AcpiPmConfig) -> Self {
        let mut dev = Self {
            cfg,
            pm1_sts: 0,
            pm1_en: 0,
            pm1_cnt: 0,
            sci_level: false,
        };

        if dev.cfg.start_enabled {
            dev.pm1_cnt |= PM1_CNT_SCI_EN;
        }
        dev.update_sci();
        dev
    }

    pub fn sci_level(&self) -> bool {
        self.sci_level
    }

    pub fn pm1_cnt(&self) -> u16 {
        self.pm1_cnt
    }

    pub fn is_acpi_enabled(&self) -> bool {
        (self.pm1_cnt & PM1_CNT_SCI_EN) != 0
    }

    /// Inject an event into PM1_STS (used by tests and future device wiring).
    pub fn trigger_pm1_event(&mut self, sts_bits: u16) {
        self.pm1_sts |= sts_bits;
        self.update_sci();
    }

    fn set_acpi_enabled(&mut self, enabled: bool) {
        if enabled {
            self.pm1_cnt |= PM1_CNT_SCI_EN;
        } else {
            self.pm1_cnt &= !PM1_CNT_SCI_EN;
        }
        self.update_sci();
    }

    fn update_sci(&mut self) {
        let should_assert =
            (self.pm1_cnt & PM1_CNT_SCI_EN) != 0 && (self.pm1_sts & self.pm1_en) != 0;
        self.sci_level = should_assert;
    }

    fn read_pm1_event(&self, offset: u16, size: usize) -> u32 {
        match (offset, size) {
            (0, 1) => (self.pm1_sts as u8) as u32,
            (1, 1) => ((self.pm1_sts >> 8) as u8) as u32,
            (0, 2) => self.pm1_sts as u32,
            (2, 1) => (self.pm1_en as u8) as u32,
            (3, 1) => ((self.pm1_en >> 8) as u8) as u32,
            (2, 2) => self.pm1_en as u32,
            (0, 4) => ((self.pm1_en as u32) << 16) | (self.pm1_sts as u32),
            _ => 0,
        }
    }

    fn write_pm1_event(&mut self, offset: u16, size: usize, val: u32) {
        match (offset, size) {
            // PM1_STS: write-1-to-clear.
            (0, 1) => {
                let bits = (val as u8) as u16;
                self.pm1_sts &= !bits;
            }
            (1, 1) => {
                let bits = ((val as u8) as u16) << 8;
                self.pm1_sts &= !bits;
            }
            (0, 2) => {
                let bits = val as u16;
                self.pm1_sts &= !bits;
            }
            // PM1_EN: read/write.
            (2, 1) => {
                let lo = val as u8;
                self.pm1_en = (self.pm1_en & 0xFF00) | (lo as u16);
            }
            (3, 1) => {
                let hi = val as u8;
                self.pm1_en = (self.pm1_en & 0x00FF) | ((hi as u16) << 8);
            }
            (2, 2) => {
                self.pm1_en = val as u16;
            }
            (0, 4) => {
                let sts_bits = (val & 0xFFFF) as u16;
                let en_bits = (val >> 16) as u16;
                self.pm1_sts &= !sts_bits;
                self.pm1_en = en_bits;
            }
            _ => return,
        }
        self.update_sci();
    }

    fn read_pm1_cnt(&self, offset: u16, size: usize) -> u32 {
        match (offset, size) {
            (0, 1) => (self.pm1_cnt as u8) as u32,
            (1, 1) => ((self.pm1_cnt >> 8) as u8) as u32,
            (0, 2) => self.pm1_cnt as u32,
            _ => 0,
        }
    }

    fn write_pm1_cnt(&mut self, offset: u16, size: usize, val: u32) {
        match (offset, size) {
            (0, 1) => {
                let lo = val as u8;
                self.pm1_cnt = (self.pm1_cnt & 0xFF00) | lo as u16;
            }
            (1, 1) => {
                let hi = val as u8;
                self.pm1_cnt = (self.pm1_cnt & 0x00FF) | ((hi as u16) << 8);
            }
            (0, 2) => {
                self.pm1_cnt = val as u16;
            }
            _ => return,
        }
        self.update_sci();
    }
}

impl PortIoDevice for AcpiPmIo {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let size = size as usize;
        if port >= self.cfg.pm1a_evt_blk && port < self.cfg.pm1a_evt_blk + 4 {
            return self.read_pm1_event(port - self.cfg.pm1a_evt_blk, size);
        }
        if port >= self.cfg.pm1a_cnt_blk && port < self.cfg.pm1a_cnt_blk + 2 {
            return self.read_pm1_cnt(port - self.cfg.pm1a_cnt_blk, size);
        }
        if port == self.cfg.smi_cmd_port {
            return 0;
        }
        0
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size = size as usize;
        if port >= self.cfg.pm1a_evt_blk && port < self.cfg.pm1a_evt_blk + 4 {
            self.write_pm1_event(port - self.cfg.pm1a_evt_blk, size, value);
            return;
        }
        if port >= self.cfg.pm1a_cnt_blk && port < self.cfg.pm1a_cnt_blk + 2 {
            self.write_pm1_cnt(port - self.cfg.pm1a_cnt_blk, size, value);
            return;
        }
        if port == self.cfg.smi_cmd_port {
            let cmd = value as u8;
            if cmd == self.cfg.acpi_enable_cmd {
                self.set_acpi_enabled(true);
            } else if cmd == self.cfg.acpi_disable_cmd {
                self.set_acpi_enabled(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smi_cmd_acpi_enable_sets_sci_en() {
        let mut pm = AcpiPmIo::new(AcpiPmConfig::default());
        assert_eq!(pm.pm1_cnt() & PM1_CNT_SCI_EN, 0);

        pm.write(DEFAULT_SMI_CMD_PORT, 1, DEFAULT_ACPI_ENABLE as u32);
        assert_ne!(pm.pm1_cnt() & PM1_CNT_SCI_EN, 0);
    }

    #[test]
    fn sci_only_asserts_when_sci_en_set() {
        const TEST_EVENT: u16 = 1 << 8;

        let mut pm = AcpiPmIo::new(AcpiPmConfig::default());

        // Enable the event, but ACPI (SCI_EN) is still off.
        pm.write(DEFAULT_PM1A_EVT_BLK + 2, 2, TEST_EVENT as u32);
        pm.trigger_pm1_event(TEST_EVENT);
        assert!(!pm.sci_level());

        // Enable ACPI; pending enabled event should now assert SCI.
        pm.write(DEFAULT_SMI_CMD_PORT, 1, DEFAULT_ACPI_ENABLE as u32);
        assert!(pm.sci_level());

        // Disable ACPI; SCI must deassert even though event is still pending.
        pm.write(DEFAULT_SMI_CMD_PORT, 1, DEFAULT_ACPI_DISABLE as u32);
        assert!(!pm.sci_level());
    }
}

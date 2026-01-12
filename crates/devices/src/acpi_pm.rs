//! ACPI fixed-feature power management I/O (PM1/GPE/SMI_CMD).
//!
//! Windows 7 (and most ACPI-aware OSes) expects a small set of fixed-function
//! registers described by the FADT:
//! - `SMI_CMD` + `ACPI_ENABLE`/`ACPI_DISABLE` handshake to toggle `PM1a_CNT.SCI_EN`.
//! - `PM1a_EVT` (status + enable) and `PM1a_CNT` (control).
//! - `PM_TMR` (24-bit free-running timer at 3.579545MHz) and a minimal `GPE0` block.
//!
//! This device also watches `PM1a_CNT.SLP_TYP/SLP_EN` and surfaces S5 shutdown
//! requests via a host callback.
//!
//! Note: Reset via the FADT `ResetReg` (commonly port `0xCF9`) is implemented by
//! [`crate::reset_ctrl`], not this module.

use crate::irq::{IrqLine, NoIrq};
use crate::{clock::Clock, clock::NullClock};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::rc::Rc;

/// `PM1a_CNT.SCI_EN` (ACPI spec).
pub const PM1_CNT_SCI_EN: u16 = 1 << 0;

/// `PM1_STS.PWRBTN_STS` (ACPI spec).
pub const PM1_STS_PWRBTN: u16 = 1 << 8;

/// DSDT `_S5` typically encodes `{ 0x05, 0x05 }` for `SLP_TYP`.
pub const SLP_TYP_S5: u8 = 0x05;

pub const DEFAULT_PM1A_EVT_BLK: u16 = 0x0400;
pub const DEFAULT_PM1A_CNT_BLK: u16 = 0x0404;
pub const DEFAULT_PM_TMR_BLK: u16 = 0x0408;
pub const DEFAULT_GPE0_BLK: u16 = 0x0420;
pub const DEFAULT_GPE0_BLK_LEN: u8 = 0x08;

pub const DEFAULT_SMI_CMD_PORT: u16 = 0x00B2;
pub const DEFAULT_ACPI_ENABLE: u8 = 0xA0;
pub const DEFAULT_ACPI_DISABLE: u8 = 0xA1;

const PM1_EVT_LEN: u16 = 4;
const PM1_CNT_LEN: u16 = 2;
const PM_TMR_LEN: u16 = 4;

const PM1_CNT_SLP_TYP_SHIFT: u16 = 10;
const PM1_CNT_SLP_TYP_MASK: u16 = 0b111 << PM1_CNT_SLP_TYP_SHIFT;
const PM1_CNT_SLP_EN: u16 = 1 << 13;

const PM_TIMER_FREQUENCY_HZ: u128 = 3_579_545;
const PM_TIMER_MASK_24BIT: u32 = 0x00FF_FFFF;
const NS_PER_SEC: u128 = 1_000_000_000;

#[derive(Debug, Clone, Copy)]
pub struct AcpiPmConfig {
    /// PM1a event block base address (PM1_STS at +0, PM1_EN at +2).
    pub pm1a_evt_blk: u16,
    /// PM1a control block base address (PM1_CNT).
    pub pm1a_cnt_blk: u16,
    /// PM timer block base address (PM_TMR).
    pub pm_tmr_blk: u16,
    /// GPE0 block base address.
    pub gpe0_blk: u16,
    /// Total GPE0 block length. The first half is status; the second half is enable.
    pub gpe0_blk_len: u8,
    /// FADT `SMI_CMD` I/O port.
    pub smi_cmd_port: u16,
    /// Value written to `SMI_CMD` to request ACPI enable (set `SCI_EN`).
    pub acpi_enable_cmd: u8,
    /// Value written to `SMI_CMD` to request ACPI disable (clear `SCI_EN`).
    pub acpi_disable_cmd: u8,
    /// Whether ACPI starts enabled at reset.
    pub start_enabled: bool,
}

impl Default for AcpiPmConfig {
    fn default() -> Self {
        Self {
            pm1a_evt_blk: DEFAULT_PM1A_EVT_BLK,
            pm1a_cnt_blk: DEFAULT_PM1A_CNT_BLK,
            pm_tmr_blk: DEFAULT_PM_TMR_BLK,
            gpe0_blk: DEFAULT_GPE0_BLK,
            gpe0_blk_len: DEFAULT_GPE0_BLK_LEN,
            smi_cmd_port: DEFAULT_SMI_CMD_PORT,
            acpi_enable_cmd: DEFAULT_ACPI_ENABLE,
            acpi_disable_cmd: DEFAULT_ACPI_DISABLE,
            start_enabled: false,
        }
    }
}

pub struct AcpiPmCallbacks {
    /// Driven whenever SCI should be asserted/deasserted.
    pub sci_irq: Box<dyn IrqLine>,
    /// Called when the guest requests S5 (soft-off).
    pub request_power_off: Option<Box<dyn FnMut()>>,
}

impl AcpiPmCallbacks {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for AcpiPmCallbacks {
    fn default() -> Self {
        Self {
            sci_irq: Box::new(NoIrq),
            request_power_off: None,
        }
    }
}

/// ACPI PM I/O device model.
pub struct AcpiPmIo<C: Clock = NullClock> {
    cfg: AcpiPmConfig,
    callbacks: AcpiPmCallbacks,
    clock: C,

    pm1_sts: u16,
    pm1_en: u16,
    pm1_cnt: u16,

    gpe0_sts: Vec<u8>,
    gpe0_en: Vec<u8>,

    sci_level: bool,
    timer_base_ns: u64,
    timer_last_clock_ns: u64,
}

impl AcpiPmIo<NullClock> {
    pub fn new(cfg: AcpiPmConfig) -> Self {
        Self::new_with_callbacks(cfg, AcpiPmCallbacks::default())
    }

    pub fn new_with_callbacks(cfg: AcpiPmConfig, callbacks: AcpiPmCallbacks) -> Self {
        Self::new_with_callbacks_and_clock(cfg, callbacks, NullClock)
    }
}

impl<C: Clock> AcpiPmIo<C> {
    pub fn new_with_callbacks_and_clock(
        cfg: AcpiPmConfig,
        callbacks: AcpiPmCallbacks,
        clock: C,
    ) -> Self {
        let half = (cfg.gpe0_blk_len as usize) / 2;
        let mut dev = Self {
            cfg,
            callbacks,
            clock,
            pm1_sts: 0,
            pm1_en: 0,
            pm1_cnt: 0,
            gpe0_sts: vec![0; half],
            gpe0_en: vec![0; half],
            sci_level: false,
            timer_base_ns: 0,
            timer_last_clock_ns: 0,
        };

        dev.reset_timer_base();
        if dev.cfg.start_enabled {
            dev.pm1_cnt |= PM1_CNT_SCI_EN;
        }
        dev.update_sci();
        dev
    }

    pub fn cfg(&self) -> AcpiPmConfig {
        self.cfg
    }

    pub fn sci_level(&self) -> bool {
        self.sci_level
    }

    pub fn pm1_cnt(&self) -> u16 {
        self.pm1_cnt
    }

    pub fn pm1_status(&self) -> u16 {
        self.pm1_sts
    }

    pub fn is_acpi_enabled(&self) -> bool {
        (self.pm1_cnt & PM1_CNT_SCI_EN) != 0
    }

    /// Advance the ACPI PM timer timebase by `delta_ns` nanoseconds.
    ///
    /// The PM timer is a 24-bit free-running counter that increments at 3.579545MHz.
    /// Unlike the legacy wall-clock-based implementation, this is deterministic and
    /// driven by the platform's notion of time.
    ///
    /// If the PM timer is backed by a deterministic clock (e.g. [`ManualClock`]) that
    /// is already advanced elsewhere, this method will avoid double-advancing the
    /// timer and only make up any delta not already covered by the backing clock.
    pub fn advance_ns(&mut self, delta_ns: u64) {
        let now = self.clock.now_ns();
        let clock_delta = now.wrapping_sub(self.timer_last_clock_ns);
        self.timer_last_clock_ns = now;

        // If the backing clock already advanced far enough, the timer has progressed
        // implicitly; otherwise, shift the base so that the effective elapsed time
        // increases by the requested delta.
        if clock_delta >= delta_ns {
            return;
        }
        let remaining = delta_ns - clock_delta;
        self.timer_base_ns = self.timer_base_ns.wrapping_sub(remaining);
    }

    /// Alias for [`AcpiPmIo::advance_ns`].
    ///
    /// Some platform code uses a `tick(delta_ns)` convention for deterministic time progression.
    #[inline]
    pub fn tick(&mut self, delta_ns: u64) {
        self.advance_ns(delta_ns);
    }

    /// Inject bits into `PM1_STS` and refresh SCI.
    pub fn trigger_pm1_event(&mut self, sts_bits: u16) {
        self.pm1_sts |= sts_bits;
        self.update_sci();
    }

    pub fn trigger_power_button(&mut self) {
        self.trigger_pm1_event(PM1_STS_PWRBTN);
    }

    /// Inject bits into a GPE0 status byte and refresh SCI.
    pub fn trigger_gpe0(&mut self, byte_index: usize, sts_bits: u8) {
        if let Some(slot) = self.gpe0_sts.get_mut(byte_index) {
            *slot |= sts_bits;
            self.update_sci();
        }
    }

    fn drive_sci_level(&mut self, level: bool) {
        if level == self.sci_level {
            return;
        }
        self.sci_level = level;
        self.callbacks.sci_irq.set_level(level);
    }

    fn update_sci(&mut self) {
        let sci_en = (self.pm1_cnt & PM1_CNT_SCI_EN) != 0;
        let pm_pending = (self.pm1_sts & self.pm1_en) != 0;

        let mut gpe_pending = false;
        for (sts, en) in self.gpe0_sts.iter().zip(self.gpe0_en.iter()) {
            if (sts & en) != 0 {
                gpe_pending = true;
                break;
            }
        }

        self.drive_sci_level(sci_en && (pm_pending || gpe_pending));
    }

    fn pm_timer_value(&self) -> u32 {
        let elapsed_ns = self.clock.now_ns().wrapping_sub(self.timer_base_ns) as u128;
        let ticks = elapsed_ns.saturating_mul(PM_TIMER_FREQUENCY_HZ) / NS_PER_SEC;
        (ticks as u32) & PM_TIMER_MASK_24BIT
    }

    fn timer_elapsed_ns(&self) -> u64 {
        self.clock.now_ns().wrapping_sub(self.timer_base_ns)
    }

    fn reset_timer_base(&mut self) {
        let now = self.clock.now_ns();
        self.timer_base_ns = now;
        self.timer_last_clock_ns = now;
    }

    fn set_acpi_enabled(&mut self, enabled: bool) {
        if enabled {
            self.pm1_cnt |= PM1_CNT_SCI_EN;
        } else {
            self.pm1_cnt &= !PM1_CNT_SCI_EN;
        }
        self.update_sci();
    }

    fn maybe_trigger_sleep(&mut self) {
        if (self.pm1_cnt & PM1_CNT_SLP_EN) == 0 {
            return;
        }
        let slp_typ = ((self.pm1_cnt & PM1_CNT_SLP_TYP_MASK) >> PM1_CNT_SLP_TYP_SHIFT) as u8;
        if slp_typ != SLP_TYP_S5 {
            return;
        }
        if let Some(cb) = self.callbacks.request_power_off.as_mut() {
            cb();
        }
    }

    fn port_read_u8(&mut self, port: u16) -> u8 {
        // PM1a_EVT: status @ +0..1, enable @ +2..3.
        if port >= self.cfg.pm1a_evt_blk && port < self.cfg.pm1a_evt_blk + PM1_EVT_LEN {
            let off = port - self.cfg.pm1a_evt_blk;
            return match off {
                0 => (self.pm1_sts & 0x00FF) as u8,
                1 => ((self.pm1_sts >> 8) & 0x00FF) as u8,
                2 => (self.pm1_en & 0x00FF) as u8,
                _ => ((self.pm1_en >> 8) & 0x00FF) as u8,
            };
        }

        // PM1a_CNT: control @ +0..1.
        if port >= self.cfg.pm1a_cnt_blk && port < self.cfg.pm1a_cnt_blk + PM1_CNT_LEN {
            let off = port - self.cfg.pm1a_cnt_blk;
            return if off == 0 {
                (self.pm1_cnt & 0x00FF) as u8
            } else {
                ((self.pm1_cnt >> 8) & 0x00FF) as u8
            };
        }

        // GPE0: status (first half) then enable (second half).
        if port >= self.cfg.gpe0_blk && port < self.cfg.gpe0_blk + self.cfg.gpe0_blk_len as u16 {
            let off = (port - self.cfg.gpe0_blk) as usize;
            let half = self.gpe0_sts.len();
            if half == 0 {
                return 0;
            }
            if off < half {
                return self.gpe0_sts[off];
            }
            return self.gpe0_en.get(off - half).copied().unwrap_or(0);
        }

        // SMI_CMD is write-only for our purposes.
        0
    }

    fn port_write_u8(&mut self, port: u16, value: u8) {
        // PM1a_EVT.
        if port >= self.cfg.pm1a_evt_blk && port < self.cfg.pm1a_evt_blk + PM1_EVT_LEN {
            let off = port - self.cfg.pm1a_evt_blk;
            match off {
                0 => self.pm1_sts &= !(value as u16),
                1 => self.pm1_sts &= !((value as u16) << 8),
                2 => self.pm1_en = (self.pm1_en & 0xFF00) | value as u16,
                _ => self.pm1_en = (self.pm1_en & 0x00FF) | ((value as u16) << 8),
            }
            self.update_sci();
            return;
        }

        // PM1a_CNT.
        if port >= self.cfg.pm1a_cnt_blk && port < self.cfg.pm1a_cnt_blk + PM1_CNT_LEN {
            let off = port - self.cfg.pm1a_cnt_blk;
            if off == 0 {
                self.pm1_cnt = (self.pm1_cnt & 0xFF00) | value as u16;
            } else {
                self.pm1_cnt = (self.pm1_cnt & 0x00FF) | ((value as u16) << 8);
            }
            self.update_sci();
            self.maybe_trigger_sleep();
            return;
        }

        // PM_TMR: read-only.
        if port >= self.cfg.pm_tmr_blk && port < self.cfg.pm_tmr_blk + PM_TMR_LEN {
            return;
        }

        // GPE0.
        if port >= self.cfg.gpe0_blk && port < self.cfg.gpe0_blk + self.cfg.gpe0_blk_len as u16 {
            let off = (port - self.cfg.gpe0_blk) as usize;
            let half = self.gpe0_sts.len();
            if half == 0 {
                return;
            }
            if off < half {
                self.gpe0_sts[off] &= !value; // write-1-to-clear
            } else if let Some(slot) = self.gpe0_en.get_mut(off - half) {
                *slot = value;
            }
            self.update_sci();
            return;
        }

        // SMI_CMD.
        if port == self.cfg.smi_cmd_port {
            if value == self.cfg.acpi_enable_cmd {
                self.set_acpi_enabled(true);
            } else if value == self.cfg.acpi_disable_cmd {
                self.set_acpi_enabled(false);
            }
        }
    }

    fn port_read(&mut self, port: u16, size: u8) -> u32 {
        let size = size.clamp(1, 4);
        let pm_tmr_base = self.cfg.pm_tmr_blk;
        let pm_tmr_end = pm_tmr_base + PM_TMR_LEN;

        // Latch the 32-bit PM timer value once per read so byte/word/dword accesses over the
        // 4-byte window are stable even if the clock advances between individual byte reads.
        let mut pm_tmr_latch: Option<u32> = None;
        for i in 0..(size as u16) {
            let p = port.wrapping_add(i);
            if p >= pm_tmr_base && p < pm_tmr_end {
                pm_tmr_latch = Some(self.pm_timer_value());
                break;
            }
        }
        let pm_tmr_latch = pm_tmr_latch.unwrap_or(0);

        let mut out = 0u32;
        for i in 0..(size as u32) {
            let p = port.wrapping_add(i as u16);
            let b = if p >= pm_tmr_base && p < pm_tmr_end {
                let off = p - pm_tmr_base;
                ((pm_tmr_latch >> (off * 8)) & 0xFF) as u8
            } else {
                self.port_read_u8(p)
            };
            out |= (b as u32) << (i * 8);
        }
        out
    }

    fn port_write(&mut self, port: u16, size: u8, value: u32) {
        let size = size.clamp(1, 4);
        for i in 0..(size as u32) {
            let b = ((value >> (i * 8)) & 0xFF) as u8;
            self.port_write_u8(port.wrapping_add(i as u16), b);
        }
    }

    fn reset_state(&mut self) {
        self.pm1_sts = 0;
        self.pm1_en = 0;
        self.pm1_cnt = if self.cfg.start_enabled {
            PM1_CNT_SCI_EN
        } else {
            0
        };
        for b in &mut self.gpe0_sts {
            *b = 0;
        }
        for b in &mut self.gpe0_en {
            *b = 0;
        }
        self.reset_timer_base();
        self.drive_sci_level(false);
    }
}

impl<C: Clock> IoSnapshot for AcpiPmIo<C> {
    const DEVICE_ID: [u8; 4] = *b"ACPM";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PM1_STS: u16 = 1;
        const TAG_PM1_EN: u16 = 2;
        const TAG_PM1_CNT: u16 = 3;
        const TAG_GPE0_STS: u16 = 4;
        const TAG_GPE0_EN: u16 = 5;
        const TAG_PM_TIMER_ELAPSED_NS: u16 = 6;
        const TAG_SCI_LEVEL: u16 = 7;
        const TAG_PM_TIMER_TICKS: u16 = 8;
        const TAG_PM_TIMER_REMAINDER: u16 = 9;

        let elapsed_ns = self.timer_elapsed_ns();
        let numer = (elapsed_ns as u128).saturating_mul(PM_TIMER_FREQUENCY_HZ);
        let ticks = ((numer / NS_PER_SEC) as u32) & PM_TIMER_MASK_24BIT;
        let remainder = (numer % NS_PER_SEC) as u32;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u16(TAG_PM1_STS, self.pm1_sts);
        w.field_u16(TAG_PM1_EN, self.pm1_en);
        w.field_u16(TAG_PM1_CNT, self.pm1_cnt);
        w.field_bytes(TAG_GPE0_STS, self.gpe0_sts.clone());
        w.field_bytes(TAG_GPE0_EN, self.gpe0_en.clone());
        w.field_u64(TAG_PM_TIMER_ELAPSED_NS, elapsed_ns);
        w.field_u32(TAG_PM_TIMER_TICKS, ticks);
        w.field_u32(TAG_PM_TIMER_REMAINDER, remainder);
        w.field_bool(TAG_SCI_LEVEL, self.sci_level);

        // Host wiring (`callbacks`) and the clock itself are intentionally not serialized.
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PM1_STS: u16 = 1;
        const TAG_PM1_EN: u16 = 2;
        const TAG_PM1_CNT: u16 = 3;
        const TAG_GPE0_STS: u16 = 4;
        const TAG_GPE0_EN: u16 = 5;
        const TAG_PM_TIMER_ELAPSED_NS: u16 = 6;
        const TAG_SCI_LEVEL: u16 = 7;
        const TAG_PM_TIMER_TICKS: u16 = 8;
        const TAG_PM_TIMER_REMAINDER: u16 = 9;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // NOTE: Avoid driving the SCI line low during restore. `Machine::restore_snapshot_*` and
        // other snapshot consumers may restore into an already-running instance, and introducing a
        // spurious SCI deassert/reassert edge can create an extra interrupt in edge-triggered PIC
        // mode (and generally adds non-deterministic timing).
        //
        // Decode all fields into temporary locals first so `load_state()` is atomic: if decoding
        // fails due to snapshot corruption, the device remains unchanged.

        let pm1_sts = r.u16(TAG_PM1_STS)?.unwrap_or(0);
        let pm1_en = r.u16(TAG_PM1_EN)?.unwrap_or(0);

        // Restore PM1a_CNT directly. Snapshot loads must not replay guest port writes
        // as those may trigger host callbacks (e.g. S5 shutdown via SLP_TYP/SLP_EN).
        let mut pm1_cnt = if self.cfg.start_enabled { PM1_CNT_SCI_EN } else { 0 };
        if let Some(v) = r.u16(TAG_PM1_CNT)? {
            pm1_cnt = v;
        }

        let mut gpe0_sts = vec![0u8; self.gpe0_sts.len()];
        let mut gpe0_en = vec![0u8; self.gpe0_en.len()];
        if let Some(buf) = r.bytes(TAG_GPE0_STS) {
            for (dst, src) in gpe0_sts.iter_mut().zip(buf.iter().copied()) {
                *dst = src;
            }
        }
        if let Some(buf) = r.bytes(TAG_GPE0_EN) {
            for (dst, src) in gpe0_en.iter_mut().zip(buf.iter().copied()) {
                *dst = src;
            }
        }

        let elapsed_ns = r.u64(TAG_PM_TIMER_ELAPSED_NS)?;
        // Only decode tick-based timer fields when `elapsed_ns` is absent. This avoids rejecting an
        // otherwise-valid snapshot due to a corrupted/unparseable redundant field.
        let pm_timer_ticks = if elapsed_ns.is_none() {
            r.u32(TAG_PM_TIMER_TICKS)?
        } else {
            None
        };
        let pm_timer_remainder = if pm_timer_ticks.is_some() {
            r.u32(TAG_PM_TIMER_REMAINDER)?
        } else {
            None
        };

        // `sci_level` is derived from the register state; it is snapshotted for completeness.
        let _ = r.bool(TAG_SCI_LEVEL)?;

        // Sample the clock once and reuse that value throughout restore so snapshot restore is
        // deterministic even under unusual clock implementations (e.g. test clocks that advance on
        // each `now_ns()` call).
        let now = self.clock.now_ns();

        let mut timer_base_ns = now;
        if let Some(elapsed) = elapsed_ns {
            timer_base_ns = now.wrapping_sub(elapsed);
        } else if let Some(ticks) = pm_timer_ticks {
            // Forward-compatible fallback for snapshots that may omit `TAG_PM_TIMER_ELAPSED_NS`.
            //
            // Pick an elapsed-nanoseconds value that yields the desired 24-bit PM_TMR tick count under:
            //   ticks = elapsed_ns * FREQ / 1e9
            // (matching the guest-visible PM_TMR register). This cannot preserve the sub-tick
            // remainder unless `TAG_PM_TIMER_REMAINDER` is also present.

            fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
                while b != 0 {
                    let r = a % b;
                    a = b;
                    b = r;
                }
                a
            }

            fn egcd(a: i128, b: i128) -> (i128, i128, i128) {
                if b == 0 {
                    (a, 1, 0)
                } else {
                    let (g, x, y) = egcd(b, a % b);
                    (g, y, x - (a / b) * y)
                }
            }

            fn modinv(a: i128, m: i128) -> Option<i128> {
                let (g, x, _) = egcd(a, m);
                if g != 1 {
                    return None;
                }
                Some(x.rem_euclid(m))
            }

            let ticks_mod = (ticks & PM_TIMER_MASK_24BIT) as u128;
            if let Some(rem) = pm_timer_remainder {
                // The remainder is the numerator remainder in:
                //   ticks = (elapsed_ns * FREQ) / 1e9
                //   remainder = (elapsed_ns * FREQ) % 1e9
                //
                // Restore `elapsed_ns` from `(ticks_mod, remainder)` by selecting a wrap count `k`
                // such that:
                //   (ticks_mod + k*2^24) * 1e9 + remainder
                // is divisible by `FREQ`.
                let remainder = u128::from(rem);
                if remainder >= NS_PER_SEC {
                    return Err(SnapshotError::InvalidFieldEncoding("pm_timer_remainder"));
                }
                let mod_ticks = u128::from(PM_TIMER_MASK_24BIT) + 1;
                let freq = PM_TIMER_FREQUENCY_HZ;

                let a = mod_ticks.saturating_mul(NS_PER_SEC);
                let b = ticks_mod
                    .saturating_mul(NS_PER_SEC)
                    .saturating_add(remainder);
                let g = gcd_u128(a, freq);

                if b % g == 0 {
                    let m = freq / g;
                    let a_mod = ((a / g) % m) as i128;
                    let b_mod = ((b / g) % m) as i128;
                    if let Some(inv) = modinv(a_mod, m as i128) {
                        let rhs = (m as i128 - b_mod).rem_euclid(m as i128);
                        let k = (rhs * inv).rem_euclid(m as i128) as u128;
                        let ticks_total = ticks_mod.saturating_add(k.saturating_mul(mod_ticks));
                        let numer = ticks_total
                            .saturating_mul(NS_PER_SEC)
                            .saturating_add(remainder);
                        let elapsed_ns = numer / freq;
                        let elapsed_ns_u64 = elapsed_ns.min(u64::MAX as u128) as u64;
                        timer_base_ns = now.wrapping_sub(elapsed_ns_u64);
                    } else {
                        // Fall back to restoring the visible tick counter only.
                        let numer = ticks_mod.saturating_mul(NS_PER_SEC);
                        let elapsed_ns = numer.saturating_add(freq.saturating_sub(1)) / freq;
                        let elapsed_ns_u64 = elapsed_ns.min(u64::MAX as u128) as u64;
                        timer_base_ns = now.wrapping_sub(elapsed_ns_u64);
                    }
                } else {
                    // Fall back to restoring the visible tick counter only.
                    let numer = ticks_mod.saturating_mul(NS_PER_SEC);
                    let elapsed_ns = numer.saturating_add(freq.saturating_sub(1)) / freq;
                    let elapsed_ns_u64 = elapsed_ns.min(u64::MAX as u128) as u64;
                    timer_base_ns = now.wrapping_sub(elapsed_ns_u64);
                }
            } else {
                // Restore the visible tick counter only (no fractional remainder).
                let numer = ticks_mod.saturating_mul(NS_PER_SEC);
                let elapsed_ns = numer.saturating_add(PM_TIMER_FREQUENCY_HZ.saturating_sub(1))
                    / PM_TIMER_FREQUENCY_HZ;
                let elapsed_ns_u64 = elapsed_ns.min(u64::MAX as u128) as u64;
                timer_base_ns = now.wrapping_sub(elapsed_ns_u64);
            }
        }

        // Apply decoded state.
        self.pm1_sts = pm1_sts;
        self.pm1_en = pm1_en;
        self.pm1_cnt = pm1_cnt;

        for (dst, src) in self.gpe0_sts.iter_mut().zip(gpe0_sts) {
            *dst = src;
        }
        for (dst, src) in self.gpe0_en.iter_mut().zip(gpe0_en) {
            *dst = src;
        }

        self.timer_base_ns = timer_base_ns;
        self.timer_last_clock_ns = now;

        // Re-drive SCI based on the restored latch/enabled state.
        self.update_sci();
        Ok(())
    }
}

impl<C: Clock + 'static> PortIoDevice for AcpiPmIo<C> {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        self.port_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        self.port_write(port, size, value);
    }

    fn reset(&mut self) {
        self.reset_state();
    }
}

pub type SharedAcpiPmIo<C = NullClock> = Rc<RefCell<AcpiPmIo<C>>>;

#[derive(Clone)]
pub struct AcpiPmPort<C: Clock = NullClock> {
    pm: SharedAcpiPmIo<C>,
    port: u16,
}

impl<C: Clock> AcpiPmPort<C> {
    fn new(pm: SharedAcpiPmIo<C>, port: u16) -> Self {
        Self { pm, port }
    }
}

impl<C: Clock + 'static> PortIoDevice for AcpiPmPort<C> {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.pm.borrow_mut().read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.pm.borrow_mut().write(port, size, value);
    }

    fn reset(&mut self) {
        self.pm.borrow_mut().reset();
    }
}

/// Register the ACPI PM fixed-feature I/O ports on an [`IoPortBus`].
pub fn register_acpi_pm<C: Clock + 'static>(bus: &mut IoPortBus, pm: SharedAcpiPmIo<C>) {
    let cfg = pm.borrow().cfg();

    for port in cfg.pm1a_evt_blk..cfg.pm1a_evt_blk + PM1_EVT_LEN {
        bus.register(port, Box::new(AcpiPmPort::new(pm.clone(), port)));
    }
    for port in cfg.pm1a_cnt_blk..cfg.pm1a_cnt_blk + PM1_CNT_LEN {
        bus.register(port, Box::new(AcpiPmPort::new(pm.clone(), port)));
    }
    for port in cfg.pm_tmr_blk..cfg.pm_tmr_blk + PM_TMR_LEN {
        bus.register(port, Box::new(AcpiPmPort::new(pm.clone(), port)));
    }
    for port in cfg.gpe0_blk..cfg.gpe0_blk + cfg.gpe0_blk_len as u16 {
        bus.register(port, Box::new(AcpiPmPort::new(pm.clone(), port)));
    }

    bus.register(
        cfg.smi_cmd_port,
        Box::new(AcpiPmPort::new(pm, cfg.smi_cmd_port)),
    );
}

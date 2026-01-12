use crate::clock::Clock;
use crate::irq::IrqLine;
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::rc::Rc;

const CMOS_LEN: usize = 128;

const PORT_INDEX: u16 = 0x70;
const PORT_DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_SECONDS_ALARM: u8 = 0x01;
const REG_MINUTES: u8 = 0x02;
const REG_MINUTES_ALARM: u8 = 0x03;
const REG_HOURS: u8 = 0x04;
const REG_HOURS_ALARM: u8 = 0x05;
const REG_DAY_OF_WEEK: u8 = 0x06;
const REG_DAY_OF_MONTH: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;
const REG_STATUS_C: u8 = 0x0C;
const REG_STATUS_D: u8 = 0x0D;

const REG_BASE_MEM_LO: u8 = 0x15;
const REG_BASE_MEM_HI: u8 = 0x16;
const REG_EXT_MEM_LO: u8 = 0x17;
const REG_EXT_MEM_HI: u8 = 0x18;
const REG_EXT_MEM2_LO: u8 = 0x30;
const REG_EXT_MEM2_HI: u8 = 0x31;
const REG_HIGH_MEM_LO: u8 = 0x34;
const REG_HIGH_MEM_HI: u8 = 0x35;

const REG_CENTURY: u8 = 0x32;

const REG_B_SET: u8 = 1 << 7;
const REG_B_PIE: u8 = 1 << 6;
const REG_B_AIE: u8 = 1 << 5;
const REG_B_UIE: u8 = 1 << 4;
const REG_B_DM_BINARY: u8 = 1 << 2;
const REG_B_24H: u8 = 1 << 1;

const REG_C_IRQF: u8 = 1 << 7;
const REG_C_PF: u8 = 1 << 6;
const REG_C_AF: u8 = 1 << 5;
const REG_C_UF: u8 = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcDateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl RtcDateTime {
    fn is_valid(&self) -> bool {
        if !(1..=12).contains(&self.month) {
            return false;
        }
        if self.day == 0 || self.day > days_in_month(self.year, self.month) {
            return false;
        }
        self.hour < 24 && self.minute < 60 && self.second < 60
    }
}

pub struct RtcCmos<C: Clock, I: IrqLine> {
    clock: C,
    irq8: I,
    index: u8,
    nmi_disabled: bool,
    nvram: [u8; CMOS_LEN],
    reg_a: u8,
    reg_b: u8,
    reg_c_flags: u8,
    offset_seconds: i64,
    /// Nanosecond phase offset applied when computing second boundaries from the clock.
    ///
    /// This allows snapshot restore paths to preserve sub-second alignment even when the host
    /// clock's fractional seconds differ from the snapshot moment.
    phase_offset_ns: u32,
    set_mode: bool,
    frozen_seconds: i64,
    last_rtc_seconds: i64,
    periodic_interval_ns: Option<u128>,
    next_periodic_ns: Option<u128>,
    irq_level: bool,
}

impl<C: Clock, I: IrqLine> RtcCmos<C, I> {
    pub fn new(clock: C, irq8: I) -> Self {
        Self::with_datetime(
            clock,
            irq8,
            RtcDateTime {
                year: 2000,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            },
        )
    }

    pub fn with_datetime(clock: C, irq8: I, datetime: RtcDateTime) -> Self {
        let mut rtc = Self {
            clock,
            irq8,
            index: 0,
            nmi_disabled: false,
            nvram: [0; CMOS_LEN],
            reg_a: 0x26,
            reg_b: REG_B_24H,
            reg_c_flags: 0,
            offset_seconds: 0,
            phase_offset_ns: 0,
            set_mode: false,
            frozen_seconds: 0,
            last_rtc_seconds: 0,
            periodic_interval_ns: None,
            next_periodic_ns: None,
            irq_level: false,
        };

        rtc.init_nvram();
        let now_ns = rtc.clock.now_ns();
        rtc.set_rtc_seconds(now_ns, datetime_to_unix_seconds(datetime));
        rtc.recompute_periodic(now_ns as u128);
        rtc
    }

    /// Reset the RTC/CMOS device back to its power-on state.
    ///
    /// This preserves host wiring (`clock` and `irq8`) so callers can reset a running platform
    /// without needing to rebuild the device graph.
    pub fn reset(&mut self) {
        self.index = 0;
        self.nmi_disabled = false;
        self.nvram = [0; CMOS_LEN];
        self.reg_a = 0x26;
        self.reg_b = REG_B_24H;
        self.reg_c_flags = 0;
        self.offset_seconds = 0;
        self.phase_offset_ns = 0;
        self.set_mode = false;
        self.frozen_seconds = 0;
        self.last_rtc_seconds = 0;
        self.periodic_interval_ns = None;
        self.next_periodic_ns = None;
        self.irq_level = false;

        // Deassert the IRQ line so the platform interrupt controller doesn't get stuck with a
        // level-triggered IRQ8 asserted across resets.
        self.irq8.set_level(false);

        // Initialize the CMOS RAM contents (base/extended memory sizing fields).
        self.set_memory_size_bytes(0);

        // Reset the RTC timebase to the default date/time used by `RtcCmos::new`, relative to the
        // current clock moment.
        let now_ns = self.clock.now_ns();
        self.set_rtc_seconds(
            now_ns,
            datetime_to_unix_seconds(RtcDateTime {
                year: 2000,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
        );

        // Periodic interrupts are disabled at reset, but recompute to keep internal state
        // consistent if the reset defaults change in the future.
        self.recompute_periodic(now_ns as u128);
    }

    pub fn tick(&mut self) {
        let now_ns = self.clock.now_ns();
        self.tick_at(now_ns);
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    fn init_nvram(&mut self) {
        self.set_memory_size_bytes(0);
    }

    pub fn set_memory_size_bytes(&mut self, total_ram_bytes: u64) {
        fn write_u16(buf: &mut [u8; CMOS_LEN], idx_lo: u8, idx_hi: u8, value: u16) {
            buf[idx_lo as usize] = (value & 0xFF) as u8;
            buf[idx_hi as usize] = (value >> 8) as u8;
        }

        const ONE_MIB: u64 = 1024 * 1024;
        const SIXTEEN_MIB: u64 = 16 * 1024 * 1024;
        const SIXTY_FOUR_KIB: u64 = 64 * 1024;

        let base_kb: u16 = 640;
        write_u16(&mut self.nvram, REG_BASE_MEM_LO, REG_BASE_MEM_HI, base_kb);

        let ext_kb = total_ram_bytes.saturating_sub(ONE_MIB) / 1024;
        let ext_kb = ext_kb.min(u64::from(u16::MAX)) as u16;
        write_u16(&mut self.nvram, REG_EXT_MEM_LO, REG_EXT_MEM_HI, ext_kb);
        write_u16(&mut self.nvram, REG_EXT_MEM2_LO, REG_EXT_MEM2_HI, ext_kb);

        let high_blocks = total_ram_bytes.saturating_sub(SIXTEEN_MIB) / SIXTY_FOUR_KIB;
        let high_blocks = high_blocks.min(u64::from(u16::MAX)) as u16;
        write_u16(
            &mut self.nvram,
            REG_HIGH_MEM_LO,
            REG_HIGH_MEM_HI,
            high_blocks,
        );
    }

    fn tick_at(&mut self, now_ns: u64) {
        self.handle_periodic(now_ns as u128);

        if !self.set_mode {
            let rtc_now = self.rtc_seconds_at(now_ns);
            if rtc_now != self.last_rtc_seconds {
                self.last_rtc_seconds = rtc_now;
                self.handle_second_edge(rtc_now);
            }
        }

        self.update_irq_line();
    }

    fn handle_periodic(&mut self, now_ns: u128) {
        let Some(interval_ns) = self.periodic_interval_ns else {
            self.next_periodic_ns = None;
            return;
        };

        let next_ns = self.next_periodic_ns.get_or_insert(now_ns + interval_ns);
        if now_ns < *next_ns {
            return;
        }

        self.reg_c_flags |= REG_C_PF;

        let elapsed = now_ns - *next_ns;
        let missed = elapsed / interval_ns + 1;
        *next_ns = next_ns.saturating_add(missed * interval_ns);
    }

    fn handle_second_edge(&mut self, rtc_seconds: i64) {
        if self.reg_b & REG_B_UIE != 0 {
            self.reg_c_flags |= REG_C_UF;
        }

        if self.reg_b & REG_B_AIE != 0 {
            let now_dt = unix_seconds_to_datetime(rtc_seconds);
            if self.alarm_matches(now_dt) {
                self.reg_c_flags |= REG_C_AF;
            }
        }
    }

    fn alarm_matches(&self, now: RtcDateTime) -> bool {
        fn matches_field(now: u8, alarm_raw: u8, decode: impl Fn(u8) -> u8) -> bool {
            if alarm_raw & 0xC0 == 0xC0 {
                return true;
            }
            decode(alarm_raw) == now
        }

        matches_field(now.second, self.nvram[REG_SECONDS_ALARM as usize], |v| {
            self.decode_bcd(v)
        }) && matches_field(now.minute, self.nvram[REG_MINUTES_ALARM as usize], |v| {
            self.decode_bcd(v)
        }) && {
            let alarm_raw = self.nvram[REG_HOURS_ALARM as usize];
            if alarm_raw & 0xC0 == 0xC0 {
                true
            } else {
                self.decode_hours(alarm_raw) == now.hour
            }
        }
    }

    fn update_irq_line(&mut self) {
        let asserted = self.reg_c_flags & (REG_C_PF | REG_C_AF | REG_C_UF) != 0;
        if asserted != self.irq_level {
            self.irq_level = asserted;
            self.irq8.set_level(asserted);
        }
    }

    fn rtc_seconds_at(&self, now_ns: u64) -> i64 {
        let now_ns = now_ns.wrapping_add(u64::from(self.phase_offset_ns));
        if self.set_mode {
            self.frozen_seconds
        } else {
            (now_ns / 1_000_000_000) as i64 + self.offset_seconds
        }
    }

    fn set_rtc_seconds(&mut self, now_ns: u64, unix_seconds: i64) {
        let now_ns = now_ns.wrapping_add(u64::from(self.phase_offset_ns));
        if self.set_mode {
            self.frozen_seconds = unix_seconds;
            self.last_rtc_seconds = unix_seconds;
        } else {
            self.offset_seconds = unix_seconds - (now_ns / 1_000_000_000) as i64;
            self.last_rtc_seconds = unix_seconds;
        }
    }

    fn encode_bcd(&self, value: u8) -> u8 {
        if self.reg_b & REG_B_DM_BINARY != 0 {
            value
        } else {
            ((value / 10) << 4) | (value % 10)
        }
    }

    fn decode_bcd(&self, raw: u8) -> u8 {
        if self.reg_b & REG_B_DM_BINARY != 0 {
            raw
        } else {
            ((raw >> 4) & 0x0F) * 10 + (raw & 0x0F)
        }
    }

    fn encode_hours(&self, hour_24: u8) -> u8 {
        if self.reg_b & REG_B_24H != 0 {
            self.encode_bcd(hour_24)
        } else {
            let (mut hour_12, pm) = match hour_24 {
                0 => (12, false),
                1..=11 => (hour_24, false),
                12 => (12, true),
                13..=23 => (hour_24 - 12, true),
                _ => (12, false),
            };
            hour_12 = self.encode_bcd(hour_12);
            if pm {
                hour_12 | 0x80
            } else {
                hour_12
            }
        }
    }

    fn decode_hours(&self, raw: u8) -> u8 {
        if self.reg_b & REG_B_24H != 0 {
            self.decode_bcd(raw)
        } else {
            let pm = raw & 0x80 != 0;
            let hour = self.decode_bcd(raw & 0x7F);
            match (hour, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (1..=11, false) => hour,
                (1..=11, true) => hour + 12,
                _ => 0,
            }
        }
    }

    fn update_in_progress(&self, now_ns: u64) -> bool {
        if self.set_mode {
            return false;
        }
        let now_ns = now_ns.wrapping_add(u64::from(self.phase_offset_ns));
        let subsec_ns = (now_ns % 1_000_000_000) as u32;
        subsec_ns >= 1_000_000_000 - 244_000
    }

    fn recompute_periodic(&mut self, now_ns: u128) {
        if self.reg_b & REG_B_PIE == 0 {
            self.periodic_interval_ns = None;
            self.next_periodic_ns = None;
            return;
        }

        let rs = self.reg_a & 0x0F;
        if rs < 3 {
            self.periodic_interval_ns = None;
            self.next_periodic_ns = None;
            return;
        }

        let shift = rs.saturating_sub(1);
        let freq_hz = 32_768u32 >> shift;
        let interval_ns = (1_000_000_000u128 / freq_hz as u128).max(1);

        self.periodic_interval_ns = Some(interval_ns);
        self.next_periodic_ns = Some(now_ns + interval_ns);
    }

    fn read_selected(&mut self, now_ns: u64) -> u8 {
        let idx = self.index & 0x7F;
        match idx {
            REG_SECONDS => {
                self.encode_bcd(unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).second)
            }
            REG_MINUTES => {
                self.encode_bcd(unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).minute)
            }
            REG_HOURS => {
                self.encode_hours(unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).hour)
            }
            REG_DAY_OF_WEEK => {
                let weekday = weekday_from_unix_seconds(self.rtc_seconds_at(now_ns));
                self.encode_bcd(weekday)
            }
            REG_DAY_OF_MONTH => {
                self.encode_bcd(unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).day)
            }
            REG_MONTH => {
                self.encode_bcd(unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).month)
            }
            REG_YEAR => {
                let year = unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).year;
                self.encode_bcd(year.rem_euclid(100) as u8)
            }
            REG_CENTURY => {
                let year = unix_seconds_to_datetime(self.rtc_seconds_at(now_ns)).year;
                self.encode_bcd(year.div_euclid(100) as u8)
            }
            REG_STATUS_A => {
                let uip = if self.update_in_progress(now_ns) {
                    0x80
                } else {
                    0x00
                };
                (self.reg_a & 0x7F) | uip
            }
            REG_STATUS_B => self.reg_b,
            REG_STATUS_C => {
                let mut value = self.reg_c_flags & (REG_C_PF | REG_C_AF | REG_C_UF);
                if value != 0 {
                    value |= REG_C_IRQF;
                }
                self.reg_c_flags = 0;
                self.update_irq_line();
                value
            }
            REG_STATUS_D => 0x80,
            _ => self.nvram[idx as usize],
        }
    }

    fn write_selected(&mut self, now_ns: u64, value: u8) {
        let idx = self.index & 0x7F;
        match idx {
            REG_SECONDS | REG_MINUTES | REG_HOURS | REG_DAY_OF_MONTH | REG_MONTH | REG_YEAR
            | REG_CENTURY => {
                self.write_datetime_field(now_ns, idx, value);
            }
            REG_STATUS_A => {
                self.reg_a = value & 0x7F;
                self.recompute_periodic(now_ns as u128);
            }
            REG_STATUS_B => {
                self.write_reg_b(now_ns, value);
            }
            REG_STATUS_C | REG_STATUS_D => {}
            _ => self.nvram[idx as usize] = value,
        }
    }

    fn write_reg_b(&mut self, now_ns: u64, value: u8) {
        let old_set = self.set_mode;
        let new_set = value & REG_B_SET != 0;

        if !old_set && new_set {
            self.frozen_seconds = self.rtc_seconds_at(now_ns);
            self.set_mode = true;
            self.last_rtc_seconds = self.frozen_seconds;
        } else if old_set && !new_set {
            self.set_mode = false;
            let adjusted = now_ns.wrapping_add(u64::from(self.phase_offset_ns));
            self.offset_seconds = self.frozen_seconds - (adjusted / 1_000_000_000) as i64;
            // Leaving SET mode immediately resumes the counter from the frozen value.
            self.last_rtc_seconds = self.frozen_seconds;
        }

        self.reg_b = value;
        self.recompute_periodic(now_ns as u128);
    }

    fn write_datetime_field(&mut self, now_ns: u64, idx: u8, raw: u8) {
        let mut dt = unix_seconds_to_datetime(self.rtc_seconds_at(now_ns));
        match idx {
            REG_SECONDS => dt.second = self.decode_bcd(raw),
            REG_MINUTES => dt.minute = self.decode_bcd(raw),
            REG_HOURS => dt.hour = self.decode_hours(raw),
            REG_DAY_OF_MONTH => dt.day = self.decode_bcd(raw),
            REG_MONTH => dt.month = self.decode_bcd(raw),
            REG_YEAR => {
                let year = self.decode_bcd(raw) as i32;
                dt.year = dt.year.div_euclid(100) * 100 + year;
            }
            REG_CENTURY => {
                let century = self.decode_bcd(raw) as i32;
                dt.year = century * 100 + dt.year.rem_euclid(100);
            }
            _ => return,
        }

        if !dt.is_valid() {
            return;
        }

        self.set_rtc_seconds(now_ns, datetime_to_unix_seconds(dt));
    }

    fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            PORT_INDEX => (self.index & 0x7F) | if self.nmi_disabled { 0x80 } else { 0x00 },
            PORT_DATA => {
                let now_ns = self.clock.now_ns();
                self.tick_at(now_ns);
                self.read_selected(now_ns)
            }
            _ => 0xFF,
        }
    }

    fn write_u8(&mut self, port: u16, value: u8) {
        match port {
            PORT_INDEX => {
                self.index = value & 0x7F;
                self.nmi_disabled = value & 0x80 != 0;
            }
            PORT_DATA => {
                let now_ns = self.clock.now_ns();
                self.tick_at(now_ns);
                self.write_selected(now_ns, value);
            }
            _ => {}
        }
    }
}

impl<C: Clock, I: IrqLine> PortIoDevice for RtcCmos<C, I> {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        match size {
            1 => u32::from(self.read_u8(port)),
            2 => {
                let lo = self.read_u8(port);
                let hi = self.read_u8(port.wrapping_add(1));
                u32::from(u16::from_le_bytes([lo, hi]))
            }
            4 => {
                let b0 = self.read_u8(port);
                let b1 = self.read_u8(port.wrapping_add(1));
                let b2 = self.read_u8(port.wrapping_add(2));
                let b3 = self.read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => 0,
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        match size {
            1 => self.write_u8(port, value as u8),
            2 => {
                let [b0, b1] = (value as u16).to_le_bytes();
                self.write_u8(port, b0);
                self.write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = value.to_le_bytes();
                self.write_u8(port, b0);
                self.write_u8(port.wrapping_add(1), b1);
                self.write_u8(port.wrapping_add(2), b2);
                self.write_u8(port.wrapping_add(3), b3);
            }
            _ => {}
        }
    }
}

impl<C: Clock, I: IrqLine> IoSnapshot for RtcCmos<C, I> {
    const DEVICE_ID: [u8; 4] = *b"RTCC";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_INDEX: u16 = 1;
        const TAG_NVRAM: u16 = 2;
        const TAG_REGS: u16 = 3;
        const TAG_TIME: u16 = 4;
        const TAG_PHASE_REMAINDER_NS: u16 = 5;
        const TAG_PERIODIC_REMAINING_NS: u16 = 6;

        let now_ns = self.clock.now_ns();
        let rtc_seconds = self.rtc_seconds_at(now_ns);
        let phase_remainder_ns =
            (now_ns.wrapping_add(u64::from(self.phase_offset_ns)) % 1_000_000_000) as u32;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_bytes(
            TAG_INDEX,
            Encoder::new()
                .u8(self.index)
                .bool(self.nmi_disabled)
                .finish(),
        );
        w.field_bytes(TAG_NVRAM, self.nvram.to_vec());
        w.field_bytes(
            TAG_REGS,
            Encoder::new()
                .u8(self.reg_a)
                .u8(self.reg_b)
                .u8(self.reg_c_flags)
                .finish(),
        );
        w.field_bytes(
            TAG_TIME,
            Encoder::new()
                .bytes(&rtc_seconds.to_le_bytes())
                .bytes(&self.frozen_seconds.to_le_bytes())
                .bytes(&self.last_rtc_seconds.to_le_bytes())
                .finish(),
        );
        w.field_u32(TAG_PHASE_REMAINDER_NS, phase_remainder_ns);

        if let Some(next_ns) = self.next_periodic_ns {
            let remaining = next_ns.saturating_sub(now_ns as u128);
            w.field_u64(TAG_PERIODIC_REMAINING_NS, remaining as u64);
        }

        // `irq8` is a host wiring detail; it is intentionally not serialized.
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_INDEX: u16 = 1;
        const TAG_NVRAM: u16 = 2;
        const TAG_REGS: u16 = 3;
        const TAG_TIME: u16 = 4;
        const TAG_PHASE_REMAINDER_NS: u16 = 5;
        const TAG_PERIODIC_REMAINING_NS: u16 = 6;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset deterministic state while keeping host wiring (`clock` and `irq8`) attached.
        self.index = 0;
        self.nmi_disabled = false;
        self.nvram = [0; CMOS_LEN];
        self.reg_a = 0x26;
        self.reg_b = REG_B_24H;
        self.reg_c_flags = 0;
        self.offset_seconds = 0;
        self.phase_offset_ns = 0;
        self.set_mode = false;
        self.frozen_seconds = 0;
        self.last_rtc_seconds = 0;
        self.periodic_interval_ns = None;
        self.next_periodic_ns = None;
        self.irq_level = false;

        if let Some(buf) = r.bytes(TAG_INDEX) {
            let mut d = Decoder::new(buf);
            self.index = d.u8()?;
            self.nmi_disabled = d.bool()?;
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_NVRAM) {
            if buf.len() != CMOS_LEN {
                return Err(SnapshotError::InvalidFieldEncoding("nvram"));
            }
            self.nvram.copy_from_slice(buf);
        }

        if let Some(buf) = r.bytes(TAG_REGS) {
            let mut d = Decoder::new(buf);
            self.reg_a = d.u8()?;
            self.reg_b = d.u8()?;
            self.reg_c_flags = d.u8()?;
            d.finish()?;
        }

        let mut rtc_seconds = self.last_rtc_seconds;
        if let Some(buf) = r.bytes(TAG_TIME) {
            let mut d = Decoder::new(buf);
            let rtc_raw = d.bytes(8)?;
            rtc_seconds = i64::from_le_bytes(rtc_raw.try_into().expect("slice length checked"));
            let frozen_raw = d.bytes(8)?;
            self.frozen_seconds =
                i64::from_le_bytes(frozen_raw.try_into().expect("slice length checked"));
            let last_raw = d.bytes(8)?;
            self.last_rtc_seconds =
                i64::from_le_bytes(last_raw.try_into().expect("slice length checked"));
            d.finish()?;
        }

        self.set_mode = (self.reg_b & REG_B_SET) != 0;

        let now_ns = self.clock.now_ns();
        if let Some(phase_remainder_ns) = r.u32(TAG_PHASE_REMAINDER_NS)? {
            let now_mod = (now_ns % 1_000_000_000) as u32;
            self.phase_offset_ns = (phase_remainder_ns
                .wrapping_add(1_000_000_000u32)
                .wrapping_sub(now_mod))
                % 1_000_000_000u32;
        }

        if !self.set_mode {
            let adjusted = now_ns.wrapping_add(u64::from(self.phase_offset_ns));
            self.offset_seconds = rtc_seconds - (adjusted / 1_000_000_000) as i64;
        }

        // Recompute periodic interval from the restored registers, then re-anchor the next tick
        // based on the stored remaining time.
        self.recompute_periodic(now_ns as u128);
        if let Some(remaining) = r.u64(TAG_PERIODIC_REMAINING_NS)? {
            if self.periodic_interval_ns.is_some() {
                self.next_periodic_ns = Some(now_ns as u128 + remaining as u128);
            }
        }

        // Re-drive IRQ8 based on the restored latch bits so pending level-triggered interrupts
        // are preserved across restore.
        let asserted = self.reg_c_flags & (REG_C_PF | REG_C_AF | REG_C_UF) != 0;
        self.irq_level = asserted;
        self.irq8.set_level(asserted);

        Ok(())
    }
}

pub type SharedRtcCmos<C, I> = Rc<RefCell<RtcCmos<C, I>>>;

/// I/O-port view of a shared [`RtcCmos`].
///
/// `IoPortBus` maps one port to one device instance. The RTC responds to two ports
/// (0x70 and 0x71), so callers typically share it behind `Rc<RefCell<_>>` and
/// register two `RtcCmosPort` instances.
pub struct RtcCmosPort<C: Clock, I: IrqLine> {
    rtc: SharedRtcCmos<C, I>,
    port: u16,
}

impl<C: Clock, I: IrqLine> RtcCmosPort<C, I> {
    pub fn new(rtc: SharedRtcCmos<C, I>, port: u16) -> Self {
        Self { rtc, port }
    }
}

impl<C: Clock + 'static, I: IrqLine + 'static> PortIoDevice for RtcCmosPort<C, I> {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.rtc.borrow_mut().read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.rtc.borrow_mut().write(port, size, value);
    }

    fn reset(&mut self) {
        // Reset the shared RTC back to its power-on state. This is safe to call multiple times
        // (once per port mapping) as the operation is idempotent.
        self.rtc.borrow_mut().reset();
    }
}

/// Convenience helper to register the RTC ports on an [`IoPortBus`].
pub fn register_rtc_cmos<C: Clock + 'static, I: IrqLine + 'static>(
    bus: &mut IoPortBus,
    rtc: SharedRtcCmos<C, I>,
) {
    bus.register(
        PORT_INDEX,
        Box::new(RtcCmosPort::new(rtc.clone(), PORT_INDEX)),
    );
    bus.register(PORT_DATA, Box::new(RtcCmosPort::new(rtc, PORT_DATA)));
}

fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let mut y = year as i64;
    let m = month as i64;
    let d = day as i64;
    y -= if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u8, d as u8)
}

fn datetime_to_unix_seconds(dt: RtcDateTime) -> i64 {
    let days = days_from_civil(dt.year, dt.month, dt.day);
    let seconds_in_day = dt.hour as i64 * 3600 + dt.minute as i64 * 60 + dt.second as i64;
    days * 86_400 + seconds_in_day
}

fn unix_seconds_to_datetime(unix_seconds: i64) -> RtcDateTime {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_in_day = unix_seconds.rem_euclid(86_400);
    let hour = (seconds_in_day / 3600) as u8;
    let minute = ((seconds_in_day % 3600) / 60) as u8;
    let second = (seconds_in_day % 60) as u8;
    let (year, month, day) = civil_from_days(days);
    RtcDateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }
}

fn weekday_from_unix_seconds(unix_seconds: i64) -> u8 {
    let days = unix_seconds.div_euclid(86_400);
    (days + 4).rem_euclid(7) as u8 + 1
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::irq::PlatformIrqLine;
    use aero_platform::interrupts::{
        InterruptController, PlatformInterruptMode, PlatformInterrupts,
    };
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
        let redtbl_low = 0x10u32 + gsi * 2;
        let redtbl_high = redtbl_low + 1;
        ints.ioapic_mmio_write(0x00, redtbl_low);
        ints.ioapic_mmio_write(0x10, low);
        ints.ioapic_mmio_write(0x00, redtbl_high);
        ints.ioapic_mmio_write(0x10, high);
    }

    #[derive(Clone)]
    struct TestIrq(Rc<Cell<bool>>);

    impl TestIrq {
        fn new() -> Self {
            Self(Rc::new(Cell::new(false)))
        }

        fn level(&self) -> bool {
            self.0.get()
        }
    }

    impl IrqLine for TestIrq {
        fn set_level(&self, level: bool) {
            self.0.set(level);
        }
    }

    fn read_reg(rtc: &mut impl PortIoDevice, idx: u8) -> u8 {
        rtc.write(PORT_INDEX, 1, idx as u32);
        rtc.read(PORT_DATA, 1) as u8
    }

    fn write_reg(rtc: &mut impl PortIoDevice, idx: u8, value: u8) {
        rtc.write(PORT_INDEX, 1, idx as u32);
        rtc.write(PORT_DATA, 1, value as u32);
    }

    #[test]
    fn time_registers_update_on_second_edges() {
        let clock = ManualClock::new();
        let irq = TestIrq::new();
        let mut rtc = RtcCmos::new(clock.clone(), irq);

        let s0 = read_reg(&mut rtc, REG_SECONDS);
        clock.advance_ns(500_000_000);
        let s1 = read_reg(&mut rtc, REG_SECONDS);
        assert_eq!(s0, s1);

        clock.advance_ns(600_000_000);
        let s2 = read_reg(&mut rtc, REG_SECONDS);
        assert_eq!(s2, 0x01);
    }

    #[test]
    fn initial_datetime_is_relative_to_current_clock() {
        let clock = ManualClock::new();
        clock.set_ns(10_000_000_000);

        let irq = TestIrq::new();
        let mut rtc = RtcCmos::with_datetime(
            clock,
            irq,
            RtcDateTime {
                year: 2000,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            },
        );

        assert_eq!(read_reg(&mut rtc, REG_SECONDS), 0x00);
    }

    #[test]
    fn bcd_and_binary_mode_affect_time_encoding() {
        let clock = ManualClock::new();
        let irq = TestIrq::new();
        let mut rtc = RtcCmos::new(clock.clone(), irq);

        clock.advance_ns(12_000_000_000);
        assert_eq!(read_reg(&mut rtc, REG_SECONDS), 0x12);

        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H | REG_B_DM_BINARY);
        assert_eq!(read_reg(&mut rtc, REG_SECONDS), 12);

        // Writing time fields should be interpreted using the currently selected encoding.
        write_reg(&mut rtc, REG_SECONDS, 42);
        assert_eq!(read_reg(&mut rtc, REG_SECONDS), 42);

        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H);
        assert_eq!(read_reg(&mut rtc, REG_SECONDS), 0x42);
    }

    #[test]
    fn status_c_read_clears_interrupt_flags() {
        let clock = ManualClock::new();
        let irq = TestIrq::new();
        let mut rtc = RtcCmos::new(clock.clone(), irq.clone());

        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H | REG_B_UIE);

        clock.advance_ns(1_000_000_000);
        rtc.tick();
        assert!(irq.level());

        let c = read_reg(&mut rtc, REG_STATUS_C);
        assert_eq!(c & (REG_C_IRQF | REG_C_UF), REG_C_IRQF | REG_C_UF);
        assert!(!irq.level());

        let c2 = read_reg(&mut rtc, REG_STATUS_C);
        assert_eq!(c2, 0);
    }

    #[test]
    fn irq8_asserts_only_when_enabled_and_event_occurs() {
        let clock = ManualClock::new();
        let irq = TestIrq::new();
        let mut rtc = RtcCmos::new(clock.clone(), irq.clone());

        clock.advance_ns(1_000_000_000);
        rtc.tick();
        assert!(!irq.level());

        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H | REG_B_UIE);
        clock.advance_ns(1_000_000_000);
        rtc.tick();
        assert!(irq.level());

        let _ = read_reg(&mut rtc, REG_STATUS_C);
        assert!(!irq.level());

        write_reg(&mut rtc, REG_STATUS_A, 0x26);
        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H | REG_B_PIE);
        clock.advance_ns(2_000_000);
        rtc.tick();
        assert!(irq.level());

        let c = read_reg(&mut rtc, REG_STATUS_C);
        assert_ne!(c & REG_C_PF, 0);
        assert!(!irq.level());
    }

    #[test]
    fn alarm_interrupt_triggers_when_enabled_and_time_matches() {
        let clock = ManualClock::new();
        let irq = TestIrq::new();
        let mut rtc = RtcCmos::new(clock.clone(), irq.clone());

        // Alarm at 00:00:01.
        write_reg(&mut rtc, REG_SECONDS_ALARM, 0x01);
        write_reg(&mut rtc, REG_MINUTES_ALARM, 0x00);
        write_reg(&mut rtc, REG_HOURS_ALARM, 0x00);
        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H | REG_B_AIE);

        clock.advance_ns(1_000_000_000);
        rtc.tick();
        assert!(irq.level());

        let c = read_reg(&mut rtc, REG_STATUS_C);
        assert_eq!(c & (REG_C_IRQF | REG_C_AF), REG_C_IRQF | REG_C_AF);
        assert!(!irq.level());

        // With AIE disabled, no alarm flags should be raised even if the compare matches.
        write_reg(&mut rtc, REG_STATUS_B, REG_B_24H);
        write_reg(&mut rtc, REG_SECONDS_ALARM, 0x02);
        clock.advance_ns(1_000_000_000);
        rtc.tick();
        assert!(!irq.level());
        assert_eq!(read_reg(&mut rtc, REG_STATUS_C), 0);
    }

    #[test]
    fn irq8_routes_through_platform_interrupts_and_requires_status_c_clear() {
        let clock = ManualClock::new();

        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
        interrupts.borrow_mut().pic_mut().set_offsets(0x20, 0x28);

        let irq_line = PlatformIrqLine::isa(interrupts.clone(), 8);
        let rtc = Rc::new(RefCell::new(RtcCmos::new(clock.clone(), irq_line)));

        let mut bus = IoPortBus::new();
        register_rtc_cmos(&mut bus, rtc.clone());

        // Enable update-ended interrupts.
        bus.write_u8(PORT_INDEX, REG_STATUS_B);
        bus.write_u8(PORT_DATA, REG_B_24H | REG_B_UIE);

        clock.advance_ns(1_000_000_000);
        rtc.borrow_mut().tick();
        assert_eq!(interrupts.borrow().get_pending(), Some(0x28));

        // Acknowledge and EOI without reading status C: the RTC line stays asserted,
        // so no new edges should be observed.
        interrupts.borrow_mut().acknowledge(0x28);
        interrupts.borrow_mut().eoi(0x28);
        assert_eq!(interrupts.borrow().get_pending(), None);

        clock.advance_ns(1_000_000_000);
        rtc.borrow_mut().tick();
        assert_eq!(interrupts.borrow().get_pending(), None);

        // Reading status C clears the event latch and lowers IRQ8, enabling the next edge.
        bus.write_u8(PORT_INDEX, REG_STATUS_C);
        let status_c = bus.read_u8(PORT_DATA);
        assert_ne!(status_c & REG_C_UF, 0);

        clock.advance_ns(1_000_000_000);
        rtc.borrow_mut().tick();
        assert_eq!(interrupts.borrow().get_pending(), Some(0x28));
    }

    #[test]
    fn irq8_routes_via_ioapic_in_apic_mode() {
        let clock = ManualClock::new();

        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
        interrupts
            .borrow_mut()
            .set_mode(PlatformInterruptMode::Apic);

        let vector = 0x48u8;
        program_ioapic_entry(&mut interrupts.borrow_mut(), 8, u32::from(vector), 0);

        let irq_line = PlatformIrqLine::isa(interrupts.clone(), 8);
        let rtc = Rc::new(RefCell::new(RtcCmos::new(clock.clone(), irq_line)));

        let mut bus = IoPortBus::new();
        register_rtc_cmos(&mut bus, rtc.clone());

        bus.write_u8(PORT_INDEX, REG_STATUS_B);
        bus.write_u8(PORT_DATA, REG_B_24H | REG_B_UIE);

        clock.advance_ns(1_000_000_000);
        rtc.borrow_mut().tick();
        assert_eq!(interrupts.borrow().get_pending(), Some(vector));

        interrupts.borrow_mut().acknowledge(vector);
        interrupts.borrow_mut().eoi(vector);
        assert_eq!(interrupts.borrow().get_pending(), None);

        clock.advance_ns(1_000_000_000);
        rtc.borrow_mut().tick();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ8 stays asserted until Status C is read, so edge-triggered IOAPIC should not re-fire",
        );

        bus.write_u8(PORT_INDEX, REG_STATUS_C);
        let _ = bus.read_u8(PORT_DATA);

        clock.advance_ns(1_000_000_000);
        rtc.borrow_mut().tick();
        assert_eq!(interrupts.borrow().get_pending(), Some(vector));
    }

    #[test]
    fn nvram_reports_base_and_extended_memory_sizes() {
        let clock = ManualClock::new();
        let irq = TestIrq::new();
        let mut rtc = RtcCmos::new(clock, irq);

        rtc.set_memory_size_bytes(32 * 1024 * 1024);

        let base_lo = read_reg(&mut rtc, REG_BASE_MEM_LO);
        let base_hi = read_reg(&mut rtc, REG_BASE_MEM_HI);
        assert_eq!(u16::from_le_bytes([base_lo, base_hi]), 640);

        let ext_lo = read_reg(&mut rtc, REG_EXT_MEM_LO);
        let ext_hi = read_reg(&mut rtc, REG_EXT_MEM_HI);
        assert_eq!(u16::from_le_bytes([ext_lo, ext_hi]), 31_744);

        let ext2_lo = read_reg(&mut rtc, REG_EXT_MEM2_LO);
        let ext2_hi = read_reg(&mut rtc, REG_EXT_MEM2_HI);
        assert_eq!(u16::from_le_bytes([ext2_lo, ext2_hi]), 31_744);

        let high_lo = read_reg(&mut rtc, REG_HIGH_MEM_LO);
        let high_hi = read_reg(&mut rtc, REG_HIGH_MEM_HI);
        assert_eq!(u16::from_le_bytes([high_lo, high_hi]), 256);
    }
}

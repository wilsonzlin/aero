use crate::{memory::MemoryBus, rtc::CmosRtc};
use std::time::Duration;

pub const TICKS_PER_DAY: u32 = 0x1800B0;

pub const BDA_TICK_COUNT_ADDR: u64 = 0x046C;
pub const BDA_MIDNIGHT_FLAG_ADDR: u64 = 0x0470;

const NANOS_PER_DAY: u128 = 86_400_000_000_000;

#[derive(Debug, Clone)]
pub struct BdaTime {
    tick_count: u32,
    tick_remainder: u128,
    midnight_flag: u8,
}

impl BdaTime {
    pub fn from_rtc(rtc: &CmosRtc) -> Self {
        let time_of_day = rtc.datetime().time_of_day();
        let nanos = time_of_day.as_nanos();
        let numerator = nanos * u128::from(TICKS_PER_DAY);

        let tick_count = (numerator / NANOS_PER_DAY) as u32;
        let tick_remainder = numerator % NANOS_PER_DAY;

        Self {
            tick_count,
            tick_remainder,
            midnight_flag: 0,
        }
    }

    pub fn tick_count(&self) -> u32 {
        self.tick_count
    }

    pub fn midnight_flag(&self) -> u8 {
        self.midnight_flag
    }

    pub fn clear_midnight_flag(&mut self) {
        self.midnight_flag = 0;
    }

    pub fn set_tick_count(&mut self, memory: &mut impl MemoryBus, tick_count: u32) {
        self.tick_count = tick_count % TICKS_PER_DAY;
        self.tick_remainder = 0;
        self.midnight_flag = 0;
        self.write_to_bda(memory);
    }

    pub fn duration_from_ticks(ticks: u32) -> Duration {
        let ticks = u128::from(ticks % TICKS_PER_DAY);
        let nanos = ticks * NANOS_PER_DAY / u128::from(TICKS_PER_DAY);
        Duration::new(
            (nanos / 1_000_000_000) as u64,
            (nanos % 1_000_000_000) as u32,
        )
    }

    pub fn advance(&mut self, memory: &mut impl MemoryBus, delta: Duration) {
        let delta_nanos = delta.as_nanos();
        let numerator = delta_nanos * u128::from(TICKS_PER_DAY) + self.tick_remainder;

        let ticks_to_add = numerator / NANOS_PER_DAY;
        self.tick_remainder = numerator % NANOS_PER_DAY;

        let total = u128::from(self.tick_count) + ticks_to_add;
        let wraps = total / u128::from(TICKS_PER_DAY);
        if wraps != 0 {
            self.midnight_flag = self.midnight_flag.wrapping_add(wraps as u8);
        }

        self.tick_count = (total % u128::from(TICKS_PER_DAY)) as u32;
        self.write_to_bda(memory);
    }

    pub fn write_to_bda(&self, memory: &mut impl MemoryBus) {
        memory.write_u32(BDA_TICK_COUNT_ADDR, self.tick_count);
        memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, self.midnight_flag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::VecMemory, rtc::DateTime};

    #[test]
    fn tick_count_advances_at_pit_rate() {
        let rtc = CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0));
        let mut bda_time = BdaTime::from_rtc(&rtc);
        let mut memory = VecMemory::new(0x100000);
        bda_time.write_to_bda(&mut memory);

        for _ in 0..100 {
            bda_time.advance(&mut memory, Duration::from_secs(1));
        }

        let expected = (u64::from(TICKS_PER_DAY) * 100 / 86_400) as u32;
        assert_eq!(bda_time.tick_count(), expected);
        assert_eq!(memory.read_u32(BDA_TICK_COUNT_ADDR), expected);
        assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);
    }

    #[test]
    fn tick_count_rolls_over_and_sets_midnight_flag() {
        let rtc = CmosRtc::new(DateTime::new(2026, 1, 1, 23, 59, 59));
        let mut bda_time = BdaTime::from_rtc(&rtc);
        let mut memory = VecMemory::new(0x100000);
        bda_time.write_to_bda(&mut memory);

        let expected_pre_midnight = (u64::from(TICKS_PER_DAY) * 86_399 / 86_400) as u32;
        assert_eq!(bda_time.tick_count(), expected_pre_midnight);

        bda_time.advance(&mut memory, Duration::from_secs(2));

        let expected_after_midnight = (u64::from(TICKS_PER_DAY) * 1 / 86_400) as u32;
        assert_eq!(bda_time.tick_count(), expected_after_midnight);
        assert_eq!(bda_time.midnight_flag(), 1);
        assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 1);
    }

    #[test]
    fn midnight_flag_counts_multiple_wraps() {
        let rtc = CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0));
        let mut bda_time = BdaTime::from_rtc(&rtc);
        let mut memory = VecMemory::new(0x100000);
        bda_time.write_to_bda(&mut memory);

        bda_time.advance(&mut memory, Duration::from_secs(172_800 + 3));

        let expected_after = (u64::from(TICKS_PER_DAY) * 3 / 86_400) as u32;
        assert_eq!(bda_time.tick_count(), expected_after);
        assert_eq!(bda_time.midnight_flag(), 2);
        assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 2);
    }
}

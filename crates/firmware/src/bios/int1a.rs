use crate::{cpu::CpuState, memory::MemoryBus};

use super::{BdaTime, Bios};

impl Bios {
    pub fn handle_int1a(&mut self, cpu: &mut CpuState, memory: &mut impl MemoryBus) {
        match cpu.ah() {
            0x00 => {
                let ticks = self.bda_time.tick_count();
                let midnight_flag = self.bda_time.midnight_flag();

                cpu.set_cx((ticks >> 16) as u16);
                cpu.set_dx((ticks & 0xFFFF) as u16);
                cpu.set_al(midnight_flag);
                cpu.clear_cf();

                self.bda_time.clear_midnight_flag();
                self.bda_time.write_to_bda(memory);
            }
            0x01 => {
                let ticks = ((cpu.cx() as u32) << 16) | (cpu.dx() as u32);
                self.bda_time.set_tick_count(memory, ticks);
                let _ = self
                    .rtc
                    .set_time_of_day(BdaTime::duration_from_ticks(ticks));

                cpu.set_ah(0);
                cpu.clear_cf();
            }
            0x02 => {
                let time = self.rtc.read_time();

                cpu.set_ah(0);
                cpu.set_ch(time.hour);
                cpu.set_cl(time.minute);
                cpu.set_dh(time.second);
                cpu.set_dl(time.daylight_savings);
                cpu.clear_cf();
            }
            0x03 => {
                let hour = (cpu.cx() >> 8) as u8;
                let minute = (cpu.cx() & 0xFF) as u8;
                let second = (cpu.dx() >> 8) as u8;
                let daylight_savings = (cpu.dx() & 0xFF) as u8;

                match self
                    .rtc
                    .set_time_cmos(hour, minute, second, daylight_savings)
                {
                    Ok(()) => {
                        self.bda_time = BdaTime::from_rtc(&self.rtc);
                        self.bda_time.write_to_bda(memory);

                        cpu.set_ah(0);
                        cpu.clear_cf();
                    }
                    Err(()) => {
                        cpu.set_ah(1);
                        cpu.set_cf();
                    }
                }
            }
            0x04 => {
                let date = self.rtc.read_date();

                cpu.set_ah(0);
                cpu.set_ch(date.century);
                cpu.set_cl(date.year);
                cpu.set_dh(date.month);
                cpu.set_dl(date.day);
                cpu.clear_cf();
            }
            0x05 => {
                let century = (cpu.cx() >> 8) as u8;
                let year = (cpu.cx() & 0xFF) as u8;
                let month = (cpu.dx() >> 8) as u8;
                let day = (cpu.dx() & 0xFF) as u8;

                match self.rtc.set_date_cmos(century, year, month, day) {
                    Ok(()) => {
                        cpu.set_ah(0);
                        cpu.clear_cf();
                    }
                    Err(()) => {
                        cpu.set_ah(1);
                        cpu.set_cf();
                    }
                }
            }
            _ => {
                cpu.set_cf();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bios::BDA_MIDNIGHT_FLAG_ADDR, memory::VecMemory, rtc::DateTime};
    use std::time::Duration;

    #[test]
    fn ah00_reports_bda_ticks_and_clears_midnight_flag() {
        let mut bios = Bios::new(crate::rtc::CmosRtc::new(DateTime::new(
            2026, 1, 1, 23, 59, 59,
        )));
        let mut memory = VecMemory::new(0x100000);
        bios.init(&mut memory);

        bios.advance_time(&mut memory, Duration::from_secs(2));
        assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 1);

        let mut cpu = CpuState::default();
        cpu.set_ah(0x00);
        bios.handle_int1a(&mut cpu, &mut memory);

        let ticks = ((cpu.cx() as u32) << 16) | (cpu.dx() as u32);
        assert_eq!(ticks, bios.bda_time.tick_count());
        assert_eq!(cpu.al(), 1);
        assert!(!cpu.cf());
        assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);

        cpu.set_ah(0x00);
        bios.handle_int1a(&mut cpu, &mut memory);
        assert_eq!(cpu.al(), 0);
    }

    #[test]
    fn rtc_time_and_date_respect_bcd_mode() {
        let mut rtc = crate::rtc::CmosRtc::new(DateTime::new(2026, 1, 10, 21, 34, 56));
        rtc.set_bcd_mode(true);
        let mut bios = Bios::new(rtc.clone());
        let mut memory = VecMemory::new(0x100000);
        bios.init(&mut memory);

        let mut cpu = CpuState::default();
        cpu.set_ah(0x02);
        bios.handle_int1a(&mut cpu, &mut memory);
        assert_eq!(cpu.ah(), 0);
        assert_eq!((cpu.cx() >> 8) as u8, 0x21);
        assert_eq!((cpu.cx() & 0xFF) as u8, 0x34);
        assert_eq!((cpu.dx() >> 8) as u8, 0x56);

        cpu.set_ah(0x04);
        bios.handle_int1a(&mut cpu, &mut memory);
        assert_eq!((cpu.cx() >> 8) as u8, 0x20);
        assert_eq!((cpu.cx() & 0xFF) as u8, 0x26);
        assert_eq!((cpu.dx() >> 8) as u8, 0x01);
        assert_eq!((cpu.dx() & 0xFF) as u8, 0x10);

        rtc.set_bcd_mode(false);
        let mut bios = Bios::new(rtc);
        bios.init(&mut memory);

        cpu.set_ah(0x02);
        bios.handle_int1a(&mut cpu, &mut memory);
        assert_eq!((cpu.cx() >> 8) as u8, 21);
        assert_eq!((cpu.cx() & 0xFF) as u8, 34);
        assert_eq!((cpu.dx() >> 8) as u8, 56);
    }

    #[test]
    fn set_system_time_updates_bda_tick_count() {
        let mut bios = Bios::new(crate::rtc::CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
        let mut memory = VecMemory::new(0x100000);
        bios.init(&mut memory);

        let mut cpu = CpuState::default();
        cpu.set_ah(0x01);
        cpu.set_cx(0x1234);
        cpu.set_dx(0x5678);
        bios.handle_int1a(&mut cpu, &mut memory);

        assert_eq!(cpu.ah(), 0);
        assert!(!cpu.cf());

        let expected = ((0x1234u32 << 16) | 0x5678u32) % super::super::TICKS_PER_DAY;
        assert_eq!(bios.bda_time.tick_count(), expected);
        assert_eq!(memory.read_u32(super::super::BDA_TICK_COUNT_ADDR), expected);
    }

    #[test]
    fn set_rtc_time_and_date_recompute_bda_ticks() {
        let mut rtc = crate::rtc::CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0));
        rtc.set_bcd_mode(true);
        let mut bios = Bios::new(rtc);
        let mut memory = VecMemory::new(0x100000);
        bios.init(&mut memory);

        let mut cpu = CpuState::default();
        cpu.set_ah(0x03);
        cpu.set_ch(0x12);
        cpu.set_cl(0x00);
        cpu.set_dh(0x00);
        cpu.set_dl(0x00);
        bios.handle_int1a(&mut cpu, &mut memory);

        assert_eq!(cpu.ah(), 0);
        assert!(!cpu.cf());

        let expected = (u64::from(super::super::TICKS_PER_DAY) * 43_200 / 86_400) as u32;
        assert_eq!(bios.bda_time.tick_count(), expected);
        assert_eq!(memory.read_u32(super::super::BDA_TICK_COUNT_ADDR), expected);

        cpu.set_ah(0x05);
        cpu.set_ch(0x20);
        cpu.set_cl(0x26);
        cpu.set_dh(0x01);
        cpu.set_dl(0x10);
        bios.handle_int1a(&mut cpu, &mut memory);
        assert_eq!(cpu.ah(), 0);
        assert!(!cpu.cf());

        let dt = bios.rtc.datetime();
        assert_eq!(dt.year, 2026);
        assert_eq!(dt.month, 1);
        assert_eq!(dt.day, 10);
    }
}

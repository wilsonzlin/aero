use std::time::Duration;

fn to_bcd(value: u8) -> u8 {
    debug_assert!(value < 100);
    (value / 10) << 4 | (value % 10)
}

fn from_bcd(value: u8) -> Option<u8> {
    let hi = value >> 4;
    let lo = value & 0x0F;
    if hi < 10 && lo < 10 {
        Some(hi * 10 + lo)
    } else {
        None
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub nanosecond: u32,
}

impl DateTime {
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanosecond: 0,
        }
    }

    pub fn time_of_day(&self) -> Duration {
        Duration::new(
            (self.hour as u64) * 3600 + (self.minute as u64) * 60 + (self.second as u64),
            self.nanosecond,
        )
    }

    pub fn advance(&mut self, delta: Duration) {
        let extra_nanos = self.nanosecond as u64 + delta.subsec_nanos() as u64;
        let carry_secs = extra_nanos / 1_000_000_000;
        self.nanosecond = (extra_nanos % 1_000_000_000) as u32;

        let total_secs = self.time_of_day().as_secs() + delta.as_secs() + carry_secs;
        let days = total_secs / 86_400;
        let secs = total_secs % 86_400;

        self.hour = (secs / 3600) as u8;
        self.minute = ((secs % 3600) / 60) as u8;
        self.second = (secs % 60) as u8;

        self.add_days(days);
    }

    pub fn set_date(&mut self, year: u16, month: u8, day: u8) -> Result<(), ()> {
        if !(1..=12).contains(&month) {
            return Err(());
        }
        let dim = days_in_month(year, month);
        if day == 0 || day > dim {
            return Err(());
        }
        self.year = year;
        self.month = month;
        self.day = day;
        Ok(())
    }

    pub fn set_time_of_day(&mut self, time: Duration) -> Result<(), ()> {
        if time.as_secs() >= 86_400 {
            return Err(());
        }
        let secs = time.as_secs();
        self.hour = (secs / 3600) as u8;
        self.minute = ((secs % 3600) / 60) as u8;
        self.second = (secs % 60) as u8;
        self.nanosecond = time.subsec_nanos();
        Ok(())
    }

    fn add_days(&mut self, mut days: u64) {
        while days > 0 {
            let dim = days_in_month(self.year, self.month) as u64;
            let day = self.day as u64;
            let remaining = dim.saturating_sub(day);

            if days <= remaining {
                self.day = (day + days) as u8;
                return;
            }

            days -= remaining + 1;
            self.day = 1;
            if self.month == 12 {
                self.month = 1;
                self.year += 1;
            } else {
                self.month += 1;
            }
        }
    }
}

fn is_leap_year(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_month(year: u16, month: u8) -> u8 {
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
        _ => 30,
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct RtcTime {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub daylight_savings: u8,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct RtcDate {
    pub century: u8,
    pub year: u8,
    pub month: u8,
    pub day: u8,
}

#[derive(Debug, Clone)]
pub struct CmosRtc {
    datetime: DateTime,
    bcd_mode: bool,
    hour_24: bool,
    daylight_savings: bool,
}

impl CmosRtc {
    pub fn new(datetime: DateTime) -> Self {
        Self {
            datetime,
            bcd_mode: true,
            hour_24: true,
            daylight_savings: false,
        }
    }

    pub fn datetime(&self) -> DateTime {
        self.datetime
    }

    pub fn advance(&mut self, delta: Duration) {
        self.datetime.advance(delta);
    }

    pub fn set_bcd_mode(&mut self, enabled: bool) {
        self.bcd_mode = enabled;
    }

    pub fn set_time_of_day(&mut self, time: Duration) -> Result<(), ()> {
        self.datetime.set_time_of_day(time)
    }

    pub fn set_time_cmos(
        &mut self,
        hour: u8,
        minute: u8,
        second: u8,
        daylight_savings: u8,
    ) -> Result<(), ()> {
        let hour = self.decode_hour(hour).ok_or(())?;
        let minute = self.decode_field(minute).ok_or(())?;
        let second = self.decode_field(second).ok_or(())?;
        if minute >= 60 || second >= 60 {
            return Err(());
        }

        self.daylight_savings = daylight_savings != 0;
        self.datetime.set_time_of_day(Duration::new(
            (hour as u64) * 3600 + (minute as u64) * 60 + (second as u64),
            0,
        ))
    }

    pub fn set_date_cmos(&mut self, century: u8, year: u8, month: u8, day: u8) -> Result<(), ()> {
        let century = self.decode_field(century).ok_or(())?;
        let year = self.decode_field(year).ok_or(())?;
        let month = self.decode_field(month).ok_or(())?;
        let day = self.decode_field(day).ok_or(())?;

        let full_year = (century as u16) * 100 + (year as u16);
        self.datetime.set_date(full_year, month, day)
    }

    pub fn read_time(&self) -> RtcTime {
        let mut hour = self.datetime.hour;
        if !self.hour_24 {
            let pm = hour >= 12;
            hour %= 12;
            if hour == 0 {
                hour = 12;
            }
            if pm {
                hour |= 0x80;
            }
        }

        let (hour, minute, second) = if self.bcd_mode {
            (
                to_bcd(hour),
                to_bcd(self.datetime.minute),
                to_bcd(self.datetime.second),
            )
        } else {
            (hour, self.datetime.minute, self.datetime.second)
        };

        RtcTime {
            hour,
            minute,
            second,
            daylight_savings: self.daylight_savings as u8,
        }
    }

    pub fn read_date(&self) -> RtcDate {
        let century = (self.datetime.year / 100) as u8;
        let year = (self.datetime.year % 100) as u8;
        let (century, year, month, day) = if self.bcd_mode {
            (
                to_bcd(century),
                to_bcd(year),
                to_bcd(self.datetime.month),
                to_bcd(self.datetime.day),
            )
        } else {
            (century, year, self.datetime.month, self.datetime.day)
        };

        RtcDate {
            century,
            year,
            month,
            day,
        }
    }

    fn decode_field(&self, value: u8) -> Option<u8> {
        if self.bcd_mode {
            from_bcd(value)
        } else {
            Some(value)
        }
    }

    fn decode_hour(&self, hour: u8) -> Option<u8> {
        if self.hour_24 {
            let hour = self.decode_field(hour)?;
            if hour < 24 {
                Some(hour)
            } else {
                None
            }
        } else {
            let pm = (hour & 0x80) != 0;
            let raw = self.decode_field(hour & 0x7F)?;
            if !(1..=12).contains(&raw) {
                return None;
            }
            let mut hour = raw % 12;
            if pm {
                hour += 12;
            }
            Some(hour)
        }
    }
}

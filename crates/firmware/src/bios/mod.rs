mod bda_time;
mod int10_vbe;
mod int1a;

pub use bda_time::{BdaTime, BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};

use crate::{memory::MemoryBus, rtc::CmosRtc};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Bios {
    pub rtc: CmosRtc,
    bda_time: BdaTime,
}

impl Bios {
    pub fn new(rtc: CmosRtc) -> Self {
        let bda_time = BdaTime::from_rtc(&rtc);
        Self { rtc, bda_time }
    }

    pub fn init(&mut self, memory: &mut impl MemoryBus) {
        self.bda_time.write_to_bda(memory);
    }

    pub fn advance_time(&mut self, memory: &mut impl MemoryBus, delta: Duration) {
        self.rtc.advance(delta);
        self.bda_time.advance(memory, delta);
    }
}


use aero_timers::DeviceTimer;
use aero_timers::LocalApicTimer;
use aero_time::{TimeSource, TimerQueue, Tsc};

use crate::CpuRegs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msr {
    Ia32Tsc = 0x10,
    Ia32TscAux = 0xC000_0103,
    Ia32TscDeadline = 0x6E0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsrError {
    Unsupported,
}

#[derive(Debug)]
pub struct TimeInstructions {
    pub regs: CpuRegs,
    pub tsc: Tsc,
    pub apic_timer: LocalApicTimer,
}

impl TimeInstructions {
    pub fn new(tsc_freq_hz: u64, apic_timer_freq_hz: u64) -> Self {
        Self {
            regs: CpuRegs::default(),
            tsc: Tsc::new(tsc_freq_hz),
            apic_timer: LocalApicTimer::new(apic_timer_freq_hz),
        }
    }

    pub fn instr_rdtsc(&mut self, guest_now_ns: u64) {
        let tsc = self.tsc.read(guest_now_ns);
        self.regs.rax = (tsc & 0xffff_ffff) as u64;
        self.regs.rdx = (tsc >> 32) as u64;
    }

    pub fn instr_rdtscp(&mut self, guest_now_ns: u64) {
        let (tsc, aux) = self.tsc.read_rdtscp(guest_now_ns);
        self.regs.rax = (tsc & 0xffff_ffff) as u64;
        self.regs.rdx = (tsc >> 32) as u64;
        self.regs.rcx = aux as u64;
    }

    pub fn wrmsr(
        &mut self,
        msr: u32,
        value: u64,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
    ) -> Result<(), MsrError> {
        match msr {
            x if x == Msr::Ia32Tsc as u32 => {
                self.tsc.write(guest_now_ns, value);
                Ok(())
            }
            x if x == Msr::Ia32TscAux as u32 => {
                self.tsc.set_aux(value as u32);
                Ok(())
            }
            x if x == Msr::Ia32TscDeadline as u32 => {
                self.apic_timer
                    .write_tsc_deadline(guest_now_ns, value, &self.tsc, queue);
                Ok(())
            }
            _ => Err(MsrError::Unsupported),
        }
    }

    /// Helper for implementing `HLT` in a host-backed run loop.
    ///
    /// When the CPU is halted with interrupts enabled, callers should:
    /// 1. Poll devices/timers and inject any pending interrupts.
    /// 2. If nothing is pending, call this function to park the current thread until the next
    ///    scheduled timer deadline.
    pub fn hlt_wait(time: &TimeSource, queue: &mut TimerQueue<DeviceTimer>) {
        let Some(deadline_ns) = queue.next_deadline_ns() else {
            std::thread::park();
            return;
        };
        match time.host_duration_until_guest_ns(deadline_ns) {
            Some(dur) if dur.is_zero() => {}
            Some(dur) => std::thread::park_timeout(dur),
            None => std::thread::park(),
        }
    }
}

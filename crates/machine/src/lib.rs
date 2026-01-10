//! Minimal x86 "machine" building blocks used by the firmware crate.
//!
//! The real Aero project will likely split these across dedicated crates
//! (CPU, MMU, devices). For the BIOS firmware task we only need:
//! - A real-mode CPU state representation + a tiny interpreter for a handful
//!   of instructions (`INT`, `IRET`, `HLT`, `NOP`, `CLI`, `STI`, `JMP rel8`).
//! - A physical memory implementation with A20 gating and a read-only ROM
//!   region mechanism.
//! - A sector-based block device abstraction for boot disks.

pub mod cpu;
pub mod disk;
pub mod memory;

pub use cpu::{
    CpuExit, CpuState, Segment, FLAG_AF, FLAG_ALWAYS_ON, FLAG_CF, FLAG_DF, FLAG_IF, FLAG_OF,
    FLAG_PF, FLAG_SF, FLAG_TF, FLAG_ZF,
};
pub use disk::{BlockDevice, DiskError, InMemoryDisk};
pub use memory::{A20Gate, FirmwareMemory, MemoryAccess, PhysicalMemory};

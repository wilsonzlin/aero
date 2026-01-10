//! Guest physical RAM and a routing layer for MMIO/ROM.
//!
//! The design goal is to support large guest RAM sizes without eagerly reserving
//! multi‑gigabyte host memory. [`PhysicalMemory`] uses a sparse, lazy allocation
//! strategy (configurable chunk size) and offers efficient bulk APIs that are
//! friendly to DMA-style device models.
//!
//! [`MemoryBus`] is a simple physical address router that lets devices register
//! MMIO windows and ROM regions. MMIO/ROM regions take priority over RAM.

#![forbid(unsafe_code)]

mod memory_bus;
mod physical_memory;

pub use memory_bus::{MemoryBus, MemoryBusError, MmioHandler};
pub use physical_memory::{PhysicalMemory, PhysicalMemoryError, PhysicalMemoryOptions};

#![forbid(unsafe_code)]

pub mod address_filter;
pub mod audio;
pub mod chipset;
pub mod dirty_memory;
pub mod interrupts;
pub mod io;
pub mod memory;
pub mod platform;
pub mod reset;
pub mod time;

pub use chipset::{A20GateHandle, ChipsetState};
pub use platform::Platform;
pub use reset::{PlatformResetSink, ResetKind, ResetLatch};

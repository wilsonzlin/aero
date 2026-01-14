#[cfg(feature = "legacy-audio")]
pub mod audio;
pub mod input;
pub mod net;
pub mod pci;
pub mod serial;
pub mod storage;
pub mod usb;

pub use aero_platform::io::PortIoDevice;

#[deprecated(note = "Use aero_platform::io::PortIoDevice")]
pub use aero_platform::io::PortIoDevice as PortIO;

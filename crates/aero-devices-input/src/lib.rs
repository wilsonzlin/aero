//! Input device models (PS/2 keyboard + mouse) and the i8042 controller.
//!
//! This crate is intentionally self-contained so it can be reused by both native
//! tests and the WASM build used by the browser host.

pub mod i8042;
pub mod ps2_keyboard;
pub mod ps2_mouse;
pub mod scancode;

pub use crate::i8042::{I8042Controller, IrqSink, SystemControlSink};
pub use crate::ps2_keyboard::Ps2Keyboard;
pub use crate::ps2_mouse::{Ps2Mouse, Ps2MouseButton};
pub use crate::scancode::{browser_code_to_set2, Set2Scancode};

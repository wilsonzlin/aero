//! Triangle-domain tessellation math helpers.
//!
//! The core tessellator helpers live at [`crate::runtime::tessellator`] so they can be reused by
//! non-HS/DS expansion paths. Re-export them here for convenience so tessellation-related code can
//! import everything from the `runtime::tessellation` namespace.

pub use crate::runtime::tessellator::*;


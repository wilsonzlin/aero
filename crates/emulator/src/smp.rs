//! Legacy re-export of the deterministic SMP/APIC model.
//!
//! The minimal SMP/APIC + snapshot harness was extracted into `aero-smp-model` to avoid
//! collisions/confusion with the canonical `aero_machine::Machine`. This module is retained for
//! backwards compatibility behind the `legacy-smp-model` feature.

pub use aero_smp_model::*;

#[deprecated(note = "Use aero_smp_model::SmpMachine instead (or depend on aero-smp-model directly).")]
pub type Machine = aero_smp_model::SmpMachine;


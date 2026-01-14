#![doc = r#"
Device + I/O stack crate for Aero.

## Canonical VM entrypoint

The canonical full-system machine wiring lives in `crates/aero-machine` as [`aero_machine::Machine`].

If you're already depending on `crates/emulator`, the recommended entrypoint is
[`crate::machine::Machine`], which is a re-export of the canonical `aero-machine` type (so
`emulator::machine::Machine` == `aero_machine::Machine`).
"#]
#![cfg_attr(
    not(feature = "legacy-audio"),
    doc = r#"

## Legacy audio stack (feature gated)

The `emulator` crate contains a legacy audio stack (AC97/HDA/DSP)
that is intentionally **not** built by default.

These compile-fail doctests ensure the default feature set does not accidentally expose the legacy
modules, while still allowing them to be enabled for targeted testing via `--features legacy-audio`.

```compile_fail
use emulator::io::audio;
fn main() {}
```

```compile_fail
use emulator::io::virtio;
fn main() {}
```
"#
)]
#![cfg_attr(
    not(feature = "legacy-usb-ehci"),
    doc = r#"

## Legacy USB EHCI wrapper (feature gated)

The emulator previously carried a thin PCI/MMIO wrapper around the `aero-usb` EHCI controller model.
This is not part of the canonical VM device stack, so it is intentionally hidden behind
`--features legacy-usb-ehci`.

```compile_fail
use emulator::io::usb::ehci;
fn main() {}
```
"#
)]
#![cfg_attr(
    not(feature = "legacy-usb-xhci"),
    doc = r#"

## Legacy USB xHCI wrapper (feature gated)

The emulator previously carried a thin PCI/MMIO wrapper around the `aero-usb` xHCI controller model.
This is not part of the canonical VM device stack, so it is intentionally hidden behind
`--features legacy-usb-xhci`.

```compile_fail
use emulator::io::usb::xhci;
fn main() {}
```
"#
)]

#[cfg(feature = "legacy-audio")]
pub mod audio;
pub mod devices;
pub mod display;
pub mod gpu_worker;
pub mod io;
pub mod machine;

// The deterministic SMP/APIC model (and snapshot validation harness) lives in its own crate to
// avoid collisions/confusion with `aero_machine::Machine`. Keep this as an opt-in legacy re-export.
#[cfg(feature = "legacy-smp-model")]
pub mod smp;

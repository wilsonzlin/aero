#![cfg_attr(
    not(feature = "legacy-audio"),
    doc = r#"
The `emulator` crate contains a legacy audio stack (AC97/HDA/DSP and an older virtio-snd device model)
that is intentionally **not** built by default.

These compile-fail doctests ensure the default feature set does not accidentally expose the legacy
modules, while still allowing them to be enabled for targeted testing via `--features legacy-audio`.

```compile_fail
use emulator::io::audio;
fn main() {}
```

```compile_fail
use emulator::io::virtio::devices::snd;
fn main() {}
```
"#
)]

#[cfg(feature = "legacy-audio")]
pub mod audio;
pub mod chipset;
pub mod devices;
pub mod display;
pub mod gpu_worker;
pub mod in_capture;
pub mod io;
pub mod memory_bus;
pub mod smp;

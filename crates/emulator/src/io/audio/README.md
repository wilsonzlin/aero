# Legacy emulator audio (`emulator/legacy-audio`)

This directory contains the **legacy** audio device models and helpers for the `emulator` crate
(AC'97 / Intel HDA + a small DSP pipeline). It is retained for reference and targeted testing and is
**not** part of the browser/WASM runtime path.

## Feature gate

This code is compiled only when the `emulator/legacy-audio` feature is enabled (the `legacy-audio`
crate feature on `emulator`):

```bash
cargo test -p emulator --features legacy-audio
```

## What’s in here?

- `ac97`: AC'97 controller model and DMA plumbing.
- `hda`: Intel High Definition Audio (HDA) controller/codec model.
- `dsp`: PCM decode/convert + channel remixing + resampling (`sinc-resampler` optional).

## Tests

```bash
cargo test -p emulator --features legacy-audio
cargo test -p emulator --features "legacy-audio sinc-resampler"
```

## DSP benchmark

```bash
cargo bench -p emulator --bench dsp --features legacy-audio
```

## Canonical replacements

The current audio stack lives in:

- `crates/aero-audio`
- `crates/aero-virtio`
- `crates/platform::audio`

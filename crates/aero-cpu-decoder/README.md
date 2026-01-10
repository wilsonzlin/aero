# `aero-cpu-decoder`

Production-grade x86/x86-64 instruction decoding for Aero.

## Highlights

- Decodes 16/32/64-bit instructions (including Windows 7 boot/runtime code).
- Supports legacy prefixes, REX, and VEX/EVEX/XOP prefix *detection* (metadata hooks).
- Deterministic and allocation-free in the hot path (`decode_instruction()` / `decode_one()`).
- Broad instruction coverage via the table-driven `iced-x86` backend.

## Usage

```rust
use aero_cpu_decoder::{decode_instruction, DecodeMode, Register};

let bytes = [0x48, 0x8B, 0x05, 0x78, 0x56, 0x34, 0x12];
let inst = decode_instruction(DecodeMode::Bits64, 0x1000, &bytes).unwrap();

assert_eq!(inst.memory_base(), Register::RIP);
```

## Tests

```bash
cargo test -p aero-cpu-decoder
```

The suite includes:

- targeted unit tests (prefix parsing, ModR/M+SIB addressing cases)
- golden tests vs Capstone for thousands of random instructions (length agreement)
- property fuzz tests (`proptest`) for “no panics + sane lengths”
- allocation guard test ensuring `decode_one()` does not allocate per instruction

## Coverage

Suggested tooling:

```bash
# install once: cargo install cargo-llvm-cov
cargo llvm-cov -p aero-cpu-decoder --html
```

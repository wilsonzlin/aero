pub mod vbe;
pub mod vga;

// NOTE: The authoritative AeroGPU PCI/MMIO device models live in
// `crates/emulator/src/devices/pci/aerogpu*.rs` and their corresponding guest↔host ABI headers in
// `drivers/aerogpu/protocol/*` (mirrored in `emulator/protocol`).
//
// See `docs/graphics/aerogpu-protocols.md` for an overview of similarly named in-tree protocols.

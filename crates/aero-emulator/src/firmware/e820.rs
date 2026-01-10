//! E820 memory map helpers.
//!
//! For VBE LFB we reserve a fixed MMIO region in the PCI hole so bootloaders
//! don't accidentally treat it as RAM.

use crate::devices::vbe::{VBE_LFB_BASE, VBE_LFB_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum E820Type {
    Ram = 1,
    Reserved = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct E820Entry {
    pub addr: u64,
    pub len: u64,
    pub entry_type: E820Type,
}

pub fn build_e820_map(ram_size: u64) -> Vec<E820Entry> {
    let lfb_base = VBE_LFB_BASE as u64;
    let lfb_size = VBE_LFB_SIZE as u64;

    let usable_ram = ram_size.min(lfb_base);

    vec![
        E820Entry {
            addr: 0,
            len: usable_ram,
            entry_type: E820Type::Ram,
        },
        E820Entry {
            addr: lfb_base,
            len: lfb_size,
            entry_type: E820Type::Reserved,
        },
    ]
}

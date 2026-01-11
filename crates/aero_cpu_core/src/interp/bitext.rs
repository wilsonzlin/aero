pub fn popcnt(val: u64, width_bits: u32) -> u32 {
    match width_bits {
        16 => (val as u16).count_ones(),
        32 => (val as u32).count_ones(),
        64 => val.count_ones(),
        _ => unreachable!("unsupported popcnt width"),
    }
}

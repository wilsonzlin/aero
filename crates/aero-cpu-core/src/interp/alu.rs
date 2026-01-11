use crate::cpu::RFlags;

fn mask_for_size(size: usize) -> u64 {
    let bits = (size * 8) as u32;
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn sign_bit(size: usize) -> u64 {
    1u64 << ((size * 8) as u32 - 1)
}

fn parity(byte: u8) -> bool {
    byte.count_ones() % 2 == 0
}

pub fn sub_with_flags(
    rflags: &mut RFlags,
    dest: u64,
    src: u64,
    borrow_in: bool,
    size: usize,
) -> u64 {
    let mask = mask_for_size(size);
    let dest = dest & mask;
    let src = src & mask;
    let borrow = borrow_in as u64;
    let src2 = src.wrapping_add(borrow) & mask;
    let subtrahend = (src as u128) + (borrow as u128);
    let result = (dest as u128).wrapping_sub(subtrahend) as u64 & mask;

    let sb = sign_bit(size);
    rflags.set(RFlags::CF, (dest as u128) < subtrahend);
    rflags.set(RFlags::ZF, result == 0);
    rflags.set(RFlags::SF, (result & sb) != 0);
    rflags.set(RFlags::OF, ((dest ^ src2) & (dest ^ result) & sb) != 0);
    rflags.set(RFlags::AF, ((dest ^ src2 ^ result) & 0x10) != 0);
    rflags.set(RFlags::PF, parity(result as u8));

    result
}

pub fn update_sub_flags(rflags: &mut RFlags, dest: u64, src: u64, size: usize) {
    let _ = sub_with_flags(rflags, dest, src, false, size);
}

pub fn add_with_flags(
    rflags: &mut RFlags,
    dest: u64,
    src: u64,
    carry_in: bool,
    size: usize,
) -> u64 {
    let mask = mask_for_size(size);
    let dest = dest & mask;
    let src = src & mask;
    let carry = carry_in as u64;
    let full = (dest as u128) + (src as u128) + (carry as u128);
    let result = (full as u64) & mask;

    let sb = sign_bit(size);
    rflags.set(RFlags::CF, full > mask as u128);
    rflags.set(RFlags::ZF, result == 0);
    rflags.set(RFlags::SF, (result & sb) != 0);
    rflags.set(RFlags::OF, ((dest ^ result) & (src ^ result) & sb) != 0);
    rflags.set(RFlags::AF, ((dest ^ src ^ result) & 0x10) != 0);
    rflags.set(RFlags::PF, parity(result as u8));

    result
}

pub fn logic_with_flags(rflags: &mut RFlags, result: u64, size: usize) -> u64 {
    let mask = mask_for_size(size);
    let result = result & mask;

    rflags.set(RFlags::CF, false);
    rflags.set(RFlags::OF, false);

    let sb = sign_bit(size);
    rflags.set(RFlags::ZF, result == 0);
    rflags.set(RFlags::SF, (result & sb) != 0);
    rflags.set(RFlags::PF, parity(result as u8));

    result
}

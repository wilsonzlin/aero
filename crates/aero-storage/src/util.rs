use crate::{DiskError, Result};

pub fn align_up_u64(value: u64, alignment: u64) -> Result<u64> {
    if alignment == 0 {
        return Err(DiskError::OffsetOverflow);
    }
    let mask = alignment - 1;
    if value & mask == 0 {
        return Ok(value);
    }
    value
        .checked_add(alignment - (value & mask))
        .ok_or(DiskError::OffsetOverflow)
}

pub fn div_ceil_u64(n: u64, d: u64) -> Result<u64> {
    if d == 0 {
        return Err(DiskError::OffsetOverflow);
    }
    Ok((n / d) + u64::from(n % d != 0))
}

pub fn checked_range(offset: u64, len: usize, capacity: u64) -> Result<()> {
    let end = offset
        .checked_add(len as u64)
        .ok_or(DiskError::OffsetOverflow)?;
    if end > capacity {
        return Err(DiskError::OutOfBounds {
            offset,
            len,
            capacity,
        });
    }
    Ok(())
}

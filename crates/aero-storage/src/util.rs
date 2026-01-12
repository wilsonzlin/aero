use crate::{DiskError, Result};

pub fn align_up_u64(value: u64, alignment: u64) -> Result<u64> {
    if alignment == 0 {
        return Err(DiskError::OffsetOverflow);
    }
    let rem = value % alignment;
    if rem == 0 {
        return Ok(value);
    }
    value
        .checked_add(alignment - rem)
        .ok_or(DiskError::OffsetOverflow)
}

pub fn div_ceil_u64(n: u64, d: u64) -> Result<u64> {
    if d == 0 {
        return Err(DiskError::OffsetOverflow);
    }
    Ok(n.div_ceil(d))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_u64_supports_non_power_of_two_alignments() {
        assert_eq!(align_up_u64(0, 10).unwrap(), 0);
        assert_eq!(align_up_u64(20, 10).unwrap(), 20);
        assert_eq!(align_up_u64(1, 10).unwrap(), 10);
        assert_eq!(align_up_u64(12, 10).unwrap(), 20);
    }

    #[test]
    fn align_up_u64_errors_on_zero_alignment() {
        assert!(matches!(
            align_up_u64(1, 0).unwrap_err(),
            DiskError::OffsetOverflow
        ));
    }

    #[test]
    fn align_up_u64_reports_overflow() {
        // u64::MAX is not 10-byte aligned and cannot be rounded up without overflowing.
        assert!(matches!(
            align_up_u64(u64::MAX, 10).unwrap_err(),
            DiskError::OffsetOverflow
        ));
    }
}

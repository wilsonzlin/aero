use std::io::{Read, Write};

use crate::error::{Result, SnapshotError};

pub trait WriteLeExt: Write {
    fn write_u8(&mut self, v: u8) -> Result<()> {
        self.write_all(&[v])?;
        Ok(())
    }

    fn write_u16_le(&mut self, v: u16) -> Result<()> {
        self.write_all(&v.to_le_bytes())?;
        Ok(())
    }

    fn write_u32_le(&mut self, v: u32) -> Result<()> {
        self.write_all(&v.to_le_bytes())?;
        Ok(())
    }

    fn write_u64_le(&mut self, v: u64) -> Result<()> {
        self.write_all(&v.to_le_bytes())?;
        Ok(())
    }

    fn write_u128_le(&mut self, v: u128) -> Result<()> {
        self.write_all(&v.to_le_bytes())?;
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.write_all(bytes)?;
        Ok(())
    }

    fn write_len_prefixed_bytes_u32(&mut self, bytes: &[u8]) -> Result<()> {
        let len: u32 = bytes
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("length does not fit in u32"))?;
        self.write_u32_le(len)?;
        self.write_bytes(bytes)?;
        Ok(())
    }

    fn write_string_u32(&mut self, s: &str) -> Result<()> {
        self.write_len_prefixed_bytes_u32(s.as_bytes())
    }
}

impl<T: Write + ?Sized> WriteLeExt for T {}

pub trait ReadLeExt: Read {
    fn read_u8(&mut self) -> Result<u8> {
        let mut buf = [0u8; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32_le(&mut self) -> Result<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64_le(&mut self) -> Result<u64> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128_le(&mut self) -> Result<u128> {
        let mut buf = [0u8; 16];
        self.read_exact(&mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn read_exact_vec(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        buf.try_reserve_exact(len)
            .map_err(|_| SnapshotError::OutOfMemory { len })?;
        buf.resize(len, 0);
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_exact_into_vec(&mut self, buf: &mut Vec<u8>, len: usize) -> Result<()> {
        if len > buf.len() {
            buf.try_reserve_exact(len - buf.len())
                .map_err(|_| SnapshotError::OutOfMemory { len })?;
        }
        buf.resize(len, 0);
        self.read_exact(buf)?;
        Ok(())
    }
}

impl<T: Read + ?Sized> ReadLeExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_exact_vec_allocation_failure_returns_error() {
        let mut cursor = Cursor::new(Vec::new());
        let err = cursor.read_exact_vec(usize::MAX).unwrap_err();
        assert!(matches!(err, SnapshotError::OutOfMemory { .. }));
    }

    #[test]
    fn read_exact_into_vec_allocation_failure_returns_error() {
        let mut cursor = Cursor::new(Vec::new());
        let mut buf = Vec::new();
        let err = cursor.read_exact_into_vec(&mut buf, usize::MAX).unwrap_err();
        assert!(matches!(err, SnapshotError::OutOfMemory { .. }));
    }
}

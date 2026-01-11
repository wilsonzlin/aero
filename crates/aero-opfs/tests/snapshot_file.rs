#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Seek, SeekFrom, Write};

use aero_opfs::io::snapshot_file::{OpfsSyncFile, OpfsSyncFileHandle};

#[derive(Default, Debug)]
struct MockHandle {
    data: Vec<u8>,
}

impl OpfsSyncFileHandle for MockHandle {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = &self.data[offset..];
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        Ok(len)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;

        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn get_size(&mut self) -> std::io::Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn truncate(&mut self, size: u64) -> std::io::Result<()> {
        let size: usize = size
            .try_into()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "size overflow"))?;
        self.data.resize(size, 0);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn close(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn read_to_end_seek_start<R: Read + Seek>(mut r: R) -> Vec<u8> {
    r.seek(SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    out
}

#[test]
fn sequential_write_then_read_back() {
    let mut file = OpfsSyncFile::from_handle(MockHandle::default());
    file.write_all(b"hello").unwrap();
    file.write_all(b" world").unwrap();

    file.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = [0u8; 11];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello world");
}

#[test]
fn seek_and_overwrite() {
    let mut file = OpfsSyncFile::from_handle(MockHandle::default());
    file.write_all(b"abcdef").unwrap();

    file.seek(SeekFrom::Start(2)).unwrap();
    file.write_all(b"ZZ").unwrap();

    assert_eq!(read_to_end_seek_start(&mut file), b"abZZef");
}

#[test]
fn seek_from_end_reads_tail() {
    let mut file = OpfsSyncFile::from_handle(MockHandle::default());
    file.write_all(b"hello world").unwrap();

    let pos = file.seek(SeekFrom::End(-5)).unwrap();
    assert_eq!(pos, 6);

    let mut tail = [0u8; 5];
    file.read_exact(&mut tail).unwrap();
    assert_eq!(&tail, b"world");
}

#[test]
fn truncate_then_write() {
    let mut file = OpfsSyncFile::from_handle(MockHandle::default());
    file.write_all(b"abcdefghij").unwrap();

    file.truncate(5).unwrap();
    let pos = file.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(pos, 5);

    file.write_all(b"XYZ").unwrap();
    assert_eq!(read_to_end_seek_start(&mut file), b"abcdeXYZ");
}


use aero_storage::{BlockCachedDisk, MemBackend, RawDisk, Result, VirtualDisk, SECTOR_SIZE};
use std::time::Instant;

fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn main() -> Result<()> {
    let disk_size_mb: u64 = std::env::var("AERO_BENCH_SIZE_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);
    let disk_size = disk_size_mb * 1024 * 1024;

    let block_size: usize = std::env::var("AERO_BENCH_BLOCK_KB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(1024 * 1024) as usize;

    let raw = RawDisk::create(MemBackend::new(), disk_size).unwrap();
    let mut disk = BlockCachedDisk::new(raw, block_size, 64).unwrap();

    let mut buf = vec![0u8; block_size];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i & 0xFF) as u8;
    }

    // Sequential write.
    let start = Instant::now();
    let mut written = 0u64;
    let mut off = 0u64;
    while off < disk_size {
        let len = (disk_size - off).min(buf.len() as u64) as usize;
        disk.write_at(off, &buf[..len])?;
        written += len as u64;
        off += len as u64;
    }
    disk.flush()?;
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "sequential write: {:.1} MiB/s",
        written as f64 / (1024.0 * 1024.0) / elapsed
    );

    // Sequential read.
    let start = Instant::now();
    let mut read = 0u64;
    let mut off = 0u64;
    while off < disk_size {
        let len = (disk_size - off).min(buf.len() as u64) as usize;
        disk.read_at(off, &mut buf[..len])?;
        read += len as u64;
        off += len as u64;
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "sequential read: {:.1} MiB/s",
        read as f64 / (1024.0 * 1024.0) / elapsed
    );

    // Random 4 KiB reads.
    let io_size = 4096usize;
    let ops = 50_000u64;
    let start = Instant::now();
    let mut rng = 0x1234_5678_9ABC_DEF0u64;
    let mut tmp = vec![0u8; io_size];
    let max_lba = (disk_size - io_size as u64) / SECTOR_SIZE as u64;
    for _ in 0..ops {
        let lba = xorshift64(&mut rng) % max_lba.max(1);
        let offset = lba * SECTOR_SIZE as u64;
        disk.read_at(offset, &mut tmp)?;
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!("random read: {:.0} ops/s", ops as f64 / elapsed);

    Ok(())
}

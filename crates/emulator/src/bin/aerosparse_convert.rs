#[cfg(target_arch = "wasm32")]
fn main() {
    eprintln!("aerosparse_convert is only supported on non-wasm targets");
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use std::env;

    use aero_storage::{FileBackend, StorageBackend as _};
    use emulator::io::storage::disk::DiskBackend;
    use emulator::io::storage::formats::{aerosprs, SparseDisk};

    fn print_usage() {
        eprintln!("Usage: aerosparse_convert <input.aerosprs> <output.aerospar>");
    }

    let mut args = env::args().skip(1);
    let Some(input_path) = args.next() else {
        print_usage();
        std::process::exit(2);
    };
    let Some(output_path) = args.next() else {
        print_usage();
        std::process::exit(2);
    };
    if args.next().is_some() {
        print_usage();
        std::process::exit(2);
    }

    let mut input = match FileBackend::open_read_only(&input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open input: {e}");
            std::process::exit(1);
        }
    };

    let mut magic = [0u8; 8];
    if let Err(e) = input.read_at(0, &mut magic) {
        eprintln!("failed to read magic: {e}");
        std::process::exit(1);
    }
    if magic != *b"AEROSPRS" {
        eprintln!("input is not an AEROSPRS image");
        std::process::exit(1);
    }

    let mut legacy = match aerosprs::SparseDisk::open(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to open legacy image: {e}");
            std::process::exit(1);
        }
    };

    let header = legacy.header().clone();
    let disk_size_bytes = match header.total_sectors.checked_mul(header.sector_size as u64) {
        Some(v) => v,
        None => {
            eprintln!("disk size overflow");
            std::process::exit(1);
        }
    };
    if disk_size_bytes % 512 != 0 {
        eprintln!("legacy disk size is not a multiple of 512 bytes");
        std::process::exit(1);
    }
    let total_sectors_512 = disk_size_bytes / 512;

    let legacy_block_size = header.block_size;
    let output_block_size = if legacy_block_size.is_power_of_two() && legacy_block_size % 512 == 0 {
        legacy_block_size
    } else {
        1024 * 1024
    };

    let out_backend = match FileBackend::create(&output_path, 0) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to create output: {e}");
            std::process::exit(1);
        }
    };

    let mut out = match SparseDisk::create(out_backend, 512, total_sectors_512, output_block_size) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to create output disk: {e}");
            std::process::exit(1);
        }
    };

    let allocated_blocks: Vec<u64> = legacy
        .allocated_blocks()
        .map(|(logical_block, _phys)| logical_block)
        .collect();

    let block_size_bytes = legacy_block_size as u64;
    let sector_size_bytes = header.sector_size as u64;
    let mut buf = vec![0u8; legacy_block_size as usize];

    let mut copied = 0u64;
    for logical_block in allocated_blocks {
        let byte_offset = match logical_block.checked_mul(block_size_bytes) {
            Some(v) => v,
            None => {
                eprintln!("offset overflow at logical block {logical_block}");
                std::process::exit(1);
            }
        };
        if byte_offset >= disk_size_bytes {
            continue;
        }
        let remaining = disk_size_bytes - byte_offset;
        let len_bytes = block_size_bytes.min(remaining);
        let len_usize = len_bytes as usize;

        let legacy_lba = byte_offset / sector_size_bytes;
        if let Err(e) = legacy.read_sectors(legacy_lba, &mut buf[..len_usize]) {
            eprintln!("failed to read legacy block {logical_block}: {e}");
            std::process::exit(1);
        }

        if buf[..len_usize].iter().all(|b| *b == 0) {
            continue;
        }

        let out_lba = byte_offset / 512;
        if let Err(e) = out.write_sectors(out_lba, &buf[..len_usize]) {
            eprintln!("failed to write output block {logical_block}: {e}");
            std::process::exit(1);
        }

        copied += 1;
        if copied.is_multiple_of(1024) {
            eprintln!("copied {copied} blocks...");
        }
    }

    if let Err(e) = out.flush() {
        eprintln!("failed to flush output: {e}");
        std::process::exit(1);
    }

    eprintln!("done; copied {copied} allocated blocks");
}

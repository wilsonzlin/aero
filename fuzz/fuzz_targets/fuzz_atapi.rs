#![no_main]

use std::io;

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};

#[derive(Clone)]
struct MemIso {
    sector_count: u32,
    data: Vec<u8>,
}

impl MemIso {
    fn new(sector_count: u32, init: &[u8]) -> Self {
        let bytes_len = sector_count as usize * AtapiCdrom::SECTOR_SIZE;
        let mut data = vec![0u8; bytes_len];
        if !data.is_empty() && !init.is_empty() {
            // Repeat attacker-controlled bytes so the disc image isn't mostly zeros even when the
            // fuzzer input is small (libFuzzer default max_len=4096).
            let mut off = 0usize;
            while off < data.len() {
                let take = (data.len() - off).min(init.len());
                data[off..off + take].copy_from_slice(&init[..take]);
                off += take;
            }
        }
        Self { sector_count, data }
    }
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        if !buf.len().is_multiple_of(AtapiCdrom::SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned ATAPI read length",
            ));
        }

        let start = (lba as u64)
            .checked_mul(AtapiCdrom::SECTOR_SIZE as u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "length overflow"))?;

        if end > self.data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read beyond end of ISO",
            ));
        }

        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

fn clamp_read_10(packet: &mut [u8; 12], sector_count: u32) {
    // READ(10): LBA in bytes 2..=5, transfer length in bytes 7..=8.
    let mut lba = u32::from_be_bytes([packet[2], packet[3], packet[4], packet[5]]);
    let mut blocks = u16::from_be_bytes([packet[7], packet[8]]) as u32;
    // Keep allocations small for fuzzing (READ buffers allocate `blocks * 2048`).
    blocks = (blocks % 4).max(1);

    if sector_count > 0 {
        let max_lba = sector_count.saturating_sub(blocks).saturating_sub(1);
        if max_lba > 0 {
            lba %= max_lba;
        } else {
            lba = 0;
            blocks = blocks.min(sector_count.max(1));
        }
    } else {
        lba = 0;
    }

    packet[2..6].copy_from_slice(&lba.to_be_bytes());
    packet[7..9].copy_from_slice(&(blocks as u16).to_be_bytes());
}

fn clamp_read_12(packet: &mut [u8; 12], sector_count: u32) {
    // READ(12): LBA in bytes 2..=5, transfer length in bytes 6..=9.
    let mut lba = u32::from_be_bytes([packet[2], packet[3], packet[4], packet[5]]);
    let mut blocks = u32::from_be_bytes([packet[6], packet[7], packet[8], packet[9]]);
    // Keep allocations small for fuzzing (READ buffers allocate `blocks * 2048`).
    blocks = (blocks % 4).max(1);

    if sector_count > 0 {
        let max_lba = sector_count.saturating_sub(blocks).saturating_sub(1);
        if max_lba > 0 {
            lba %= max_lba;
        } else {
            lba = 0;
            blocks = blocks.min(sector_count.max(1));
        }
    } else {
        lba = 0;
    }

    packet[2..6].copy_from_slice(&lba.to_be_bytes());
    packet[6..10].copy_from_slice(&blocks.to_be_bytes());
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Keep the virtual CD small; we only need enough to exercise reads.
    let sectors: u32 = (u.arbitrary::<u16>().unwrap_or(32) as u32 % 128).max(1);

    // Remaining bytes are used to populate the ISO image.
    let rest_len = u.len();
    let init = u.bytes(rest_len).unwrap_or(&[]);
    let backend = MemIso::new(sectors, init);

    let mut cd = AtapiCdrom::new(Some(Box::new(backend)));

    let ops_len: usize = u.int_in_range(0usize..=64).unwrap_or(0);
    for _ in 0..ops_len {
        let kind: u8 = u.arbitrary().unwrap_or(0);
        match kind % 8 {
            0 => cd.eject_media(),
            1 => {
                // Reinsert media with a new image so we exercise media_changed paths.
                let seed: u8 = u.arbitrary().unwrap_or(0);
                let mut pattern = vec![seed; 32];
                if let Ok(extra) = u.bytes(32) {
                    pattern.copy_from_slice(extra);
                }
                cd.insert_media(Box::new(MemIso::new(sectors, &pattern)));
            }
            _ => {
                // Packet execution.
                let dma_requested: bool = u.arbitrary().unwrap_or(false);
                let mut packet = [0u8; 12];
                if let Ok(p) = u.bytes(12) {
                    packet.copy_from_slice(p);
                } else {
                    break;
                }

                match packet[0] {
                    0x28 => clamp_read_10(&mut packet, sectors),
                    0xA8 => clamp_read_12(&mut packet, sectors),
                    _ => {}
                }

                let res = cd.handle_packet(&packet, dma_requested);
                match res {
                    aero_devices_storage::atapi::PacketResult::DataIn(buf)
                    | aero_devices_storage::atapi::PacketResult::DmaIn(buf) => {
                        // Touch the buffer so the compiler can't trivially optimize it away.
                        if !buf.is_empty() {
                            let _ = buf[0];
                            let _ = buf[buf.len() - 1];
                        }
                    }
                    aero_devices_storage::atapi::PacketResult::NoDataSuccess
                    | aero_devices_storage::atapi::PacketResult::Error { .. } => {}
                }
            }
        }
    }
});

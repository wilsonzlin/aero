use std::collections::VecDeque;
use std::io::{Read, Write};

use crate::rtc::CmosRtcSnapshot;

use super::bda_time::BdaTimeSnapshot;
use super::{Bios, BiosConfig, E820Entry};

#[derive(Debug, Clone)]
pub struct BiosSnapshot {
    pub config: BiosConfig,
    pub rtc: CmosRtcSnapshot,
    pub bda_time: BdaTimeSnapshot,
    pub e820_map: Vec<E820Entry>,
    pub keyboard_queue: Vec<u16>,
    pub video_mode: u8,
    pub tty_output: Vec<u8>,
    pub rsdp_addr: Option<u64>,
}

impl BiosSnapshot {
    pub fn encode<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.config.memory_size_bytes.to_le_bytes())?;
        w.write_all(&[self.config.boot_drive])?;
        self.rtc.encode(w)?;
        self.bda_time.encode(w)?;

        let e820_len: u32 = self.e820_map.len().try_into().unwrap_or(u32::MAX);
        w.write_all(&e820_len.to_le_bytes())?;
        for entry in &self.e820_map {
            w.write_all(&entry.base.to_le_bytes())?;
            w.write_all(&entry.length.to_le_bytes())?;
            w.write_all(&entry.region_type.to_le_bytes())?;
            w.write_all(&entry.extended_attributes.to_le_bytes())?;
        }

        let keys_len: u32 = self.keyboard_queue.len().try_into().unwrap_or(u32::MAX);
        w.write_all(&keys_len.to_le_bytes())?;
        for key in &self.keyboard_queue {
            w.write_all(&key.to_le_bytes())?;
        }

        w.write_all(&[self.video_mode])?;

        let tty_len: u32 = self.tty_output.len().try_into().unwrap_or(u32::MAX);
        w.write_all(&tty_len.to_le_bytes())?;
        w.write_all(&self.tty_output)?;

        match self.rsdp_addr {
            Some(addr) => {
                w.write_all(&[1])?;
                w.write_all(&addr.to_le_bytes())?;
            }
            None => w.write_all(&[0])?,
        }

        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> std::io::Result<Self> {
        const MAX_E820_ENTRIES: u32 = 1024;
        const MAX_KEYBOARD_QUEUE: u32 = 8192;
        const MAX_TTY_OUTPUT: u32 = 4 * 1024 * 1024;

        let mut buf8 = [0u8; 8];
        r.read_exact(&mut buf8)?;
        let memory_size_bytes = u64::from_le_bytes(buf8);

        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        let boot_drive = b[0];

        let rtc = CmosRtcSnapshot::decode(r)?;
        let bda_time = BdaTimeSnapshot::decode(r)?;

        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let e820_len = u32::from_le_bytes(buf4).min(MAX_E820_ENTRIES) as usize;
        let mut e820_map = Vec::with_capacity(e820_len);
        for _ in 0..e820_len {
            r.read_exact(&mut buf8)?;
            let base = u64::from_le_bytes(buf8);
            r.read_exact(&mut buf8)?;
            let length = u64::from_le_bytes(buf8);
            r.read_exact(&mut buf4)?;
            let region_type = u32::from_le_bytes(buf4);
            r.read_exact(&mut buf4)?;
            let extended_attributes = u32::from_le_bytes(buf4);
            e820_map.push(E820Entry {
                base,
                length,
                region_type,
                extended_attributes,
            });
        }

        r.read_exact(&mut buf4)?;
        let keys_len = u32::from_le_bytes(buf4).min(MAX_KEYBOARD_QUEUE) as usize;
        let mut keyboard_queue = Vec::with_capacity(keys_len);
        let mut buf2 = [0u8; 2];
        for _ in 0..keys_len {
            r.read_exact(&mut buf2)?;
            keyboard_queue.push(u16::from_le_bytes(buf2));
        }

        r.read_exact(&mut b)?;
        let video_mode = b[0];

        r.read_exact(&mut buf4)?;
        let tty_len = u32::from_le_bytes(buf4).min(MAX_TTY_OUTPUT) as usize;
        let mut tty_output = vec![0u8; tty_len];
        r.read_exact(&mut tty_output)?;

        r.read_exact(&mut b)?;
        let rsdp_addr = match b[0] {
            0 => None,
            1 => {
                r.read_exact(&mut buf8)?;
                Some(u64::from_le_bytes(buf8))
            }
            _ => None,
        };

        Ok(Self {
            config: BiosConfig {
                memory_size_bytes,
                boot_drive,
            },
            rtc,
            bda_time,
            e820_map,
            keyboard_queue,
            video_mode,
            tty_output,
            rsdp_addr,
        })
    }
}

impl Bios {
    pub fn snapshot(&self) -> BiosSnapshot {
        BiosSnapshot {
            config: self.config.clone(),
            rtc: self.rtc.snapshot(),
            bda_time: self.bda_time.snapshot(),
            e820_map: self.e820_map.clone(),
            keyboard_queue: self.keyboard_queue.iter().copied().collect(),
            video_mode: self.video_mode,
            tty_output: self.tty_output.clone(),
            rsdp_addr: self.rsdp_addr,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: BiosSnapshot) {
        self.config = snapshot.config;
        self.rtc.restore_snapshot(snapshot.rtc);
        self.bda_time.restore_snapshot(snapshot.bda_time);
        self.e820_map = snapshot.e820_map;
        self.keyboard_queue = VecDeque::from(snapshot.keyboard_queue);
        self.video_mode = snapshot.video_mode;
        self.tty_output = snapshot.tty_output;
        self.rsdp_addr = snapshot.rsdp_addr;
    }
}

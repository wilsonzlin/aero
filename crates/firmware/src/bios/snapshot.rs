use std::collections::VecDeque;
use std::io::{Read, Write};

use crate::bda::BiosDataArea;
use crate::memory::MemoryBus;
use crate::rtc::CmosRtcSnapshot;
use crate::video::vbe::VbeDevice;

use super::bda_time::BdaTimeSnapshot;
use super::{Bios, BiosConfig, E820Entry};

#[derive(Debug, Clone)]
pub struct VbeSnapshot {
    pub current_mode: Option<u16>,
    pub lfb_base: u32,
    pub bank: u16,
    pub logical_width_pixels: u16,
    pub bytes_per_scan_line: u16,
    pub display_start_x: u16,
    pub display_start_y: u16,
    pub dac_width_bits: u8,
    pub palette: [u8; 256 * 4],
}

impl Default for VbeSnapshot {
    fn default() -> Self {
        Self::from_device(&VbeDevice::new())
    }
}

impl VbeSnapshot {
    fn from_device(dev: &VbeDevice) -> Self {
        Self {
            current_mode: dev.current_mode,
            lfb_base: dev.lfb_base,
            bank: dev.bank,
            logical_width_pixels: dev.logical_width_pixels,
            bytes_per_scan_line: dev.bytes_per_scan_line,
            display_start_x: dev.display_start_x,
            display_start_y: dev.display_start_y,
            dac_width_bits: dev.dac_width_bits,
            palette: dev.palette,
        }
    }

    fn restore(&self, dev: &mut VbeDevice) {
        dev.current_mode = self.current_mode;
        dev.lfb_base = self.lfb_base;
        dev.bank = self.bank;
        dev.logical_width_pixels = self.logical_width_pixels;
        dev.bytes_per_scan_line = self.bytes_per_scan_line;
        dev.display_start_x = self.display_start_x;
        dev.display_start_y = self.display_start_y;
        dev.dac_width_bits = self.dac_width_bits;
        dev.palette = self.palette;
    }

    fn encode<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        match self.current_mode {
            Some(mode) => {
                w.write_all(&[1])?;
                w.write_all(&mode.to_le_bytes())?;
            }
            None => w.write_all(&[0])?,
        }
        w.write_all(&self.lfb_base.to_le_bytes())?;
        w.write_all(&self.bank.to_le_bytes())?;
        w.write_all(&self.logical_width_pixels.to_le_bytes())?;
        w.write_all(&self.bytes_per_scan_line.to_le_bytes())?;
        w.write_all(&self.display_start_x.to_le_bytes())?;
        w.write_all(&self.display_start_y.to_le_bytes())?;
        w.write_all(&[self.dac_width_bits])?;
        w.write_all(&self.palette)?;
        Ok(())
    }

    fn decode<R: Read>(r: &mut R) -> std::io::Result<Self> {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)?;
        let current_mode = match tag[0] {
            0 => None,
            1 => {
                let mut buf2 = [0u8; 2];
                r.read_exact(&mut buf2)?;
                Some(u16::from_le_bytes(buf2))
            }
            _ => None,
        };

        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let lfb_base = u32::from_le_bytes(buf4);

        let mut buf2 = [0u8; 2];
        r.read_exact(&mut buf2)?;
        let bank = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf2)?;
        let logical_width_pixels = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf2)?;
        let bytes_per_scan_line = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf2)?;
        let display_start_x = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf2)?;
        let display_start_y = u16::from_le_bytes(buf2);

        r.read_exact(&mut tag)?;
        let dac_width_bits = tag[0];

        let mut palette = [0u8; 256 * 4];
        r.read_exact(&mut palette)?;

        Ok(Self {
            current_mode,
            lfb_base,
            bank,
            logical_width_pixels,
            bytes_per_scan_line,
            display_start_x,
            display_start_y,
            dac_width_bits,
            palette,
        })
    }
}

#[derive(Debug, Clone)]
pub struct BiosSnapshot {
    pub config: BiosConfig,
    pub rtc: CmosRtcSnapshot,
    pub bda_time: BdaTimeSnapshot,
    pub e820_map: Vec<E820Entry>,
    pub keyboard_queue: Vec<u16>,
    /// Current BIOS video mode (BDA 0x449).
    ///
    /// This value also lives in guest RAM (the BIOS Data Area), but we capture
    /// it explicitly so BIOS snapshot payloads can restore it even when callers
    /// choose a RAM snapshot mode that does not include low memory.
    pub video_mode: u8,
    pub tty_output: Vec<u8>,
    pub rsdp_addr: Option<u64>,
    pub acpi_reclaimable: Option<(u64, u64)>,
    pub acpi_nvs: Option<(u64, u64)>,
    pub smbios_eps_addr: Option<u32>,
    pub last_int13_status: u8,
    pub vbe: VbeSnapshot,
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

        // v2 extension block (appended; older decoders will ignore trailing bytes).
        w.write_all(&[1])?;
        w.write_all(&[self.last_int13_status])?;
        self.vbe.encode(w)?;

        // v3 extension block: BIOS config + firmware table placement metadata.
        w.write_all(&[2])?;
        w.write_all(&[self.config.cpu_count])?;
        w.write_all(&[self.config.enable_acpi as u8])?;

        let placement = &self.config.acpi_placement;
        w.write_all(&placement.tables_base.to_le_bytes())?;
        w.write_all(&placement.nvs_base.to_le_bytes())?;
        w.write_all(&placement.nvs_size.to_le_bytes())?;
        w.write_all(&placement.rsdp_addr.to_le_bytes())?;
        w.write_all(&placement.alignment.to_le_bytes())?;

        for gsi in self.config.pirq_to_gsi {
            w.write_all(&gsi.to_le_bytes())?;
        }

        match self.acpi_reclaimable {
            Some((base, len)) => {
                w.write_all(&[1])?;
                w.write_all(&base.to_le_bytes())?;
                w.write_all(&len.to_le_bytes())?;
            }
            None => w.write_all(&[0])?,
        }

        match self.acpi_nvs {
            Some((base, len)) => {
                w.write_all(&[1])?;
                w.write_all(&base.to_le_bytes())?;
                w.write_all(&len.to_le_bytes())?;
            }
            None => w.write_all(&[0])?,
        }

        match self.smbios_eps_addr {
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

        let mut config = BiosConfig {
            memory_size_bytes,
            boot_drive,
            ..BiosConfig::default()
        };

        let mut last_int13_status = 0;
        let mut vbe = VbeSnapshot::default();
        let mut acpi_reclaimable = None;
        let mut acpi_nvs = None;
        let mut smbios_eps_addr = None;

        // Optional extension blocks (appended).
        loop {
            let mut ext_tag = [0u8; 1];
            match r.read_exact(&mut ext_tag) {
                Ok(()) => match ext_tag[0] {
                    1 => {
                        r.read_exact(&mut ext_tag)?;
                        last_int13_status = ext_tag[0];
                        vbe = VbeSnapshot::decode(r)?;
                    }
                    2 => {
                        r.read_exact(&mut ext_tag)?;
                        config.cpu_count = ext_tag[0];
                        r.read_exact(&mut ext_tag)?;
                        config.enable_acpi = ext_tag[0] != 0;

                        let mut buf8 = [0u8; 8];
                        r.read_exact(&mut buf8)?;
                        config.acpi_placement.tables_base = u64::from_le_bytes(buf8);
                        r.read_exact(&mut buf8)?;
                        config.acpi_placement.nvs_base = u64::from_le_bytes(buf8);
                        r.read_exact(&mut buf8)?;
                        config.acpi_placement.nvs_size = u64::from_le_bytes(buf8);
                        r.read_exact(&mut buf8)?;
                        config.acpi_placement.rsdp_addr = u64::from_le_bytes(buf8);
                        r.read_exact(&mut buf8)?;
                        config.acpi_placement.alignment = u64::from_le_bytes(buf8);

                        let mut buf4 = [0u8; 4];
                        for slot in config.pirq_to_gsi.iter_mut() {
                            r.read_exact(&mut buf4)?;
                            *slot = u32::from_le_bytes(buf4);
                        }

                        let mut present = [0u8; 1];
                        r.read_exact(&mut present)?;
                        if present[0] != 0 {
                            r.read_exact(&mut buf8)?;
                            let base = u64::from_le_bytes(buf8);
                            r.read_exact(&mut buf8)?;
                            let len = u64::from_le_bytes(buf8);
                            acpi_reclaimable = Some((base, len));
                        }

                        r.read_exact(&mut present)?;
                        if present[0] != 0 {
                            r.read_exact(&mut buf8)?;
                            let base = u64::from_le_bytes(buf8);
                            r.read_exact(&mut buf8)?;
                            let len = u64::from_le_bytes(buf8);
                            acpi_nvs = Some((base, len));
                        }

                        r.read_exact(&mut present)?;
                        if present[0] != 0 {
                            r.read_exact(&mut buf4)?;
                            smbios_eps_addr = Some(u32::from_le_bytes(buf4));
                        }
                    }
                    _ => {
                        // Unknown extension; ignore trailing bytes.
                        break;
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }

        Ok(Self {
            config,
            rtc,
            bda_time,
            e820_map,
            keyboard_queue,
            video_mode,
            tty_output,
            rsdp_addr,
            acpi_reclaimable,
            acpi_nvs,
            smbios_eps_addr,
            last_int13_status,
            vbe,
        })
    }
}

impl Bios {
    pub fn snapshot(&self, memory: &impl MemoryBus) -> BiosSnapshot {
        BiosSnapshot {
            config: self.config.clone(),
            rtc: self.rtc.snapshot(),
            bda_time: self.bda_time.snapshot(),
            e820_map: self.e820_map.clone(),
            keyboard_queue: self.keyboard_queue.iter().copied().collect(),
            // Video mode lives in the BIOS Data Area (BDA), which is part of guest RAM.
            // Capture it explicitly so snapshot payloads stay self-contained even if callers
            // choose a RAM snapshot mode that does not include low memory.
            video_mode: BiosDataArea::read_video_mode(memory),
            tty_output: self.tty_output.clone(),
            rsdp_addr: self.rsdp_addr,
            acpi_reclaimable: self.acpi_reclaimable,
            acpi_nvs: self.acpi_nvs,
            smbios_eps_addr: self.smbios_eps_addr,
            last_int13_status: self.last_int13_status,
            vbe: VbeSnapshot::from_device(&self.video.vbe),
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: BiosSnapshot, memory: &mut impl MemoryBus) {
        self.config = snapshot.config;
        self.rtc.restore_snapshot(snapshot.rtc);
        self.bda_time.restore_snapshot(snapshot.bda_time);
        self.e820_map = snapshot.e820_map;
        self.keyboard_queue = VecDeque::from(snapshot.keyboard_queue);
        BiosDataArea::write_video_mode(memory, snapshot.video_mode);
        self.tty_output = snapshot.tty_output;
        self.rsdp_addr = snapshot.rsdp_addr;
        self.acpi_reclaimable = snapshot.acpi_reclaimable;
        self.acpi_nvs = snapshot.acpi_nvs;
        self.smbios_eps_addr = snapshot.smbios_eps_addr;
        self.last_int13_status = snapshot.last_int13_status;
        snapshot.vbe.restore(&mut self.video.vbe);
    }
}

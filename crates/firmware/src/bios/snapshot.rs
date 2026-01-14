use std::io::{Read, Write};

use crate::bda::BiosDataArea;
use crate::memory::MemoryBus;
use crate::rtc::CmosRtcSnapshot;
use crate::video::vbe::VbeDevice;

use super::bda_time::BdaTimeSnapshot;
use super::{Bios, BiosBootDevice, BiosConfig, E820Entry, ElToritoBootInfo, ElToritoBootMediaType};

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

/// Snapshot representation of cached El Torito boot metadata.
///
/// This is populated when the BIOS booted via an El Torito boot catalog entry and is later used to
/// answer INT 13h AH=4Bh (El Torito disk emulation services).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElToritoBootInfoSnapshot {
    /// El Torito boot media type as an El Torito/BIOS encoding (e.g. `0x00` for "no emulation").
    ///
    /// This mirrors [`super::ElToritoBootMediaType`] but is stored as a raw `u8` for
    /// forward-compatibility with potential future media type additions.
    pub media_type: u8,
    /// Boot drive number passed to the boot image (DL).
    pub boot_drive: u8,
    /// BIOS controller index for the boot device (usually 0).
    pub controller_index: u8,
    /// Boot catalog sector (LBA) on the CD-ROM image (if known).
    pub boot_catalog_lba: Option<u32>,
    /// Boot image start sector (RBA/LBA) on the CD-ROM image (if known).
    pub boot_image_lba: Option<u32>,
    /// Real-mode segment the boot image was loaded to (if known).
    pub load_segment: Option<u16>,
    /// Number of 512-byte sectors loaded for the initial boot image (if known).
    pub sector_count: Option<u16>,
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
    pub el_torito_boot_info: Option<ElToritoBootInfoSnapshot>,
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

        // v4 extension block: BIOS config video overrides.
        w.write_all(&[3])?;
        match self.config.vbe_lfb_base {
            Some(base) => {
                w.write_all(&[1])?;
                w.write_all(&base.to_le_bytes())?;
            }
            None => w.write_all(&[0])?,
        }

        // v5 extension block: BIOS boot-selection policy config (boot order + CD policy).
        w.write_all(&[4])?;
        let boot_order_len: u8 = self
            .config
            .boot_order
            .len()
            .try_into()
            .unwrap_or(u8::MAX);
        w.write_all(&[boot_order_len])?;
        for dev in self.config.boot_order.iter().take(boot_order_len as usize) {
            w.write_all(&[*dev as u8])?;
        }
        w.write_all(&[self.config.cd_boot_drive])?;
        w.write_all(&[self.config.boot_from_cd_if_present as u8])?;

        // v6 extension block: SMBIOS UUID seed.
        w.write_all(&[5])?;
        w.write_all(&self.config.smbios_uuid_seed.to_le_bytes())?;

        // v7 extension block: El Torito boot metadata (INT 13h AH=4Bh services).
        w.write_all(&[6])?;
        match self.el_torito_boot_info {
            Some(info) => {
                w.write_all(&[1])?;
                w.write_all(&[info.media_type])?;
                w.write_all(&[info.boot_drive])?;
                w.write_all(&[info.controller_index])?;

                let mut mask: u8 = 0;
                if info.boot_catalog_lba.is_some() {
                    mask |= 1 << 0;
                }
                if info.boot_image_lba.is_some() {
                    mask |= 1 << 1;
                }
                if info.load_segment.is_some() {
                    mask |= 1 << 2;
                }
                if info.sector_count.is_some() {
                    mask |= 1 << 3;
                }
                w.write_all(&[mask])?;

                w.write_all(&info.boot_catalog_lba.unwrap_or(0).to_le_bytes())?;
                w.write_all(&info.boot_image_lba.unwrap_or(0).to_le_bytes())?;
                w.write_all(&info.load_segment.unwrap_or(0).to_le_bytes())?;
                w.write_all(&info.sector_count.unwrap_or(0).to_le_bytes())?;
            }
            None => w.write_all(&[0])?,
        }

        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> std::io::Result<Self> {
        const MAX_E820_ENTRIES: u32 = 1024;
        const MAX_KEYBOARD_QUEUE: u32 = 8192;
        // Maximum number of bytes we'll read for the legacy BIOS TTY buffer from a snapshot.
        //
        // Snapshots are primarily an internal format, but treating them as untrusted input is
        // cheap. If a snapshot claims an absurd length, fail fast instead of attempting to
        // allocate or stream-read gigabytes.
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
        let tty_len_raw = u32::from_le_bytes(buf4);
        if tty_len_raw > MAX_TTY_OUTPUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "BIOS TTY output buffer in snapshot is too large",
            ));
        }
        let tty_len = tty_len_raw as usize;
        // The runtime BIOS keeps a rolling `MAX_TTY_OUTPUT_BYTES` window; preserve only the tail
        // when decoding older snapshots that may have captured more output.
        let keep_len = tty_len.min(super::MAX_TTY_OUTPUT_BYTES);
        let mut tty_output = vec![0u8; keep_len];
        if tty_len > keep_len {
            let mut discard = tty_len - keep_len;
            let mut scratch = [0u8; 4096];
            while discard != 0 {
                let n = discard.min(scratch.len());
                r.read_exact(&mut scratch[..n])?;
                discard -= n;
            }
        }
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
        let mut el_torito_boot_info: Option<ElToritoBootInfoSnapshot> = None;
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
                    3 => {
                        let mut present = [0u8; 1];
                        r.read_exact(&mut present)?;
                        if present[0] != 0 {
                            let mut buf4 = [0u8; 4];
                            r.read_exact(&mut buf4)?;
                            config.vbe_lfb_base = Some(u32::from_le_bytes(buf4));
                        } else {
                            config.vbe_lfb_base = None;
                        }
                    }
                    4 => {
                        const MAX_BOOT_ORDER_LEN: u8 = 32;

                        let mut b = [0u8; 1];
                        r.read_exact(&mut b)?;
                        let encoded_len = b[0];

                        let mut boot_order =
                            Vec::with_capacity((encoded_len.min(MAX_BOOT_ORDER_LEN)) as usize);
                        for i in 0..encoded_len {
                            r.read_exact(&mut b)?;
                            let Some(dev) = (match b[0] {
                                0 => Some(BiosBootDevice::Hdd),
                                1 => Some(BiosBootDevice::Cdrom),
                                2 => Some(BiosBootDevice::Floppy),
                                _ => None,
                            }) else {
                                // Skip unknown device codes for forward compatibility.
                                continue;
                            };
                            if i < MAX_BOOT_ORDER_LEN {
                                boot_order.push(dev);
                            }
                        }
                        config.boot_order = boot_order;

                        r.read_exact(&mut b)?;
                        config.cd_boot_drive = b[0];
                        r.read_exact(&mut b)?;
                        config.boot_from_cd_if_present = b[0] != 0;
                    }
                    5 => {
                        let mut buf8 = [0u8; 8];
                        r.read_exact(&mut buf8)?;
                        config.smbios_uuid_seed = u64::from_le_bytes(buf8);
                    }
                    6 => {
                        let mut present = [0u8; 1];
                        r.read_exact(&mut present)?;
                        if present[0] == 0 {
                            el_torito_boot_info = None;
                            continue;
                        }

                        let mut fields = [0u8; 4];
                        r.read_exact(&mut fields)?;
                        let media_type = fields[0];
                        let boot_drive = fields[1];
                        let controller_index = fields[2];
                        let mask = fields[3];

                        let mut buf4 = [0u8; 4];
                        r.read_exact(&mut buf4)?;
                        let boot_catalog_raw = u32::from_le_bytes(buf4);
                        r.read_exact(&mut buf4)?;
                        let boot_image_raw = u32::from_le_bytes(buf4);

                        let mut buf2 = [0u8; 2];
                        r.read_exact(&mut buf2)?;
                        let load_segment_raw = u16::from_le_bytes(buf2);
                        r.read_exact(&mut buf2)?;
                        let sector_count_raw = u16::from_le_bytes(buf2);

                        el_torito_boot_info = Some(ElToritoBootInfoSnapshot {
                            media_type,
                            boot_drive,
                            controller_index,
                            boot_catalog_lba: (mask & (1 << 0) != 0).then_some(boot_catalog_raw),
                            boot_image_lba: (mask & (1 << 1) != 0).then_some(boot_image_raw),
                            load_segment: (mask & (1 << 2) != 0).then_some(load_segment_raw),
                            sector_count: (mask & (1 << 3) != 0).then_some(sector_count_raw),
                        });
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
            el_torito_boot_info,
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
            // Video mode lives in the BIOS Data Area (BDA), which is part of guest RAM. We also
            // cache the value in `Bios::video_mode` so BIOS snapshots can be created without a
            // memory bus (important for snapshot APIs that only expose `&self`).
            video_mode: self.video_mode,
            tty_output: self.tty_output().to_vec(),
            rsdp_addr: self.rsdp_addr,
            acpi_reclaimable: self.acpi_reclaimable,
            acpi_nvs: self.acpi_nvs,
            smbios_eps_addr: self.smbios_eps_addr,
            last_int13_status: self.last_int13_status,
            vbe: VbeSnapshot::from_device(&self.video.vbe),
            el_torito_boot_info: self.el_torito_boot_info.map(|info| ElToritoBootInfoSnapshot {
                media_type: info.media_type as u8,
                boot_drive: info.boot_drive,
                controller_index: info.controller_index,
                boot_catalog_lba: info.boot_catalog_lba,
                boot_image_lba: info.boot_image_lba,
                load_segment: info.load_segment,
                sector_count: info.sector_count,
            }),
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: BiosSnapshot, memory: &mut impl MemoryBus) {
        self.config = snapshot.config;
        self.rtc.restore_snapshot(snapshot.rtc);
        self.bda_time.restore_snapshot(snapshot.bda_time);
        self.e820_map = snapshot.e820_map;
        self.keyboard_queue.clear();
        for key in snapshot.keyboard_queue {
            self.push_key(key);
        }
        BiosDataArea::write_video_mode(memory, snapshot.video_mode);
        self.video_mode = snapshot.video_mode;
        self.clear_tty_output();
        self.push_tty_bytes(&snapshot.tty_output);
        self.rsdp_addr = snapshot.rsdp_addr;
        self.acpi_reclaimable = snapshot.acpi_reclaimable;
        self.acpi_nvs = snapshot.acpi_nvs;
        self.smbios_eps_addr = snapshot.smbios_eps_addr;
        self.last_int13_status = snapshot.last_int13_status;
        self.el_torito_boot_info = snapshot.el_torito_boot_info.and_then(|info| {
            let media_type = match info.media_type {
                0x00 => ElToritoBootMediaType::NoEmulation,
                0x01 => ElToritoBootMediaType::Floppy1200KiB,
                0x02 => ElToritoBootMediaType::Floppy1440KiB,
                0x03 => ElToritoBootMediaType::Floppy2880KiB,
                0x04 => ElToritoBootMediaType::HardDisk,
                _ => return None,
            };
            Some(ElToritoBootInfo {
                media_type,
                boot_drive: info.boot_drive,
                controller_index: info.controller_index,
                boot_catalog_lba: info.boot_catalog_lba,
                boot_image_lba: info.boot_image_lba,
                load_segment: info.load_segment,
                sector_count: info.sector_count,
            })
        });
        snapshot.vbe.restore(&mut self.video.vbe);
        if let Some(base) = self.config.vbe_lfb_base {
            self.video.vbe.lfb_base = base;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::bios::MAX_TTY_OUTPUT_BYTES;
    use crate::bios::{ElToritoBootInfo, ElToritoBootMediaType};

    #[test]
    fn bios_snapshot_decode_truncates_tty_output_to_a_rolling_tail_window() {
        let bios = Bios::new(BiosConfig::default());
        let mut snapshot = bios.snapshot();

        let total = MAX_TTY_OUTPUT_BYTES + 1024;
        snapshot.tty_output = (0..total).map(|i| (i % 256) as u8).collect();

        let mut buf = Vec::new();
        snapshot.encode(&mut buf).unwrap();

        let decoded = BiosSnapshot::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.tty_output.len(), MAX_TTY_OUTPUT_BYTES);
        assert_eq!(decoded.tty_output[0], ((total - MAX_TTY_OUTPUT_BYTES) % 256) as u8);
        assert_eq!(decoded.tty_output[decoded.tty_output.len() - 1], ((total - 1) % 256) as u8);
    }

    #[test]
    fn bios_snapshot_encode_decode_preserves_el_torito_boot_info() {
        let mut bios = Bios::new(BiosConfig::default());
        bios.el_torito_boot_info = Some(ElToritoBootInfo {
            media_type: ElToritoBootMediaType::NoEmulation,
            boot_drive: 0xE0,
            controller_index: 0,
            boot_catalog_lba: Some(0x1111_2222),
            boot_image_lba: Some(0x3333_4444),
            load_segment: Some(0x07C0),
            sector_count: Some(4),
        });

        let snapshot = bios.snapshot();
        assert_eq!(
            snapshot.el_torito_boot_info,
            Some(ElToritoBootInfoSnapshot {
                media_type: 0x00,
                boot_drive: 0xE0,
                controller_index: 0,
                boot_catalog_lba: Some(0x1111_2222),
                boot_image_lba: Some(0x3333_4444),
                load_segment: Some(0x07C0),
                sector_count: Some(4),
            })
        );

        let mut buf = Vec::new();
        snapshot.encode(&mut buf).unwrap();

        let decoded = BiosSnapshot::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.el_torito_boot_info, snapshot.el_torito_boot_info);

        let mut bios2 = Bios::new(BiosConfig::default());
        let mut mem = crate::memory::VecMemory::new(2 * 1024 * 1024);
        bios2.restore_snapshot(decoded, &mut mem);
        assert_eq!(bios2.el_torito_boot_info, bios.el_torito_boot_info);
    }

    #[test]
    fn restore_snapshot_resets_el_torito_boot_info_when_absent() {
        let mut bios = Bios::new(BiosConfig::default());
        bios.el_torito_boot_info = Some(ElToritoBootInfo {
            media_type: ElToritoBootMediaType::NoEmulation,
            boot_drive: 0xE0,
            controller_index: 0,
            boot_catalog_lba: Some(1),
            boot_image_lba: Some(2),
            load_segment: Some(0x07C0),
            sector_count: Some(4),
        });

        let snapshot_none = Bios::new(BiosConfig::default()).snapshot();
        let mut mem = crate::memory::VecMemory::new(2 * 1024 * 1024);
        bios.restore_snapshot(snapshot_none, &mut mem);
        assert_eq!(bios.el_torito_boot_info, None);
    }

    const PRE_BOOT_ORDER_EXT_SNAPSHOT: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x80, 0xEA, 0x07, 0x01, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x2A, 0x00, 0x00, 0x00,
        0x00, 0x2A, 0x00, 0x00, 0x2A, 0x2A, 0x00, 0x00, 0x00, 0x00, 0x2A, 0x00, 0x2A, 0x00,
        0x2A, 0x00, 0x00, 0x15, 0x2A, 0x00, 0x2A, 0x2A, 0x2A, 0x00, 0x15, 0x15, 0x15, 0x00,
        0x3F, 0x15, 0x15, 0x00, 0x15, 0x3F, 0x15, 0x00, 0x3F, 0x3F, 0x15, 0x00, 0x15, 0x15,
        0x3F, 0x00, 0x3F, 0x15, 0x3F, 0x00, 0x15, 0x3F, 0x3F, 0x00, 0x3F, 0x3F, 0x3F, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x19, 0x00, 0x00, 0x00, 0x26, 0x00,
        0x00, 0x00, 0x33, 0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x00,
        0x0C, 0x0C, 0x00, 0x00, 0x19, 0x0C, 0x00, 0x00, 0x26, 0x0C, 0x00, 0x00, 0x33, 0x0C,
        0x00, 0x00, 0x3F, 0x0C, 0x00, 0x00, 0x00, 0x19, 0x00, 0x00, 0x0C, 0x19, 0x00, 0x00,
        0x19, 0x19, 0x00, 0x00, 0x26, 0x19, 0x00, 0x00, 0x33, 0x19, 0x00, 0x00, 0x3F, 0x19,
        0x00, 0x00, 0x00, 0x26, 0x00, 0x00, 0x0C, 0x26, 0x00, 0x00, 0x19, 0x26, 0x00, 0x00,
        0x26, 0x26, 0x00, 0x00, 0x33, 0x26, 0x00, 0x00, 0x3F, 0x26, 0x00, 0x00, 0x00, 0x33,
        0x00, 0x00, 0x0C, 0x33, 0x00, 0x00, 0x19, 0x33, 0x00, 0x00, 0x26, 0x33, 0x00, 0x00,
        0x33, 0x33, 0x00, 0x00, 0x3F, 0x33, 0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x0C, 0x3F,
        0x00, 0x00, 0x19, 0x3F, 0x00, 0x00, 0x26, 0x3F, 0x00, 0x00, 0x33, 0x3F, 0x00, 0x00,
        0x3F, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x0C, 0x00, 0x0C, 0x00, 0x19, 0x00,
        0x0C, 0x00, 0x26, 0x00, 0x0C, 0x00, 0x33, 0x00, 0x0C, 0x00, 0x3F, 0x00, 0x0C, 0x00,
        0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x0C, 0x00, 0x19, 0x0C, 0x0C, 0x00, 0x26, 0x0C,
        0x0C, 0x00, 0x33, 0x0C, 0x0C, 0x00, 0x3F, 0x0C, 0x0C, 0x00, 0x00, 0x19, 0x0C, 0x00,
        0x0C, 0x19, 0x0C, 0x00, 0x19, 0x19, 0x0C, 0x00, 0x26, 0x19, 0x0C, 0x00, 0x33, 0x19,
        0x0C, 0x00, 0x3F, 0x19, 0x0C, 0x00, 0x00, 0x26, 0x0C, 0x00, 0x0C, 0x26, 0x0C, 0x00,
        0x19, 0x26, 0x0C, 0x00, 0x26, 0x26, 0x0C, 0x00, 0x33, 0x26, 0x0C, 0x00, 0x3F, 0x26,
        0x0C, 0x00, 0x00, 0x33, 0x0C, 0x00, 0x0C, 0x33, 0x0C, 0x00, 0x19, 0x33, 0x0C, 0x00,
        0x26, 0x33, 0x0C, 0x00, 0x33, 0x33, 0x0C, 0x00, 0x3F, 0x33, 0x0C, 0x00, 0x00, 0x3F,
        0x0C, 0x00, 0x0C, 0x3F, 0x0C, 0x00, 0x19, 0x3F, 0x0C, 0x00, 0x26, 0x3F, 0x0C, 0x00,
        0x33, 0x3F, 0x0C, 0x00, 0x3F, 0x3F, 0x0C, 0x00, 0x00, 0x00, 0x19, 0x00, 0x0C, 0x00,
        0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x26, 0x00, 0x19, 0x00, 0x33, 0x00, 0x19, 0x00,
        0x3F, 0x00, 0x19, 0x00, 0x00, 0x0C, 0x19, 0x00, 0x0C, 0x0C, 0x19, 0x00, 0x19, 0x0C,
        0x19, 0x00, 0x26, 0x0C, 0x19, 0x00, 0x33, 0x0C, 0x19, 0x00, 0x3F, 0x0C, 0x19, 0x00,
        0x00, 0x19, 0x19, 0x00, 0x0C, 0x19, 0x19, 0x00, 0x19, 0x19, 0x19, 0x00, 0x26, 0x19,
        0x19, 0x00, 0x33, 0x19, 0x19, 0x00, 0x3F, 0x19, 0x19, 0x00, 0x00, 0x26, 0x19, 0x00,
        0x0C, 0x26, 0x19, 0x00, 0x19, 0x26, 0x19, 0x00, 0x26, 0x26, 0x19, 0x00, 0x33, 0x26,
        0x19, 0x00, 0x3F, 0x26, 0x19, 0x00, 0x00, 0x33, 0x19, 0x00, 0x0C, 0x33, 0x19, 0x00,
        0x19, 0x33, 0x19, 0x00, 0x26, 0x33, 0x19, 0x00, 0x33, 0x33, 0x19, 0x00, 0x3F, 0x33,
        0x19, 0x00, 0x00, 0x3F, 0x19, 0x00, 0x0C, 0x3F, 0x19, 0x00, 0x19, 0x3F, 0x19, 0x00,
        0x26, 0x3F, 0x19, 0x00, 0x33, 0x3F, 0x19, 0x00, 0x3F, 0x3F, 0x19, 0x00, 0x00, 0x00,
        0x26, 0x00, 0x0C, 0x00, 0x26, 0x00, 0x19, 0x00, 0x26, 0x00, 0x26, 0x00, 0x26, 0x00,
        0x33, 0x00, 0x26, 0x00, 0x3F, 0x00, 0x26, 0x00, 0x00, 0x0C, 0x26, 0x00, 0x0C, 0x0C,
        0x26, 0x00, 0x19, 0x0C, 0x26, 0x00, 0x26, 0x0C, 0x26, 0x00, 0x33, 0x0C, 0x26, 0x00,
        0x3F, 0x0C, 0x26, 0x00, 0x00, 0x19, 0x26, 0x00, 0x0C, 0x19, 0x26, 0x00, 0x19, 0x19,
        0x26, 0x00, 0x26, 0x19, 0x26, 0x00, 0x33, 0x19, 0x26, 0x00, 0x3F, 0x19, 0x26, 0x00,
        0x00, 0x26, 0x26, 0x00, 0x0C, 0x26, 0x26, 0x00, 0x19, 0x26, 0x26, 0x00, 0x26, 0x26,
        0x26, 0x00, 0x33, 0x26, 0x26, 0x00, 0x3F, 0x26, 0x26, 0x00, 0x00, 0x33, 0x26, 0x00,
        0x0C, 0x33, 0x26, 0x00, 0x19, 0x33, 0x26, 0x00, 0x26, 0x33, 0x26, 0x00, 0x33, 0x33,
        0x26, 0x00, 0x3F, 0x33, 0x26, 0x00, 0x00, 0x3F, 0x26, 0x00, 0x0C, 0x3F, 0x26, 0x00,
        0x19, 0x3F, 0x26, 0x00, 0x26, 0x3F, 0x26, 0x00, 0x33, 0x3F, 0x26, 0x00, 0x3F, 0x3F,
        0x26, 0x00, 0x00, 0x00, 0x33, 0x00, 0x0C, 0x00, 0x33, 0x00, 0x19, 0x00, 0x33, 0x00,
        0x26, 0x00, 0x33, 0x00, 0x33, 0x00, 0x33, 0x00, 0x3F, 0x00, 0x33, 0x00, 0x00, 0x0C,
        0x33, 0x00, 0x0C, 0x0C, 0x33, 0x00, 0x19, 0x0C, 0x33, 0x00, 0x26, 0x0C, 0x33, 0x00,
        0x33, 0x0C, 0x33, 0x00, 0x3F, 0x0C, 0x33, 0x00, 0x00, 0x19, 0x33, 0x00, 0x0C, 0x19,
        0x33, 0x00, 0x19, 0x19, 0x33, 0x00, 0x26, 0x19, 0x33, 0x00, 0x33, 0x19, 0x33, 0x00,
        0x3F, 0x19, 0x33, 0x00, 0x00, 0x26, 0x33, 0x00, 0x0C, 0x26, 0x33, 0x00, 0x19, 0x26,
        0x33, 0x00, 0x26, 0x26, 0x33, 0x00, 0x33, 0x26, 0x33, 0x00, 0x3F, 0x26, 0x33, 0x00,
        0x00, 0x33, 0x33, 0x00, 0x0C, 0x33, 0x33, 0x00, 0x19, 0x33, 0x33, 0x00, 0x26, 0x33,
        0x33, 0x00, 0x33, 0x33, 0x33, 0x00, 0x3F, 0x33, 0x33, 0x00, 0x00, 0x3F, 0x33, 0x00,
        0x0C, 0x3F, 0x33, 0x00, 0x19, 0x3F, 0x33, 0x00, 0x26, 0x3F, 0x33, 0x00, 0x33, 0x3F,
        0x33, 0x00, 0x3F, 0x3F, 0x33, 0x00, 0x00, 0x00, 0x3F, 0x00, 0x0C, 0x00, 0x3F, 0x00,
        0x19, 0x00, 0x3F, 0x00, 0x26, 0x00, 0x3F, 0x00, 0x33, 0x00, 0x3F, 0x00, 0x3F, 0x00,
        0x3F, 0x00, 0x00, 0x0C, 0x3F, 0x00, 0x0C, 0x0C, 0x3F, 0x00, 0x19, 0x0C, 0x3F, 0x00,
        0x26, 0x0C, 0x3F, 0x00, 0x33, 0x0C, 0x3F, 0x00, 0x3F, 0x0C, 0x3F, 0x00, 0x00, 0x19,
        0x3F, 0x00, 0x0C, 0x19, 0x3F, 0x00, 0x19, 0x19, 0x3F, 0x00, 0x26, 0x19, 0x3F, 0x00,
        0x33, 0x19, 0x3F, 0x00, 0x3F, 0x19, 0x3F, 0x00, 0x00, 0x26, 0x3F, 0x00, 0x0C, 0x26,
        0x3F, 0x00, 0x19, 0x26, 0x3F, 0x00, 0x26, 0x26, 0x3F, 0x00, 0x33, 0x26, 0x3F, 0x00,
        0x3F, 0x26, 0x3F, 0x00, 0x00, 0x33, 0x3F, 0x00, 0x0C, 0x33, 0x3F, 0x00, 0x19, 0x33,
        0x3F, 0x00, 0x26, 0x33, 0x3F, 0x00, 0x33, 0x33, 0x3F, 0x00, 0x3F, 0x33, 0x3F, 0x00,
        0x00, 0x3F, 0x3F, 0x00, 0x0C, 0x3F, 0x3F, 0x00, 0x19, 0x3F, 0x3F, 0x00, 0x26, 0x3F,
        0x3F, 0x00, 0x33, 0x3F, 0x3F, 0x00, 0x3F, 0x3F, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x02, 0x02, 0x02, 0x00, 0x05, 0x05, 0x05, 0x00, 0x08, 0x08, 0x08, 0x00, 0x0B, 0x0B, 0x0B,
        0x00, 0x0D, 0x0D, 0x0D, 0x00, 0x10, 0x10, 0x10, 0x00, 0x13, 0x13, 0x13, 0x00, 0x16, 0x16,
        0x16, 0x00, 0x18, 0x18, 0x18, 0x00, 0x1B, 0x1B, 0x1B, 0x00, 0x1E, 0x1E, 0x1E, 0x00, 0x21,
        0x21, 0x21, 0x00, 0x24, 0x24, 0x24, 0x00, 0x26, 0x26, 0x26, 0x00, 0x29, 0x29, 0x29, 0x00,
        0x2C, 0x2C, 0x2C, 0x00, 0x2F, 0x2F, 0x2F, 0x00, 0x31, 0x31, 0x31, 0x00, 0x34, 0x34, 0x34,
        0x00, 0x37, 0x37, 0x37, 0x00, 0x3A, 0x3A, 0x3A, 0x00, 0x3C, 0x3C, 0x3C, 0x00, 0x3F, 0x3F,
        0x3F, 0x00, 0x02, 0x01, 0x01, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xF1, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0A, 0x00, 0x00, 0x00, 0x0B, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x0D, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
    ];

    #[test]
    fn bios_snapshot_encode_decode_preserves_vbe_lfb_base_config_override() {
        let base = 0xDEAD_BEEFu32;
        let bios = Bios::new(BiosConfig {
            vbe_lfb_base: Some(base),
            ..BiosConfig::default()
        });
        let snapshot = bios.snapshot();

        let mut buf = Vec::new();
        snapshot.encode(&mut buf).unwrap();

        let decoded = BiosSnapshot::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.config.vbe_lfb_base, Some(base));
    }

    #[test]
    fn restore_snapshot_applies_vbe_lfb_base_config_override() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut snapshot = bios.snapshot();
        snapshot.vbe.lfb_base = 0x1234_0000;
        snapshot.config.vbe_lfb_base = Some(0x5678_0000);

        let mut mem = crate::memory::VecMemory::new(2 * 1024 * 1024);
        bios.restore_snapshot(snapshot, &mut mem);

        assert_eq!(bios.video.vbe.lfb_base, 0x5678_0000);
    }

    #[test]
    fn bios_snapshot_encode_decode_preserves_boot_order_and_cd_policy_config() {
        let cfg = BiosConfig {
            boot_order: vec![BiosBootDevice::Cdrom, BiosBootDevice::Hdd],
            cd_boot_drive: 0xE1,
            boot_from_cd_if_present: true,
            ..BiosConfig::default()
        };
        let bios = Bios::new(cfg.clone());
        let snapshot = bios.snapshot();

        let mut buf = Vec::new();
        snapshot.encode(&mut buf).unwrap();

        let decoded = BiosSnapshot::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.config.boot_order, cfg.boot_order);
        assert_eq!(decoded.config.cd_boot_drive, cfg.cd_boot_drive);
        assert_eq!(
            decoded.config.boot_from_cd_if_present,
            cfg.boot_from_cd_if_present
        );

        let mut bios2 = Bios::new(BiosConfig::default());
        let mut mem = crate::memory::VecMemory::new(2 * 1024 * 1024);
        bios2.restore_snapshot(decoded, &mut mem);
        assert_eq!(
            bios2.config().boot_order,
            vec![BiosBootDevice::Cdrom, BiosBootDevice::Hdd]
        );
        assert_eq!(bios2.config().cd_boot_drive, 0xE1);
        assert!(bios2.config().boot_from_cd_if_present);
    }

    #[test]
    fn bios_snapshot_decode_without_boot_order_extension_applies_defaults() {
        let decoded = BiosSnapshot::decode(&mut Cursor::new(PRE_BOOT_ORDER_EXT_SNAPSHOT)).unwrap();
        assert_eq!(decoded.config.boot_order, vec![BiosBootDevice::Hdd]);
        assert_eq!(decoded.config.cd_boot_drive, 0xE0);
        assert!(!decoded.config.boot_from_cd_if_present);
    }
}

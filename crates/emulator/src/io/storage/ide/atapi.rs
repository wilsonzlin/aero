use crate::io::storage::disk::{DiskError, DiskResult};

/// Read-only ISO9660 (or raw CD) backing store.
///
/// The IDE/ATAPI layer treats the image as a sequence of 2048-byte sectors.
pub trait IsoBackend: Send {
    fn sector_count(&self) -> u32;
    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> DiskResult<()>;
}

const SENSE_NO_SENSE: u8 = 0x00;
const SENSE_NOT_READY: u8 = 0x02;
const SENSE_ILLEGAL_REQUEST: u8 = 0x05;
const SENSE_UNIT_ATTENTION: u8 = 0x06;

const ASC_MEDIUM_NOT_PRESENT: u8 = 0x3A;
const ASC_MEDIUM_CHANGED: u8 = 0x28;

#[derive(Debug, Clone, Copy)]
struct Sense {
    key: u8,
    asc: u8,
    ascq: u8,
}

impl Sense {
    fn ok() -> Self {
        Self {
            key: SENSE_NO_SENSE,
            asc: 0,
            ascq: 0,
        }
    }
}

pub struct AtapiCdrom {
    backend: Option<Box<dyn IsoBackend>>,
    tray_open: bool,
    media_changed: bool,
    sense: Sense,
    supports_dma: bool,
}

impl AtapiCdrom {
    pub fn new(backend: Option<Box<dyn IsoBackend>>) -> Self {
        let media_changed = backend.is_some();
        Self {
            backend,
            tray_open: false,
            media_changed,
            sense: Sense::ok(),
            supports_dma: true,
        }
    }

    pub fn supports_dma(&self) -> bool {
        self.supports_dma
    }

    pub fn insert_media(&mut self, backend: Box<dyn IsoBackend>) {
        self.backend = Some(backend);
        self.tray_open = false;
        self.media_changed = true;
    }

    pub fn eject_media(&mut self) {
        self.backend = None;
        self.tray_open = true;
        self.media_changed = true;
    }

    pub fn set_sense(&mut self, key: u8, asc: u8, ascq: u8) {
        self.sense = Sense { key, asc, ascq };
    }

    pub fn identify_packet_data(&self) -> Vec<u8> {
        let mut words = [0u16; 256];

        // General configuration: ATAPI device, CD-ROM type, removable.
        words[0] = 0x8580;

        // Serial / firmware / model strings.
        write_ata_string(&mut words[10..20], "AEROCDROM000000000001", 20);
        write_ata_string(&mut words[23..27], "0.1", 8);
        write_ata_string(&mut words[27..47], "Aero ATAPI CD-ROM", 40);

        // Capabilities: DMA.
        words[49] = 1 << 8;

        // Packet size 12 bytes.
        words[0] |= 1; // 12-byte packets (bit0=0 for 12-byte in some docs; harmless).

        let mut out = vec![0u8; 512];
        for (i, w) in words.iter().enumerate() {
            out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    fn check_ready(&mut self) -> Result<(), PacketResult> {
        if self.media_changed {
            self.media_changed = false;
            self.set_sense(SENSE_UNIT_ATTENTION, ASC_MEDIUM_CHANGED, 0);
            return Err(PacketResult::Error {
                sense_key: SENSE_UNIT_ATTENTION,
                asc: ASC_MEDIUM_CHANGED,
                ascq: 0,
            });
        }
        if self.tray_open || self.backend.is_none() {
            self.set_sense(SENSE_NOT_READY, ASC_MEDIUM_NOT_PRESENT, 0);
            return Err(PacketResult::Error {
                sense_key: SENSE_NOT_READY,
                asc: ASC_MEDIUM_NOT_PRESENT,
                ascq: 0,
            });
        }
        Ok(())
    }

    pub fn handle_packet(&mut self, packet: &[u8; 12], dma_requested: bool) -> PacketResult {
        let opcode = packet[0];
        match opcode {
            0x12 => {
                // INQUIRY
                let alloc_len = packet[4] as usize;
                let data = self.inquiry_data();
                PacketResult::DataIn(data[..alloc_len.min(data.len())].to_vec())
            }
            0x00 => {
                // TEST UNIT READY
                match self.check_ready() {
                    Ok(()) => {
                        self.set_sense(SENSE_NO_SENSE, 0, 0);
                        PacketResult::NoDataSuccess
                    }
                    Err(e) => e,
                }
            }
            0x25 => {
                // READ CAPACITY(10)
                if let Err(e) = self.check_ready() {
                    return e;
                }
                let data = match self.read_capacity_10() {
                    Ok(d) => d,
                    Err(_) => {
                        self.set_sense(SENSE_NOT_READY, ASC_MEDIUM_NOT_PRESENT, 0);
                        return PacketResult::Error {
                            sense_key: SENSE_NOT_READY,
                            asc: ASC_MEDIUM_NOT_PRESENT,
                            ascq: 0,
                        };
                    }
                };
                PacketResult::DataIn(data)
            }
            0x28 => {
                // READ(10)
                if let Err(e) = self.check_ready() {
                    return e;
                }
                let lba = u32::from_be_bytes([packet[2], packet[3], packet[4], packet[5]]);
                let blocks = u16::from_be_bytes([packet[7], packet[8]]) as u32;
                self.read_blocks(lba, blocks, dma_requested)
            }
            0xA8 => {
                // READ(12)
                if let Err(e) = self.check_ready() {
                    return e;
                }
                let lba = u32::from_be_bytes([packet[2], packet[3], packet[4], packet[5]]);
                let blocks = u32::from_be_bytes([packet[6], packet[7], packet[8], packet[9]]);
                self.read_blocks(lba, blocks, dma_requested)
            }
            0x03 => {
                // REQUEST SENSE
                let alloc_len = packet[4] as usize;
                let sense = self.request_sense();
                PacketResult::DataIn(sense[..alloc_len.min(sense.len())].to_vec())
            }
            0x43 => {
                // READ TOC
                if let Err(e) = self.check_ready() {
                    return e;
                }
                let alloc_len = u16::from_be_bytes([packet[7], packet[8]]) as usize;
                let data = self.read_toc();
                PacketResult::DataIn(data[..alloc_len.min(data.len())].to_vec())
            }
            0x1B => {
                // START STOP UNIT
                let loej = (packet[4] & 0x02) != 0;
                let start = (packet[4] & 0x01) != 0;
                if loej && !start {
                    self.eject_media();
                } else if loej && start {
                    self.tray_open = false;
                }
                PacketResult::NoDataSuccess
            }
            _ => {
                self.set_sense(SENSE_ILLEGAL_REQUEST, 0x20, 0);
                PacketResult::Error {
                    sense_key: SENSE_ILLEGAL_REQUEST,
                    asc: 0x20,
                    ascq: 0,
                }
            }
        }
    }

    fn inquiry_data(&self) -> Vec<u8> {
        let mut data = vec![0u8; 36];
        data[0] = 0x05; // CD/DVD device
        data[1] = 0x80; // removable
        data[2] = 0x05; // SPC-3
        data[3] = 0x02; // response data format
        data[4] = (data.len() - 5) as u8;
        write_scsi_ascii(&mut data[8..16], b"AERO");
        write_scsi_ascii(&mut data[16..32], b"ATAPI CD-ROM");
        write_scsi_ascii(&mut data[32..36], b"0.1");
        data
    }

    fn request_sense(&self) -> Vec<u8> {
        let mut data = vec![0u8; 18];
        data[0] = 0x70;
        data[2] = self.sense.key & 0x0F;
        data[7] = 10;
        data[12] = self.sense.asc;
        data[13] = self.sense.ascq;
        data
    }

    fn read_capacity_10(&mut self) -> DiskResult<Vec<u8>> {
        let Some(backend) = self.backend.as_ref() else {
            return Err(DiskError::OutOfBounds);
        };
        let sectors = backend.sector_count();
        let last_lba = sectors.saturating_sub(1);
        let mut out = vec![0u8; 8];
        out[..4].copy_from_slice(&last_lba.to_be_bytes());
        out[4..].copy_from_slice(&2048u32.to_be_bytes());
        Ok(out)
    }

    fn read_blocks(&mut self, lba: u32, blocks: u32, dma_requested: bool) -> PacketResult {
        let len = blocks as usize * 2048;
        let mut buf = vec![0u8; len];
        let res = if let Some(backend) = self.backend.as_mut() {
            backend.read_sectors(lba, &mut buf)
        } else {
            Err(DiskError::OutOfBounds)
        };
        match res {
            Ok(()) => {
                self.set_sense(SENSE_NO_SENSE, 0, 0);
                if dma_requested {
                    PacketResult::DmaIn(buf)
                } else {
                    PacketResult::DataIn(buf)
                }
            }
            Err(_) => {
                self.set_sense(SENSE_ILLEGAL_REQUEST, 0x21, 0);
                PacketResult::Error {
                    sense_key: SENSE_ILLEGAL_REQUEST,
                    asc: 0x21,
                    ascq: 0,
                }
            }
        }
    }

    fn read_toc(&mut self) -> Vec<u8> {
        let sectors = self.backend.as_ref().map(|b| b.sector_count()).unwrap_or(0);
        let lead_out_lba = sectors;

        // Header (4 bytes) + 2 descriptors (track 1 + lead-out) = 4 + 16 = 20 bytes.
        let mut out = vec![0u8; 20];
        let data_len = (out.len() - 2) as u16;
        out[0..2].copy_from_slice(&data_len.to_be_bytes());
        out[2] = 1; // first track
        out[3] = 1; // last track

        // Track 1 descriptor.
        out[5] = 0x14; // ADR=1, control=4 (data track)
        out[6] = 0x01; // track number
        out[8..12].copy_from_slice(&0u32.to_be_bytes());

        // Lead-out descriptor (track number 0xAA).
        out[13] = 0x14;
        out[14] = 0xAA;
        out[16..20].copy_from_slice(&lead_out_lba.to_be_bytes());

        out
    }
}

#[derive(Debug)]
pub enum PacketResult {
    NoDataSuccess,
    DataIn(Vec<u8>),
    DmaIn(Vec<u8>),
    Error { sense_key: u8, asc: u8, ascq: u8 },
}

fn write_scsi_ascii(dst: &mut [u8], src: &[u8]) {
    dst.fill(b' ');
    let copy_len = src.len().min(dst.len());
    dst[..copy_len].copy_from_slice(&src[..copy_len]);
}

fn write_ata_string(dst_words: &mut [u16], src: &str, byte_len: usize) {
    let mut bytes = vec![b' '; byte_len];
    let src_bytes = src.as_bytes();
    let copy_len = src_bytes.len().min(byte_len);
    bytes[..copy_len].copy_from_slice(&src_bytes[..copy_len]);

    for (i, word) in dst_words.iter_mut().enumerate() {
        let idx = i * 2;
        if idx + 1 >= bytes.len() {
            break;
        }
        *word = u16::from_be_bytes([bytes[idx], bytes[idx + 1]]);
    }
}

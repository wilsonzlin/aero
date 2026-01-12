use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

// Snapshots may be loaded from untrusted sources (e.g. downloaded files). Keep decoding bounded so
// corrupted snapshots cannot force pathological allocations.
const MAX_OTHER_REGS: usize = 65_536;
const MAX_RX_PENDING_FRAMES: usize = 256;
const MAX_TX_OUT_FRAMES: usize = 256;
const MIN_L2_FRAME_LEN: usize = 14;
const MAX_L2_FRAME_LEN: usize = 1522;
const MAX_TX_PARTIAL_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct E1000TxContextState {
    pub ipcss: u32,
    pub ipcso: u32,
    pub ipcse: u32,
    pub tucss: u32,
    pub tucso: u32,
    pub tucse: u32,
    pub mss: u32,
    pub hdr_len: u32,
}

impl Default for E1000TxContextState {
    fn default() -> Self {
        Self {
            ipcss: 0,
            ipcso: 0,
            ipcse: 0,
            tucss: 0,
            tucso: 0,
            tucse: 0,
            mss: 0,
            hdr_len: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum E1000TxPacketState {
    None,
    Legacy { cmd: u8, css: u32, cso: u32 },
    Advanced { cmd: u8, popts: u8 },
}

impl Default for E1000TxPacketState {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E1000DeviceState {
    // PCI config space.
    pub pci_regs: [u8; 256],
    pub pci_bar0: u32,
    pub pci_bar0_probe: bool,
    pub pci_bar1: u32,
    pub pci_bar1_probe: bool,

    // Registers (subset).
    pub ctrl: u32,
    pub status: u32,
    pub eecd: u32,
    pub eerd: u32,
    pub ctrl_ext: u32,
    pub mdic: u32,
    pub io_reg: u32,

    pub icr: u32,
    pub ims: u32,

    pub rctl: u32,
    pub tctl: u32,

    pub rdbal: u32,
    pub rdbah: u32,
    pub rdlen: u32,
    pub rdh: u32,
    pub rdt: u32,

    pub tdbal: u32,
    pub tdbah: u32,
    pub tdlen: u32,
    pub tdh: u32,
    pub tdt: u32,

    // In-flight TX state.
    pub tx_partial: Vec<u8>,
    pub tx_drop: bool,
    pub tx_ctx: E1000TxContextState,
    pub tx_state: E1000TxPacketState,

    // MAC/EEPROM/PHY.
    pub mac_addr: [u8; 6],
    pub ra_valid: bool,
    pub eeprom: [u16; 64],
    pub phy: [u16; 32],

    // Unknown/unmodeled registers.
    pub other_regs: Vec<(u32, u32)>,

    // Host-facing queues (frame payloads, no FCS).
    pub rx_pending: Vec<Vec<u8>>,
    pub tx_out: Vec<Vec<u8>>,
}

impl Default for E1000DeviceState {
    fn default() -> Self {
        Self {
            pci_regs: [0; 256],
            pci_bar0: 0,
            pci_bar0_probe: false,
            pci_bar1: 0,
            pci_bar1_probe: false,
            ctrl: 0,
            status: 0,
            eecd: 0,
            eerd: 0,
            ctrl_ext: 0,
            mdic: 0,
            io_reg: 0,
            icr: 0,
            ims: 0,
            rctl: 0,
            tctl: 0,
            rdbal: 0,
            rdbah: 0,
            rdlen: 0,
            rdh: 0,
            rdt: 0,
            tdbal: 0,
            tdbah: 0,
            tdlen: 0,
            tdh: 0,
            tdt: 0,
            tx_partial: Vec::new(),
            tx_drop: false,
            tx_ctx: E1000TxContextState::default(),
            tx_state: E1000TxPacketState::None,
            mac_addr: [0; 6],
            ra_valid: false,
            eeprom: [0xFFFF; 64],
            phy: [0; 32],
            other_regs: Vec::new(),
            rx_pending: Vec::new(),
            tx_out: Vec::new(),
        }
    }
}

impl IoSnapshot for E1000DeviceState {
    const DEVICE_ID: [u8; 4] = *b"E1K0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI_REGS: u16 = 1;
        const TAG_PCI_BAR0: u16 = 2;
        const TAG_PCI_BAR0_PROBE: u16 = 3;
        const TAG_PCI_BAR1: u16 = 4;
        const TAG_PCI_BAR1_PROBE: u16 = 5;

        const TAG_CTRL: u16 = 10;
        const TAG_STATUS: u16 = 11;
        const TAG_EECD: u16 = 12;
        const TAG_EERD: u16 = 13;
        const TAG_CTRL_EXT: u16 = 14;
        const TAG_MDIC: u16 = 15;
        const TAG_IO_REG: u16 = 16;

        const TAG_ICR: u16 = 20;
        const TAG_IMS: u16 = 21;
        const TAG_IRQ_LEVEL: u16 = 22;

        const TAG_RCTL: u16 = 30;
        const TAG_TCTL: u16 = 31;

        const TAG_RDBAL: u16 = 40;
        const TAG_RDBAH: u16 = 41;
        const TAG_RDLEN: u16 = 42;
        const TAG_RDH: u16 = 43;
        const TAG_RDT: u16 = 44;

        const TAG_TDBAL: u16 = 50;
        const TAG_TDBAH: u16 = 51;
        const TAG_TDLEN: u16 = 52;
        const TAG_TDH: u16 = 53;
        const TAG_TDT: u16 = 54;

        const TAG_TX_PARTIAL: u16 = 60;
        const TAG_TX_DROP: u16 = 61;
        const TAG_TX_CTX_IPCSS: u16 = 62;
        const TAG_TX_CTX_IPCSO: u16 = 63;
        const TAG_TX_CTX_IPCSE: u16 = 64;
        const TAG_TX_CTX_TUCSS: u16 = 65;
        const TAG_TX_CTX_TUCSO: u16 = 66;
        const TAG_TX_CTX_TUCSE: u16 = 67;
        const TAG_TX_CTX_MSS: u16 = 68;
        const TAG_TX_CTX_HDR_LEN: u16 = 69;
        const TAG_TX_STATE_KIND: u16 = 70;
        const TAG_TX_STATE_CMD: u16 = 71;
        const TAG_TX_STATE_CSS: u16 = 72;
        const TAG_TX_STATE_CSO: u16 = 73;
        const TAG_TX_STATE_POPTS: u16 = 74;

        const TAG_MAC_ADDR: u16 = 80;
        const TAG_RA_VALID: u16 = 81;
        const TAG_EEPROM: u16 = 82;
        const TAG_PHY: u16 = 83;

        const TAG_OTHER_REGS: u16 = 90;
        const TAG_RX_PENDING: u16 = 91;
        const TAG_TX_OUT: u16 = 92;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_bytes(TAG_PCI_REGS, self.pci_regs.to_vec());
        w.field_u32(TAG_PCI_BAR0, self.pci_bar0);
        w.field_bool(TAG_PCI_BAR0_PROBE, self.pci_bar0_probe);
        w.field_u32(TAG_PCI_BAR1, self.pci_bar1);
        w.field_bool(TAG_PCI_BAR1_PROBE, self.pci_bar1_probe);

        w.field_u32(TAG_CTRL, self.ctrl);
        w.field_u32(TAG_STATUS, self.status);
        w.field_u32(TAG_EECD, self.eecd);
        w.field_u32(TAG_EERD, self.eerd);
        w.field_u32(TAG_CTRL_EXT, self.ctrl_ext);
        w.field_u32(TAG_MDIC, self.mdic);
        w.field_u32(TAG_IO_REG, self.io_reg);

        w.field_u32(TAG_ICR, self.icr);
        w.field_u32(TAG_IMS, self.ims);
        w.field_bool(TAG_IRQ_LEVEL, (self.icr & self.ims) != 0);

        w.field_u32(TAG_RCTL, self.rctl);
        w.field_u32(TAG_TCTL, self.tctl);

        w.field_u32(TAG_RDBAL, self.rdbal);
        w.field_u32(TAG_RDBAH, self.rdbah);
        w.field_u32(TAG_RDLEN, self.rdlen);
        w.field_u32(TAG_RDH, self.rdh);
        w.field_u32(TAG_RDT, self.rdt);

        w.field_u32(TAG_TDBAL, self.tdbal);
        w.field_u32(TAG_TDBAH, self.tdbah);
        w.field_u32(TAG_TDLEN, self.tdlen);
        w.field_u32(TAG_TDH, self.tdh);
        w.field_u32(TAG_TDT, self.tdt);

        w.field_bytes(TAG_TX_PARTIAL, self.tx_partial.clone());
        w.field_bool(TAG_TX_DROP, self.tx_drop);

        w.field_u32(TAG_TX_CTX_IPCSS, self.tx_ctx.ipcss);
        w.field_u32(TAG_TX_CTX_IPCSO, self.tx_ctx.ipcso);
        w.field_u32(TAG_TX_CTX_IPCSE, self.tx_ctx.ipcse);
        w.field_u32(TAG_TX_CTX_TUCSS, self.tx_ctx.tucss);
        w.field_u32(TAG_TX_CTX_TUCSO, self.tx_ctx.tucso);
        w.field_u32(TAG_TX_CTX_TUCSE, self.tx_ctx.tucse);
        w.field_u32(TAG_TX_CTX_MSS, self.tx_ctx.mss);
        w.field_u32(TAG_TX_CTX_HDR_LEN, self.tx_ctx.hdr_len);

        match self.tx_state {
            E1000TxPacketState::None => {
                w.field_u8(TAG_TX_STATE_KIND, 0);
            }
            E1000TxPacketState::Legacy { cmd, css, cso } => {
                w.field_u8(TAG_TX_STATE_KIND, 1);
                w.field_u8(TAG_TX_STATE_CMD, cmd);
                w.field_u32(TAG_TX_STATE_CSS, css);
                w.field_u32(TAG_TX_STATE_CSO, cso);
            }
            E1000TxPacketState::Advanced { cmd, popts } => {
                w.field_u8(TAG_TX_STATE_KIND, 2);
                w.field_u8(TAG_TX_STATE_CMD, cmd);
                w.field_u8(TAG_TX_STATE_POPTS, popts);
            }
        }

        w.field_bytes(TAG_MAC_ADDR, self.mac_addr.to_vec());
        w.field_bool(TAG_RA_VALID, self.ra_valid);

        let mut eeprom = Vec::with_capacity(self.eeprom.len() * 2);
        for v in self.eeprom {
            eeprom.extend_from_slice(&v.to_le_bytes());
        }
        w.field_bytes(TAG_EEPROM, eeprom);

        let mut phy = Vec::with_capacity(self.phy.len() * 2);
        for v in self.phy {
            phy.extend_from_slice(&v.to_le_bytes());
        }
        w.field_bytes(TAG_PHY, phy);

        let mut other: Vec<(u32, u32)> = self.other_regs.clone();
        other.sort_by_key(|(k, _)| *k);
        let mut other_enc = Encoder::new().u32(other.len() as u32);
        for (k, v) in other {
            other_enc = other_enc.u32(k).u32(v);
        }
        w.field_bytes(TAG_OTHER_REGS, other_enc.finish());

        let mut rx_enc = Encoder::new().u32(self.rx_pending.len() as u32);
        for frame in &self.rx_pending {
            rx_enc = rx_enc.u32(frame.len() as u32).bytes(frame);
        }
        w.field_bytes(TAG_RX_PENDING, rx_enc.finish());

        let mut tx_enc = Encoder::new().u32(self.tx_out.len() as u32);
        for frame in &self.tx_out {
            tx_enc = tx_enc.u32(frame.len() as u32).bytes(frame);
        }
        w.field_bytes(TAG_TX_OUT, tx_enc.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI_REGS: u16 = 1;
        const TAG_PCI_BAR0: u16 = 2;
        const TAG_PCI_BAR0_PROBE: u16 = 3;
        const TAG_PCI_BAR1: u16 = 4;
        const TAG_PCI_BAR1_PROBE: u16 = 5;

        const TAG_CTRL: u16 = 10;
        const TAG_STATUS: u16 = 11;
        const TAG_EECD: u16 = 12;
        const TAG_EERD: u16 = 13;
        const TAG_CTRL_EXT: u16 = 14;
        const TAG_MDIC: u16 = 15;
        const TAG_IO_REG: u16 = 16;

        const TAG_ICR: u16 = 20;
        const TAG_IMS: u16 = 21;
        const TAG_IRQ_LEVEL: u16 = 22;

        const TAG_RCTL: u16 = 30;
        const TAG_TCTL: u16 = 31;

        const TAG_RDBAL: u16 = 40;
        const TAG_RDBAH: u16 = 41;
        const TAG_RDLEN: u16 = 42;
        const TAG_RDH: u16 = 43;
        const TAG_RDT: u16 = 44;

        const TAG_TDBAL: u16 = 50;
        const TAG_TDBAH: u16 = 51;
        const TAG_TDLEN: u16 = 52;
        const TAG_TDH: u16 = 53;
        const TAG_TDT: u16 = 54;

        const TAG_TX_PARTIAL: u16 = 60;
        const TAG_TX_DROP: u16 = 61;
        const TAG_TX_CTX_IPCSS: u16 = 62;
        const TAG_TX_CTX_IPCSO: u16 = 63;
        const TAG_TX_CTX_IPCSE: u16 = 64;
        const TAG_TX_CTX_TUCSS: u16 = 65;
        const TAG_TX_CTX_TUCSO: u16 = 66;
        const TAG_TX_CTX_TUCSE: u16 = 67;
        const TAG_TX_CTX_MSS: u16 = 68;
        const TAG_TX_CTX_HDR_LEN: u16 = 69;
        const TAG_TX_STATE_KIND: u16 = 70;
        const TAG_TX_STATE_CMD: u16 = 71;
        const TAG_TX_STATE_CSS: u16 = 72;
        const TAG_TX_STATE_CSO: u16 = 73;
        const TAG_TX_STATE_POPTS: u16 = 74;

        const TAG_MAC_ADDR: u16 = 80;
        const TAG_RA_VALID: u16 = 81;
        const TAG_EEPROM: u16 = 82;
        const TAG_PHY: u16 = 83;

        const TAG_OTHER_REGS: u16 = 90;
        const TAG_RX_PENDING: u16 = 91;
        const TAG_TX_OUT: u16 = 92;

        fn desc_count(len: u32) -> Option<u32> {
            const DESC_LEN: u32 = 16;
            if len < DESC_LEN || !len.is_multiple_of(DESC_LEN) {
                return None;
            }
            Some(len / DESC_LEN)
        }

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(mac) = r.bytes(TAG_MAC_ADDR) {
            if mac.len() != self.mac_addr.len() {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 mac"));
            }
            self.mac_addr.copy_from_slice(mac);
        }

        if let Some(pci_regs) = r.bytes(TAG_PCI_REGS) {
            if pci_regs.len() != self.pci_regs.len() {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 pci regs"));
            }
            self.pci_regs.copy_from_slice(pci_regs);
        }
        self.pci_bar0 = r.u32(TAG_PCI_BAR0)?.unwrap_or(self.pci_bar0);
        self.pci_bar0_probe = r.bool(TAG_PCI_BAR0_PROBE)?.unwrap_or(self.pci_bar0_probe);
        self.pci_bar1 = r.u32(TAG_PCI_BAR1)?.unwrap_or(self.pci_bar1);
        self.pci_bar1_probe = r.bool(TAG_PCI_BAR1_PROBE)?.unwrap_or(self.pci_bar1_probe);

        self.ctrl = r.u32(TAG_CTRL)?.unwrap_or(self.ctrl);
        self.status = r.u32(TAG_STATUS)?.unwrap_or(self.status);
        self.eecd = r.u32(TAG_EECD)?.unwrap_or(self.eecd);
        self.eerd = r.u32(TAG_EERD)?.unwrap_or(self.eerd);
        self.ctrl_ext = r.u32(TAG_CTRL_EXT)?.unwrap_or(self.ctrl_ext);
        self.mdic = r.u32(TAG_MDIC)?.unwrap_or(self.mdic);
        self.io_reg = r.u32(TAG_IO_REG)?.unwrap_or(self.io_reg);

        self.icr = r.u32(TAG_ICR)?.unwrap_or(self.icr);
        self.ims = r.u32(TAG_IMS)?.unwrap_or(self.ims);

        self.rctl = r.u32(TAG_RCTL)?.unwrap_or(self.rctl);
        self.tctl = r.u32(TAG_TCTL)?.unwrap_or(self.tctl);

        self.rdbal = r.u32(TAG_RDBAL)?.unwrap_or(self.rdbal);
        self.rdbah = r.u32(TAG_RDBAH)?.unwrap_or(self.rdbah);
        self.rdlen = r.u32(TAG_RDLEN)?.unwrap_or(self.rdlen);
        self.rdh = r.u32(TAG_RDH)?.unwrap_or(self.rdh);
        self.rdt = r.u32(TAG_RDT)?.unwrap_or(self.rdt);

        self.tdbal = r.u32(TAG_TDBAL)?.unwrap_or(self.tdbal);
        self.tdbah = r.u32(TAG_TDBAH)?.unwrap_or(self.tdbah);
        self.tdlen = r.u32(TAG_TDLEN)?.unwrap_or(self.tdlen);
        self.tdh = r.u32(TAG_TDH)?.unwrap_or(self.tdh);
        self.tdt = r.u32(TAG_TDT)?.unwrap_or(self.tdt);

        if let Some(buf) = r.bytes(TAG_TX_PARTIAL) {
            if buf.len() > MAX_TX_PARTIAL_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_partial"));
            }
            self.tx_partial = buf.to_vec();
        }
        self.tx_drop = r.bool(TAG_TX_DROP)?.unwrap_or(self.tx_drop);

        let ipcss = r.u32(TAG_TX_CTX_IPCSS)?.unwrap_or(self.tx_ctx.ipcss);
        let ipcso = r.u32(TAG_TX_CTX_IPCSO)?.unwrap_or(self.tx_ctx.ipcso);
        let ipcse = r.u32(TAG_TX_CTX_IPCSE)?.unwrap_or(self.tx_ctx.ipcse);
        let tucss = r.u32(TAG_TX_CTX_TUCSS)?.unwrap_or(self.tx_ctx.tucss);
        let tucso = r.u32(TAG_TX_CTX_TUCSO)?.unwrap_or(self.tx_ctx.tucso);
        let tucse = r.u32(TAG_TX_CTX_TUCSE)?.unwrap_or(self.tx_ctx.tucse);
        let mss = r.u32(TAG_TX_CTX_MSS)?.unwrap_or(self.tx_ctx.mss);
        let hdr_len = r.u32(TAG_TX_CTX_HDR_LEN)?.unwrap_or(self.tx_ctx.hdr_len);

        for v in [ipcss, ipcso, ipcse, tucss, tucso, tucse, mss, hdr_len] {
            if v as usize > MAX_TX_PARTIAL_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_ctx"));
            }
        }
        self.tx_ctx = E1000TxContextState {
            ipcss,
            ipcso,
            ipcse,
            tucss,
            tucso,
            tucse,
            mss,
            hdr_len,
        };

        self.tx_state = match r.u8(TAG_TX_STATE_KIND)?.unwrap_or(0) {
            0 => E1000TxPacketState::None,
            1 => {
                let cmd = r.u8(TAG_TX_STATE_CMD)?.unwrap_or(0);
                let css = r.u32(TAG_TX_STATE_CSS)?.unwrap_or(0);
                let cso = r.u32(TAG_TX_STATE_CSO)?.unwrap_or(0);
                if css as usize > MAX_TX_PARTIAL_BYTES || cso as usize > MAX_TX_PARTIAL_BYTES {
                    return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_state"));
                }
                E1000TxPacketState::Legacy { cmd, css, cso }
            }
            2 => {
                let cmd = r.u8(TAG_TX_STATE_CMD)?.unwrap_or(0);
                let popts = r.u8(TAG_TX_STATE_POPTS)?.unwrap_or(0);
                E1000TxPacketState::Advanced { cmd, popts }
            }
            _ => return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_state kind")),
        };

        self.ra_valid = r.bool(TAG_RA_VALID)?.unwrap_or(self.ra_valid);

        if let Some(buf) = r.bytes(TAG_EEPROM) {
            if buf.len() != self.eeprom.len() * 2 {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 eeprom"));
            }
            for (slot, chunk) in self.eeprom.iter_mut().zip(buf.chunks_exact(2)) {
                *slot = u16::from_le_bytes([chunk[0], chunk[1]]);
            }
        }

        if let Some(buf) = r.bytes(TAG_PHY) {
            if buf.len() != self.phy.len() * 2 {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 phy"));
            }
            for (slot, chunk) in self.phy.iter_mut().zip(buf.chunks_exact(2)) {
                *slot = u16::from_le_bytes([chunk[0], chunk[1]]);
            }
        }

        self.other_regs.clear();
        if let Some(buf) = r.bytes(TAG_OTHER_REGS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_OTHER_REGS {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 other_regs count"));
            }
            let mut regs = std::collections::HashMap::new();
            for _ in 0..count {
                let key = d.u32()?;
                let value = d.u32()?;
                if regs.insert(key, value).is_some() {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "e1000 other_regs duplicate key",
                    ));
                }
            }
            d.finish()?;
            self.other_regs = regs.into_iter().collect();
            self.other_regs.sort_by_key(|(k, _)| *k);
        }

        self.rx_pending.clear();
        if let Some(buf) = r.bytes(TAG_RX_PENDING) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_RX_PENDING_FRAMES {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 rx_pending count"));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if !(MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&len) {
                    return Err(SnapshotError::InvalidFieldEncoding("e1000 rx_pending frame"));
                }
                self.rx_pending.push(d.bytes(len)?.to_vec());
            }
            d.finish()?;
        }

        self.tx_out.clear();
        if let Some(buf) = r.bytes(TAG_TX_OUT) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_TX_OUT_FRAMES {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_out count"));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if !(MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&len) {
                    return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_out frame"));
                }
                self.tx_out.push(d.bytes(len)?.to_vec());
            }
            d.finish()?;
        }

        // Validate ring indices to avoid getting stuck in `process_tx`/`flush_rx_pending` after
        // restoring a corrupted snapshot.
        if let Some(desc_count) = desc_count(self.tdlen) {
            if desc_count == 0 || self.tdh >= desc_count || self.tdt >= desc_count {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx ring indices"));
            }
        }
        if let Some(desc_count) = desc_count(self.rdlen) {
            if desc_count == 0 || self.rdh >= desc_count || self.rdt >= desc_count {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 rx ring indices"));
            }
        }

        let computed_irq = (self.icr & self.ims) != 0;
        if let Some(saved_irq) = r.bool(TAG_IRQ_LEVEL)? {
            if saved_irq != computed_irq {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 irq_level"));
            }
        }

        Ok(())
    }
}

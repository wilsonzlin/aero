use super::offload::{apply_checksum_offload, tso_segment, TxChecksumFlags, TxOffloadContext};
use super::{
    E1000Device, GuestMemory, NetworkBackend, TxDesc, TxPacketState, ICR_TXDW, MAX_L2_FRAME_LEN,
    MAX_TX_AGGREGATE_LEN, MIN_L2_FRAME_LEN, TCTL_EN, TXD_CMD_DEXT, TXD_CMD_EOP, TXD_CMD_IC,
    TXD_CMD_RS, TXD_CMD_TSE, TXD_STAT_DD,
};

use nt_packetlib::io::net::packet::checksum::internet_checksum;

const TXD_DTYP_CTXT: u8 = 0x2;
const TXD_DTYP_DATA: u8 = 0x3;

#[derive(Debug, Clone, Copy)]
struct TxContextDesc {
    ipcss: u8,
    ipcso: u8,
    ipcse: u16,
    tucss: u8,
    tucso: u8,
    tucse: u16,
    mss: u16,
    hdr_len: u8,
    cmd: u8,
}

impl TxContextDesc {
    fn from_bytes(bytes: [u8; TxDesc::LEN]) -> Self {
        Self {
            ipcss: bytes[0],
            ipcso: bytes[1],
            ipcse: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
            tucss: bytes[4],
            tucso: bytes[5],
            tucse: u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            cmd: bytes[11],
            mss: u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
            hdr_len: bytes[14],
        }
    }
}

impl From<TxContextDesc> for TxOffloadContext {
    fn from(value: TxContextDesc) -> Self {
        Self {
            ipcss: value.ipcss as usize,
            ipcso: value.ipcso as usize,
            ipcse: value.ipcse as usize,
            tucss: value.tucss as usize,
            tucso: value.tucso as usize,
            tucse: value.tucse as usize,
            mss: value.mss as usize,
            hdr_len: value.hdr_len as usize,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TxDataDesc {
    buffer_addr: u64,
    length: u16,
    cmd: u8,
    popts: u8,
}

impl TxDataDesc {
    fn from_bytes(bytes: [u8; TxDesc::LEN]) -> Self {
        Self {
            buffer_addr: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            cmd: bytes[11],
            popts: bytes[13],
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TxDescriptor {
    Legacy(TxDesc),
    Context(TxContextDesc),
    Data(TxDataDesc),
}

impl TxDescriptor {
    fn parse(bytes: [u8; TxDesc::LEN]) -> Option<Self> {
        let cmd = bytes[11];
        if (cmd & TXD_CMD_DEXT) == 0 {
            return Some(Self::Legacy(TxDesc::from_bytes(bytes)));
        }

        let dtyp = bytes[10] >> 4;
        match dtyp {
            TXD_DTYP_CTXT => Some(Self::Context(TxContextDesc::from_bytes(bytes))),
            TXD_DTYP_DATA => Some(Self::Data(TxDataDesc::from_bytes(bytes))),
            _ => None,
        }
    }
}

fn read_desc<M: GuestMemory>(mem: &M, addr: u64) -> [u8; TxDesc::LEN] {
    let mut buf = [0u8; TxDesc::LEN];
    mem.read(addr, &mut buf);
    buf
}

fn write_desc<M: GuestMemory>(mem: &mut M, addr: u64, bytes: &[u8; TxDesc::LEN]) {
    mem.write(addr, bytes);
}

impl E1000Device {
    fn transmit_frame<B: NetworkBackend>(&mut self, backend: &mut B, frame: Vec<u8>) {
        if frame.len() < MIN_L2_FRAME_LEN || frame.len() > MAX_L2_FRAME_LEN {
            return;
        }
        backend.transmit(frame);
    }

    fn enter_tx_drop_mode(&mut self) {
        self.tx_drop = true;
        self.tx_partial.clear();
        self.tx_state = None;
    }

    fn append_tx_data<M: GuestMemory>(&mut self, mem: &M, buffer_addr: u64, len: usize, tso: bool) {
        if self.tx_drop || buffer_addr == 0 || len == 0 {
            return;
        }

        let new_len = self
            .tx_partial
            .len()
            .checked_add(len)
            .unwrap_or(MAX_TX_AGGREGATE_LEN + 1);

        if new_len > MAX_TX_AGGREGATE_LEN || (!tso && new_len > MAX_L2_FRAME_LEN) {
            self.enter_tx_drop_mode();
            return;
        }

        let old_len = self.tx_partial.len();
        self.tx_partial.resize(new_len, 0);
        mem.read(buffer_addr, &mut self.tx_partial[old_len..new_len]);
    }

    pub(crate) fn process_tx<M: GuestMemory, B: NetworkBackend>(
        &mut self,
        mem: &mut M,
        backend: &mut B,
    ) {
        if self.tctl & TCTL_EN == 0 {
            return;
        }

        let count = (self.tdlen / 16) as u32;
        if count == 0 {
            return;
        }

        let base = self.tx_ring_base();
        let mut should_raise_txdw = false;

        while self.tdh != self.tdt {
            let idx = self.tdh % count;
            let desc_addr = base + idx as u64 * 16;
            let mut desc_bytes = read_desc(mem, desc_addr);

            let Some(desc) = TxDescriptor::parse(desc_bytes) else {
                // Unknown descriptor type; best-effort mark completion and move on.
                desc_bytes[12] |= TXD_STAT_DD;
                write_desc(mem, desc_addr, &desc_bytes);
                self.tdh = (self.tdh + 1) % count;
                continue;
            };

            match desc {
                TxDescriptor::Context(ctx_desc) => {
                    self.tx_ctx = ctx_desc.into();

                    if (ctx_desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    // Context descriptors overlap status with MSS; real hardware overwrites the
                    // context descriptor on completion and drivers only care about DD.
                    desc_bytes[12] |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc_bytes);
                }
                TxDescriptor::Legacy(mut desc) => {
                    match self.tx_state {
                        None => {
                            self.tx_state = Some(TxPacketState::Legacy {
                                cmd: desc.cmd,
                                css: desc.css as usize,
                                cso: desc.cso as usize,
                            });
                        }
                        Some(TxPacketState::Legacy {
                            ref mut cmd,
                            ref mut css,
                            ref mut cso,
                        }) => {
                            *cmd |= desc.cmd;
                            *css = desc.css as usize;
                            *cso = desc.cso as usize;
                        }
                        Some(TxPacketState::Advanced { .. }) => {
                            self.tx_partial.clear();
                            self.tx_state = Some(TxPacketState::Legacy {
                                cmd: desc.cmd,
                                css: desc.css as usize,
                                cso: desc.cso as usize,
                            });
                        }
                    }

                    if desc.buffer_addr != 0 && desc.length != 0 {
                        self.append_tx_data(mem, desc.buffer_addr, desc.length as usize, false);
                    }

                    desc.status |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc.to_bytes());

                    if (desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    if (desc.cmd & TXD_CMD_EOP) != 0 {
                        if self.tx_drop {
                            self.tx_drop = false;
                            self.tx_partial.clear();
                            self.tx_state = None;
                            self.tdh = (self.tdh + 1) % count;
                            continue;
                        }

                        let Some(TxPacketState::Legacy { cmd, css, cso }) = self.tx_state.take()
                        else {
                            self.tx_partial.clear();
                            self.tx_state = None;
                            self.tdh = (self.tdh + 1) % count;
                            continue;
                        };

                        if !self.tx_partial.is_empty() {
                            let mut frame = std::mem::take(&mut self.tx_partial);
                            if (cmd & TXD_CMD_IC) != 0
                                && css < frame.len()
                                && cso + 2 <= frame.len()
                            {
                                frame[cso..cso + 2].fill(0);
                                let csum = internet_checksum(&frame[css..]);
                                frame[cso..cso + 2].copy_from_slice(&csum.to_be_bytes());
                            }
                            self.transmit_frame(backend, frame);
                        }
                    }
                }
                TxDescriptor::Data(desc) => {
                    let cmd = match self.tx_state {
                        None => {
                            self.tx_state = Some(TxPacketState::Advanced {
                                cmd: desc.cmd,
                                popts: desc.popts,
                            });
                            desc.cmd
                        }
                        Some(TxPacketState::Advanced {
                            ref mut cmd,
                            ref mut popts,
                        }) => {
                            *cmd |= desc.cmd;
                            *popts |= desc.popts;
                            *cmd
                        }
                        Some(TxPacketState::Legacy { .. }) => {
                            self.tx_partial.clear();
                            self.tx_state = Some(TxPacketState::Advanced {
                                cmd: desc.cmd,
                                popts: desc.popts,
                            });
                            desc.cmd
                        }
                    };

                    let tso = (cmd & TXD_CMD_TSE) != 0;
                    if desc.buffer_addr != 0 && desc.length != 0 {
                        self.append_tx_data(mem, desc.buffer_addr, desc.length as usize, tso);
                    }

                    desc_bytes[12] |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc_bytes);

                    if (desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    if (desc.cmd & TXD_CMD_EOP) != 0 {
                        if self.tx_drop {
                            self.tx_drop = false;
                            self.tx_partial.clear();
                            self.tx_state = None;
                            self.tdh = (self.tdh + 1) % count;
                            continue;
                        }

                        let Some(TxPacketState::Advanced { cmd, popts }) = self.tx_state.take()
                        else {
                            self.tx_partial.clear();
                            self.tx_state = None;
                            self.tdh = (self.tdh + 1) % count;
                            continue;
                        };

                        if !self.tx_partial.is_empty() {
                            let flags = TxChecksumFlags::from_popts(popts);
                            let mut frame = std::mem::take(&mut self.tx_partial);

                            if (cmd & TXD_CMD_TSE) != 0 {
                                match tso_segment(&frame, self.tx_ctx, flags) {
                                    Ok(frames) => {
                                        for frame in frames {
                                            self.transmit_frame(backend, frame);
                                        }
                                    }
                                    Err(_) => {
                                        if (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN)
                                            .contains(&frame.len())
                                        {
                                            let _ = apply_checksum_offload(
                                                &mut frame,
                                                self.tx_ctx,
                                                flags,
                                            );
                                            self.transmit_frame(backend, frame);
                                        }
                                    }
                                }
                            } else {
                                if (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&frame.len()) {
                                    let _ = apply_checksum_offload(&mut frame, self.tx_ctx, flags);
                                    self.transmit_frame(backend, frame);
                                }
                            }
                        }
                    }
                }
            }

            self.tdh = (self.tdh + 1) % count;
        }

        if should_raise_txdw {
            self.raise_interrupt(ICR_TXDW);
        }
    }
}

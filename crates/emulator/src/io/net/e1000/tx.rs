use super::{
    E1000Device, GuestMemory, NetworkBackend, TxDesc, ICR_TXDW, TCTL_EN, TXD_CMD_EOP, TXD_STAT_DD,
};

impl E1000Device {
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

        while self.tdh != self.tdt {
            let idx = self.tdh % count;
            let desc_addr = base + idx as u64 * 16;
            let mut desc = TxDesc::read(mem, desc_addr);

            if desc.length != 0 {
                let mut buf = vec![0u8; desc.length as usize];
                mem.read(desc.buffer_addr, &mut buf);
                self.tx_partial.extend_from_slice(&buf);
            }

            desc.status |= TXD_STAT_DD;
            desc.write(mem, desc_addr);

            let eop = (desc.cmd & TXD_CMD_EOP) != 0;
            self.tdh = (self.tdh + 1) % count;

            if eop {
                if !self.tx_partial.is_empty() {
                    backend.transmit(std::mem::take(&mut self.tx_partial));
                }
                self.raise_interrupt(ICR_TXDW);
            }
        }
    }
}


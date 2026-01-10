use super::{E1000Device, GuestMemory, RxDesc, ICR_RXT0, RCTL_EN, RXD_STAT_DD, RXD_STAT_EOP};

impl E1000Device {
    pub(crate) fn process_rx<M: GuestMemory>(&mut self, mem: &mut M) {
        if self.rctl & RCTL_EN == 0 {
            return;
        }

        let count = (self.rdlen / 16) as u32;
        if count == 0 {
            return;
        }

        let base = self.rx_ring_base();
        let buf_len = self.rx_buffer_size();

        while let Some(frame) = self.rx_queue.front() {
            // The hardware head (RDH) must not catch up to the software tail (RDT).
            // Keep one descriptor unused to avoid ambiguity in full/empty conditions.
            if self.rdh == self.rdt {
                break;
            }

            let idx = self.rdh % count;
            let desc_addr = base + idx as u64 * 16;
            let mut desc = RxDesc::read(mem, desc_addr);

            let copy_len = frame.len().min(buf_len);
            mem.write(desc.buffer_addr, &frame[..copy_len]);

            desc.length = copy_len as u16;
            desc.status = RXD_STAT_DD | RXD_STAT_EOP;
            desc.errors = 0;
            desc.csum = 0;
            desc.special = 0;
            desc.write(mem, desc_addr);

            self.rdh = (self.rdh + 1) % count;
            self.rx_queue.pop_front();

            self.raise_interrupt(ICR_RXT0);
        }
    }
}


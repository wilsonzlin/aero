pub use aero_pcapng::{LinkType, PacketDirection};

pub struct PcapngWriter {
    buf: Vec<u8>,
    next_interface_id: u32,
}

impl PcapngWriter {
    pub fn new(user_appl: &str) -> Self {
        Self {
            buf: aero_pcapng::section_header_block(user_appl),
            next_interface_id: 0,
        }
    }

    pub fn add_interface(&mut self, link_type: LinkType, name: &str) -> u32 {
        let id = self.next_interface_id;
        self.next_interface_id += 1;
        self.buf
            .extend_from_slice(&aero_pcapng::interface_description_block(link_type, name));
        id
    }

    pub fn write_packet(
        &mut self,
        interface_id: u32,
        timestamp_ns: u64,
        payload: &[u8],
        direction: Option<PacketDirection>,
    ) {
        self.buf
            .extend_from_slice(&aero_pcapng::enhanced_packet_block(
                interface_id,
                timestamp_ns,
                payload,
                direction,
            ));
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_header_block_has_expected_fields() {
        let w = PcapngWriter::new("aero-test");
        let bytes = w.into_bytes();

        let mut cursor = 0usize;
        let block_type = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        assert_eq!(block_type, 0x0A0D0D0A);

        let total_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;

        assert!(total_len >= 28);
        assert_eq!(
            &bytes[total_len - 4..total_len],
            &(total_len as u32).to_le_bytes()
        );

        let bom = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        assert_eq!(bom, 0x1A2B3C4D);
    }

    #[test]
    fn interface_description_block_has_expected_linktype_and_tsresol() {
        let mut w = PcapngWriter::new("aero-test");
        w.add_interface(LinkType::Ethernet, "guest-eth0");
        let bytes = w.into_bytes();

        let mut offset = 0usize;
        let shb_len =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += shb_len;

        let block_type = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(block_type, 0x0000_0001);

        let idb_len =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;

        let link_type = u16::from_le_bytes(bytes[offset + 8..offset + 10].try_into().unwrap());
        assert_eq!(link_type, 1);

        let mut opt_off = offset + 16;
        let opts_end = offset + idb_len - 4;
        let mut found = false;
        while opt_off + 4 <= opts_end {
            let code = u16::from_le_bytes(bytes[opt_off..opt_off + 2].try_into().unwrap());
            let len =
                u16::from_le_bytes(bytes[opt_off + 2..opt_off + 4].try_into().unwrap()) as usize;
            opt_off += 4;
            if code == 0 {
                break;
            }
            if code == 9 {
                assert_eq!(len, 1);
                assert_eq!(bytes[opt_off], 9u8);
                found = true;
            }
            opt_off += len;
            opt_off = (opt_off + 3) & !3;
        }

        assert!(found, "missing if_tsresol option");
    }
}

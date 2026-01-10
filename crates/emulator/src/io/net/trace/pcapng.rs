#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    Ethernet,
    User0,
}

impl LinkType {
    fn to_pcapng(self) -> u16 {
        match self {
            LinkType::Ethernet => 1,
            LinkType::User0 => 147,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketDirection {
    Inbound,
    Outbound,
}

pub struct PcapngWriter {
    buf: Vec<u8>,
    next_interface_id: u32,
}

impl PcapngWriter {
    pub fn new(user_appl: &str) -> Self {
        let mut this = Self {
            buf: Vec::new(),
            next_interface_id: 0,
        };
        this.write_section_header_block(user_appl);
        this
    }

    pub fn add_interface(&mut self, link_type: LinkType, name: &str) -> u32 {
        let id = self.next_interface_id;
        self.next_interface_id += 1;
        self.write_interface_description_block(link_type, name);
        id
    }

    pub fn write_packet(
        &mut self,
        interface_id: u32,
        timestamp_ns: u64,
        payload: &[u8],
        direction: Option<PacketDirection>,
    ) {
        self.write_enhanced_packet_block(interface_id, timestamp_ns, payload, direction);
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn write_section_header_block(&mut self, user_appl: &str) {
        const BLOCK_TYPE: u32 = 0x0A0D0D0A;

        let mut body = Vec::new();
        body.extend_from_slice(&0x1A2B3C4Du32.to_le_bytes()); // byte-order magic
        body.extend_from_slice(&1u16.to_le_bytes()); // major
        body.extend_from_slice(&0u16.to_le_bytes()); // minor
        body.extend_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()); // section length: unspecified

        let mut opts = Vec::new();
        write_opt_str(&mut opts, 4, user_appl); // shb_userappl
        write_opt_end(&mut opts);

        write_block(&mut self.buf, BLOCK_TYPE, &body, &opts);
    }

    fn write_interface_description_block(&mut self, link_type: LinkType, name: &str) {
        const BLOCK_TYPE: u32 = 0x0000_0001;

        let mut body = Vec::new();
        body.extend_from_slice(&link_type.to_pcapng().to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // reserved
        body.extend_from_slice(&65535u32.to_le_bytes()); // snaplen

        let mut opts = Vec::new();
        write_opt_str(&mut opts, 2, name); // if_name
        write_opt_u8(&mut opts, 9, 9); // if_tsresol (10^-9)
        write_opt_end(&mut opts);

        write_block(&mut self.buf, BLOCK_TYPE, &body, &opts);
    }

    fn write_enhanced_packet_block(
        &mut self,
        interface_id: u32,
        timestamp_ns: u64,
        payload: &[u8],
        direction: Option<PacketDirection>,
    ) {
        const BLOCK_TYPE: u32 = 0x0000_0006;

        let mut body = Vec::new();
        body.extend_from_slice(&interface_id.to_le_bytes());

        let ts_high = (timestamp_ns >> 32) as u32;
        let ts_low = (timestamp_ns & 0xFFFF_FFFF) as u32;
        body.extend_from_slice(&ts_high.to_le_bytes());
        body.extend_from_slice(&ts_low.to_le_bytes());

        let cap_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        body.extend_from_slice(&cap_len.to_le_bytes());
        body.extend_from_slice(&cap_len.to_le_bytes());

        body.extend_from_slice(payload);
        pad_to_32(&mut body);

        let mut opts = Vec::new();
        if let Some(direction) = direction {
            let dir_bits = match direction {
                PacketDirection::Inbound => 1u32,
                PacketDirection::Outbound => 2u32,
            };
            write_opt_u32(&mut opts, 2, dir_bits); // epb_flags
        }
        write_opt_end(&mut opts);

        write_block(&mut self.buf, BLOCK_TYPE, &body, &opts);
    }
}

fn pad_to_32(buf: &mut Vec<u8>) {
    let pad_len = (4 - (buf.len() % 4)) % 4;
    buf.extend(std::iter::repeat(0u8).take(pad_len));
}

fn write_block(out: &mut Vec<u8>, block_type: u32, body: &[u8], opts: &[u8]) {
    let total_len = 12 + body.len() + opts.len();
    let total_len_u32 = u32::try_from(total_len).expect("pcapng block too large");

    out.extend_from_slice(&block_type.to_le_bytes());
    out.extend_from_slice(&total_len_u32.to_le_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(opts);
    out.extend_from_slice(&total_len_u32.to_le_bytes());
}

fn write_opt_end(out: &mut Vec<u8>) {
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
}

fn write_opt_str(out: &mut Vec<u8>, code: u16, val: &str) {
    let bytes = val.as_bytes();
    write_opt(out, code, bytes);
}

fn write_opt_u32(out: &mut Vec<u8>, code: u16, val: u32) {
    write_opt(out, code, &val.to_le_bytes());
}

fn write_opt_u8(out: &mut Vec<u8>, code: u16, val: u8) {
    write_opt(out, code, &[val]);
}

fn write_opt(out: &mut Vec<u8>, code: u16, val: &[u8]) {
    out.extend_from_slice(&code.to_le_bytes());
    out.extend_from_slice(&u16::try_from(val.len()).expect("pcapng option too large").to_le_bytes());
    out.extend_from_slice(val);
    pad_to_32(out);
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
        assert_eq!(&bytes[total_len - 4..total_len], &(total_len as u32).to_le_bytes());

        let bom = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        assert_eq!(bom, 0x1A2B3C4D);
    }

    #[test]
    fn interface_description_block_has_expected_linktype_and_tsresol() {
        let mut w = PcapngWriter::new("aero-test");
        w.add_interface(LinkType::Ethernet, "guest-eth0");
        let bytes = w.into_bytes();

        let mut offset = 0usize;
        let shb_len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += shb_len;

        let block_type = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(block_type, 0x0000_0001);

        let idb_len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;

        let link_type = u16::from_le_bytes(bytes[offset + 8..offset + 10].try_into().unwrap());
        assert_eq!(link_type, 1);

        let mut opt_off = offset + 16;
        let opts_end = offset + idb_len - 4;
        let mut found = false;
        while opt_off + 4 <= opts_end {
            let code = u16::from_le_bytes(bytes[opt_off..opt_off + 2].try_into().unwrap());
            let len = u16::from_le_bytes(bytes[opt_off + 2..opt_off + 4].try_into().unwrap()) as usize;
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


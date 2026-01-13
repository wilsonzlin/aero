#![forbid(unsafe_code)]

//! Minimal PCAPNG (PCAP Next Generation) block builder.
//!
//! This crate intentionally provides a *pure* builder API that returns fully
//! formed blocks as `Vec<u8>` so callers can decide how to write them (sync,
//! async, in-memory, streaming, etc).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    Ethernet,
    User0,
    User1,
}

impl LinkType {
    fn to_pcapng(self) -> u16 {
        match self {
            LinkType::Ethernet => 1,
            // https://www.iana.org/assignments/link-types/link-types.xhtml
            LinkType::User0 => 147,
            LinkType::User1 => 148,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketDirection {
    Inbound,
    Outbound,
}

/// Build a PCAPNG Section Header Block (SHB).
pub fn section_header_block(user_appl: &str) -> Vec<u8> {
    const BLOCK_TYPE: u32 = 0x0A0D0D0A;

    let mut body = Vec::new();
    body.extend_from_slice(&0x1A2B3C4Du32.to_le_bytes()); // byte-order magic
    body.extend_from_slice(&1u16.to_le_bytes()); // major
    body.extend_from_slice(&0u16.to_le_bytes()); // minor
    body.extend_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()); // section length: unspecified

    let mut opts = Vec::new();
    write_opt_str(&mut opts, 4, user_appl); // shb_userappl
    write_opt_end(&mut opts);

    build_block(BLOCK_TYPE, &body, &opts)
}

/// Build a PCAPNG Interface Description Block (IDB).
pub fn interface_description_block(link_type: LinkType, name: &str) -> Vec<u8> {
    const BLOCK_TYPE: u32 = 0x0000_0001;

    let mut body = Vec::new();
    body.extend_from_slice(&link_type.to_pcapng().to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes()); // reserved
    body.extend_from_slice(&65535u32.to_le_bytes()); // snaplen

    let mut opts = Vec::new();
    write_opt_str(&mut opts, 2, name); // if_name
    write_opt_u8(&mut opts, 9, 9); // if_tsresol (10^-9)
    write_opt_end(&mut opts);

    build_block(BLOCK_TYPE, &body, &opts)
}

/// Build a PCAPNG Enhanced Packet Block (EPB).
pub fn enhanced_packet_block(
    interface_id: u32,
    timestamp_ns: u64,
    payload: &[u8],
    direction: Option<PacketDirection>,
) -> Vec<u8> {
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

    build_block(BLOCK_TYPE, &body, &opts)
}

fn pad_to_32(buf: &mut Vec<u8>) {
    let pad_len = (4 - (buf.len() % 4)) % 4;
    buf.extend(std::iter::repeat_n(0u8, pad_len));
}

fn build_block(block_type: u32, body: &[u8], opts: &[u8]) -> Vec<u8> {
    let total_len = 12 + body.len() + opts.len();
    let total_len_u32 = u32::try_from(total_len).expect("pcapng block too large");

    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&block_type.to_le_bytes());
    out.extend_from_slice(&total_len_u32.to_le_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(opts);
    out.extend_from_slice(&total_len_u32.to_le_bytes());
    out
}

fn write_opt_end(out: &mut Vec<u8>) {
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
}

fn write_opt_str(out: &mut Vec<u8>, code: u16, val: &str) {
    write_opt(out, code, val.as_bytes());
}

fn write_opt_u32(out: &mut Vec<u8>, code: u16, val: u32) {
    write_opt(out, code, &val.to_le_bytes());
}

fn write_opt_u8(out: &mut Vec<u8>, code: u16, val: u8) {
    write_opt(out, code, &[val]);
}

fn write_opt(out: &mut Vec<u8>, code: u16, val: &[u8]) {
    out.extend_from_slice(&code.to_le_bytes());
    out.extend_from_slice(
        &u16::try_from(val.len())
            .expect("pcapng option too large")
            .to_le_bytes(),
    );
    out.extend_from_slice(val);
    pad_to_32(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_header_block_has_expected_fields() {
        let bytes = section_header_block("aero-test");

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
    fn enhanced_packet_block_pads_and_sets_flags() {
        // Use a payload length that requires padding.
        let payload = [0xAAu8; 7];
        let bytes = enhanced_packet_block(
            3,
            0x1122_3344_5566_7788,
            &payload,
            Some(PacketDirection::Outbound),
        );

        // Block length must be a multiple of 4 bytes.
        let total_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(total_len % 4, 0);
        assert_eq!(bytes.len(), total_len);

        // Parse caplen.
        let cap_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!(cap_len as usize, payload.len());

        // Ensure payload is followed by zero padding (to 32-bit boundary).
        let payload_off = 28;
        assert_eq!(&bytes[payload_off..payload_off + payload.len()], &payload);
        assert_eq!(&bytes[payload_off + payload.len()..payload_off + 8], &[0u8]);

        // Ensure the flags option is present (code 2, len 4, value 2).
        let opts_off = payload_off + 8;
        let code = u16::from_le_bytes(bytes[opts_off..opts_off + 2].try_into().unwrap());
        let len = u16::from_le_bytes(bytes[opts_off + 2..opts_off + 4].try_into().unwrap());
        assert_eq!(code, 2);
        assert_eq!(len, 4);
        let val = u32::from_le_bytes(bytes[opts_off + 4..opts_off + 8].try_into().unwrap());
        assert_eq!(val, 2);
    }
}

use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    Ethernet,
}

impl LinkType {
    fn to_pcapng(self) -> u16 {
        match self {
            LinkType::Ethernet => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketDirection {
    Inbound,
    Outbound,
}

pub struct PcapngWriter<W: AsyncWrite + Unpin> {
    out: W,
    next_interface_id: u32,
}

impl<W: AsyncWrite + Unpin> PcapngWriter<W> {
    pub async fn new(mut out: W, user_appl: &str) -> std::io::Result<Self> {
        let bytes = section_header_block(user_appl);
        out.write_all(&bytes).await?;
        Ok(Self {
            out,
            next_interface_id: 0,
        })
    }

    pub async fn add_interface(&mut self, link_type: LinkType, name: &str) -> std::io::Result<u32> {
        let id = self.next_interface_id;
        self.next_interface_id += 1;

        let bytes = interface_description_block(link_type, name);
        self.out.write_all(&bytes).await?;
        Ok(id)
    }

    pub async fn write_packet(
        &mut self,
        interface_id: u32,
        timestamp_ns: u64,
        payload: &[u8],
        direction: Option<PacketDirection>,
    ) -> std::io::Result<()> {
        let bytes = enhanced_packet_block(interface_id, timestamp_ns, payload, direction);
        self.out.write_all(&bytes).await
    }

    pub async fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush().await
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

fn section_header_block(user_appl: &str) -> Vec<u8> {
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

fn interface_description_block(link_type: LinkType, name: &str) -> Vec<u8> {
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

fn enhanced_packet_block(
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
    buf.extend(std::iter::repeat(0u8).take(pad_len));
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

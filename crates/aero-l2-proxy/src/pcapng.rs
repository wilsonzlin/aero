use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    Ethernet,
}

impl LinkType {
    fn to_aero_pcapng(self) -> aero_pcapng::LinkType {
        match self {
            LinkType::Ethernet => aero_pcapng::LinkType::Ethernet,
        }
    }
}

pub use aero_pcapng::PacketDirection;

pub struct PcapngWriter<W: AsyncWrite + Unpin> {
    out: W,
    next_interface_id: u32,
}

impl<W: AsyncWrite + Unpin> PcapngWriter<W> {
    pub async fn new(mut out: W, user_appl: &str) -> std::io::Result<Self> {
        let bytes = aero_pcapng::section_header_block(user_appl);
        out.write_all(&bytes).await?;
        Ok(Self {
            out,
            next_interface_id: 0,
        })
    }

    pub async fn add_interface(&mut self, link_type: LinkType, name: &str) -> std::io::Result<u32> {
        let id = self.next_interface_id;
        self.next_interface_id += 1;

        let bytes = aero_pcapng::interface_description_block(link_type.to_aero_pcapng(), name);
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
        let bytes =
            aero_pcapng::enhanced_packet_block(interface_id, timestamp_ns, payload, direction);
        self.out.write_all(&bytes).await
    }

    pub async fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush().await
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

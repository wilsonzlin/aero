use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::{
    fs,
    io::{AsyncWriteExt, BufWriter},
};

use crate::pcapng::{LinkType, PacketDirection, PcapngWriter};

#[derive(Clone)]
pub struct CaptureManager {
    dir: Option<PathBuf>,
}

impl CaptureManager {
    pub async fn new(dir: Option<PathBuf>) -> std::io::Result<Self> {
        if let Some(dir) = dir.as_ref() {
            fs::create_dir_all(dir).await?;
        }
        Ok(Self { dir })
    }

    pub async fn open_session(&self, session_id: u64) -> std::io::Result<Option<SessionCapture>> {
        let Some(dir) = self.dir.as_ref() else {
            return Ok(None);
        };

        let ts_ms = now_ms();
        let filename = format!("{ts_ms:013}-session-{session_id}.pcapng");
        let path = dir.join(filename);

        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;

        let writer = BufWriter::new(file);
        let mut pcap = PcapngWriter::new(writer, "aero-l2-proxy").await?;
        let iface = pcap.add_interface(LinkType::Ethernet, "l2-tunnel").await?;
        pcap.flush().await?;

        Ok(Some(SessionCapture { path, pcap, iface }))
    }
}

pub struct SessionCapture {
    path: PathBuf,
    pcap: PcapngWriter<BufWriter<fs::File>>,
    iface: u32,
}

impl SessionCapture {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn record_guest_to_proxy(
        &mut self,
        timestamp_ns: u64,
        frame: &[u8],
    ) -> std::io::Result<()> {
        self.pcap
            .write_packet(
                self.iface,
                timestamp_ns,
                frame,
                Some(PacketDirection::Inbound),
            )
            .await?;
        self.pcap.flush().await
    }

    pub async fn record_proxy_to_guest(
        &mut self,
        timestamp_ns: u64,
        frame: &[u8],
    ) -> std::io::Result<()> {
        self.pcap
            .write_packet(
                self.iface,
                timestamp_ns,
                frame,
                Some(PacketDirection::Outbound),
            )
            .await?;
        self.pcap.flush().await
    }

    pub async fn close(mut self) -> std::io::Result<()> {
        self.pcap.flush().await?;
        let mut writer = self.pcap.into_inner();
        writer.flush().await
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::task::{Context, Poll};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::{
    fs,
    io::{AsyncWrite, AsyncWriteExt, BufWriter},
};

use crate::metrics::Metrics;
use crate::pcapng::{LinkType, PacketDirection, PcapngWriter};

#[derive(Clone)]
pub struct CaptureManager {
    dir: Option<PathBuf>,
    max_bytes: u64,
    flush_interval: Option<Duration>,
    metrics: Metrics,
}

impl CaptureManager {
    pub async fn new(
        dir: Option<PathBuf>,
        max_bytes: u64,
        flush_interval: Option<Duration>,
        metrics: Metrics,
    ) -> Self {
        if let Some(dir) = dir.as_ref() {
            if let Err(err) = fs::create_dir_all(dir).await {
                metrics.capture_error();
                tracing::warn!(path = ?dir, "failed to create capture directory: {err}");
                return Self {
                    dir: None,
                    max_bytes,
                    flush_interval,
                    metrics,
                };
            }
        }
        Self {
            dir,
            max_bytes,
            flush_interval,
            metrics,
        }
    }

    pub async fn open_session(&self, session_id: u64) -> Option<SessionCapture> {
        let dir = self.dir.as_ref()?;

        let ts_ms = now_ms();
        let filename = format!("{ts_ms:013}-session-{session_id}.pcapng");
        let path = dir.join(filename);

        let file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => file,
            Err(err) => {
                self.metrics.capture_error();
                tracing::warn!(path = ?path, "failed to create capture file: {err}");
                return None;
            }
        };

        let bytes_written = Arc::new(AtomicU64::new(0));
        let writer = BufWriter::new(file);
        let writer = CountingWriter::new(writer, bytes_written.clone());

        let mut pcap = match PcapngWriter::new(writer, "aero-l2-proxy").await {
            Ok(pcap) => pcap,
            Err(err) => {
                self.metrics.capture_error();
                tracing::warn!(path = ?path, "failed to initialise capture file: {err}");
                return None;
            }
        };
        let iface = match pcap.add_interface(LinkType::Ethernet, "l2-tunnel").await {
            Ok(iface) => iface,
            Err(err) => {
                self.metrics.capture_error();
                tracing::warn!(path = ?path, "failed to write capture interface header: {err}");
                return None;
            }
        };

        // Count bytes written by the pcapng section header + interface description blocks.
        let header_bytes = bytes_written.load(Ordering::Relaxed);
        if header_bytes > 0 {
            self.metrics.capture_bytes_written(header_bytes);
        }

        // Respect "flush only on close" mode: don't flush unless periodic flushing is enabled.
        if self.flush_interval.is_some() {
            if let Err(err) = pcap.flush().await {
                self.metrics.capture_error();
                tracing::warn!(path = ?path, "failed to flush capture header: {err}");
                return None;
            }
        }

        Some(SessionCapture {
            path,
            pcap,
            iface,
            bytes_written,
            max_bytes: self.max_bytes,
            flush_interval: self.flush_interval,
            last_flush: tokio::time::Instant::now(),
            last_flushed_bytes: header_bytes,
            metrics: self.metrics.clone(),
            capped: false,
            disabled: false,
        })
    }
}

pub struct SessionCapture {
    path: PathBuf,
    pcap: PcapngWriter<CountingWriter<BufWriter<fs::File>>>,
    iface: u32,
    bytes_written: Arc<AtomicU64>,
    max_bytes: u64,
    flush_interval: Option<Duration>,
    last_flush: tokio::time::Instant,
    last_flushed_bytes: u64,
    metrics: Metrics,
    capped: bool,
    disabled: bool,
}

impl SessionCapture {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn record_guest_to_proxy(&mut self, timestamp_ns: u64, frame: &[u8]) {
        self.record(timestamp_ns, frame, PacketDirection::Inbound)
            .await;
    }

    pub async fn record_proxy_to_guest(&mut self, timestamp_ns: u64, frame: &[u8]) {
        self.record(timestamp_ns, frame, PacketDirection::Outbound)
            .await;
    }

    async fn record(&mut self, timestamp_ns: u64, frame: &[u8], direction: PacketDirection) {
        if self.disabled {
            return;
        }

        if self.max_bytes != 0 {
            let current = self.bytes_written.load(Ordering::Relaxed);
            if current >= self.max_bytes {
                self.capped = true;
            }

            if self.capped {
                self.metrics.capture_frame_dropped();
                return;
            }

            let block_len = pcapng_enhanced_packet_block_len(frame.len(), Some(direction));
            if current.saturating_add(block_len) > self.max_bytes {
                self.capped = true;
                self.metrics.capture_frame_dropped();
                return;
            }
        }

        let before = self.bytes_written.load(Ordering::Relaxed);
        if let Err(err) = self
            .pcap
            .write_packet(self.iface, timestamp_ns, frame, Some(direction))
            .await
        {
            self.on_error(err);
            return;
        }

        let after = self.bytes_written.load(Ordering::Relaxed);
        self.metrics
            .capture_bytes_written(after.saturating_sub(before));
        self.metrics.capture_frame_written();

        self.maybe_flush().await;
    }

    async fn maybe_flush(&mut self) {
        let Some(interval) = self.flush_interval else {
            return;
        };
        if self.disabled {
            return;
        }
        let current_bytes = self.bytes_written.load(Ordering::Relaxed);
        if current_bytes == self.last_flushed_bytes {
            return;
        }
        let now = tokio::time::Instant::now();
        if now.duration_since(self.last_flush) < interval {
            return;
        }

        if let Err(err) = self.pcap.flush().await {
            self.on_error(err);
            return;
        }
        self.last_flush = now;
        self.last_flushed_bytes = self.bytes_written.load(Ordering::Relaxed);
    }

    fn on_error(&mut self, err: std::io::Error) {
        self.metrics.capture_error();
        self.disabled = true;
        tracing::warn!(path = ?self.path, "capture I/O error (disabling capture): {err}");
    }

    pub async fn close(mut self) -> std::io::Result<()> {
        if let Err(err) = self.pcap.flush().await {
            self.metrics.capture_error();
            return Err(err);
        }
        let mut writer = self.pcap.into_inner();
        match writer.flush().await {
            Ok(()) => Ok(()),
            Err(err) => {
                self.metrics.capture_error();
                Err(err)
            }
        }
    }

    /// Returns the next time at which the capture writer should be flushed.
    ///
    /// This is only used when periodic flushing is enabled and the writer has buffered new data
    /// since the last flush.
    pub(crate) fn next_flush_deadline(&self) -> Option<tokio::time::Instant> {
        if self.disabled {
            return None;
        }
        let interval = self.flush_interval?;
        let current_bytes = self.bytes_written.load(Ordering::Relaxed);
        if current_bytes == self.last_flushed_bytes {
            return None;
        }
        self.last_flush.checked_add(interval)
    }

    pub(crate) async fn flush_if_due(&mut self) {
        self.maybe_flush().await;
    }
}

struct CountingWriter<W> {
    inner: W,
    bytes_written: Arc<AtomicU64>,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, bytes_written: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            bytes_written,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for CountingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let written = match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(n)) => n,
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
        };
        self.bytes_written
            .fetch_add(written as u64, Ordering::Relaxed);
        Poll::Ready(Ok(written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn pcapng_enhanced_packet_block_len(payload_len: usize, direction: Option<PacketDirection>) -> u64 {
    // Enhanced Packet Block:
    // - 12 bytes: block type + block total length + block total length trailer
    // - 20 bytes fixed header in the block body
    // - payload padded to 32-bit
    // - options (end-of-options + optional epb_flags for direction)
    let payload_pad = (4 - (payload_len % 4)) % 4;
    let body_len = 20usize
        .saturating_add(payload_len)
        .saturating_add(payload_pad);
    let opts_len = if direction.is_some() { 12 } else { 4 };
    12u64
        .saturating_add(body_len as u64)
        .saturating_add(opts_len as u64)
}

fn now_ms() -> u64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    ms.min(u64::MAX as u128) as u64
}

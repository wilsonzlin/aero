use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::e1000::E1000Device;
use super::NetworkBackend;
use super::stack::dns::DnsUpstream;
use super::stack::{NetConfig, NetCounters, NetworkStack, ProxyAction, ProxyEvent, StackOutput};
use crate::io::virtio::devices::net::VirtioNetDevice;
use crate::io::virtio::vio_core::VirtQueueError;
use memory::GuestMemory;

pub mod pcapng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDirection {
    GuestTx,
    GuestRx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyDirection {
    GuestToRemote,
    RemoteToGuest,
}

pub trait NetTraceRedactor: Send + Sync {
    fn redact_ethernet(&self, direction: FrameDirection, frame: &[u8]) -> Option<Vec<u8>> {
        let _ = direction;
        Some(frame.to_vec())
    }

    fn redact_tcp_proxy(
        &self,
        direction: ProxyDirection,
        connection_id: u64,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, connection_id);
        Some(data.to_vec())
    }
}

#[derive(Clone)]
pub struct NetTraceConfig {
    pub capture_ethernet: bool,
    pub capture_tcp_proxy: bool,
    pub redactor: Option<Arc<dyn NetTraceRedactor>>,
}

impl Default for NetTraceConfig {
    fn default() -> Self {
        Self {
            capture_ethernet: true,
            capture_tcp_proxy: false,
            redactor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TraceRecord {
    Ethernet {
        timestamp_ns: u64,
        direction: FrameDirection,
        frame: Vec<u8>,
    },
    TcpProxy {
        timestamp_ns: u64,
        direction: ProxyDirection,
        connection_id: u64,
        data: Vec<u8>,
    },
}

pub struct NetTracer {
    enabled: AtomicBool,
    cfg: NetTraceConfig,
    records: Mutex<Vec<TraceRecord>>,
}

impl NetTracer {
    pub fn new(cfg: NetTraceConfig) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            cfg,
            records: Mutex::new(Vec::new()),
        }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn clear(&self) {
        self.records.lock().expect("net trace lock poisoned").clear();
    }

    pub fn record_ethernet(&self, direction: FrameDirection, frame: &[u8]) {
        let ts = now_unix_timestamp_ns();
        self.record_ethernet_at(ts, direction, frame);
    }

    pub fn record_ethernet_at(&self, timestamp_ns: u64, direction: FrameDirection, frame: &[u8]) {
        if !self.is_enabled() || !self.cfg.capture_ethernet {
            return;
        }

        let frame = match &self.cfg.redactor {
            Some(redactor) => match redactor.redact_ethernet(direction, frame) {
                Some(frame) => frame,
                None => return,
            },
            None => frame.to_vec(),
        };

        self.records.lock().expect("net trace lock poisoned").push(
            TraceRecord::Ethernet {
                timestamp_ns,
                direction,
                frame,
            },
        );
    }

    pub fn record_tcp_proxy(&self, direction: ProxyDirection, connection_id: u64, data: &[u8]) {
        let ts = now_unix_timestamp_ns();
        self.record_tcp_proxy_at(ts, direction, connection_id, data);
    }

    pub fn record_tcp_proxy_at(
        &self,
        timestamp_ns: u64,
        direction: ProxyDirection,
        connection_id: u64,
        data: &[u8],
    ) {
        if !self.is_enabled() || !self.cfg.capture_tcp_proxy {
            return;
        }

        let data = match &self.cfg.redactor {
            Some(redactor) => match redactor.redact_tcp_proxy(direction, connection_id, data) {
                Some(data) => data,
                None => return,
            },
            None => data.to_vec(),
        };

        self.records.lock().expect("net trace lock poisoned").push(
            TraceRecord::TcpProxy {
                timestamp_ns,
                direction,
                connection_id,
                data,
            },
        );
    }

    pub fn export_pcapng(&self) -> Vec<u8> {
        self.export_pcapng_inner(false)
    }

    pub fn take_pcapng(&self) -> Vec<u8> {
        self.export_pcapng_inner(true)
    }

    fn export_pcapng_inner(&self, drain: bool) -> Vec<u8> {
        let records = {
            let mut guard = self.records.lock().expect("net trace lock poisoned");
            if drain {
                std::mem::take(&mut *guard)
            } else {
                guard.clone()
            }
        };

        let mut writer = pcapng::PcapngWriter::new("aero");
        let eth_if = writer.add_interface(pcapng::LinkType::Ethernet, "guest-eth0");
        let tcp_proxy_if = if records.iter().any(|r| matches!(r, TraceRecord::TcpProxy { .. })) {
            Some(writer.add_interface(pcapng::LinkType::User0, "tcp-proxy"))
        } else {
            None
        };

        for record in records {
            match record {
                TraceRecord::Ethernet {
                    timestamp_ns,
                    direction,
                    frame,
                } => {
                    let pkt_dir = match direction {
                        FrameDirection::GuestTx => pcapng::PacketDirection::Outbound,
                        FrameDirection::GuestRx => pcapng::PacketDirection::Inbound,
                    };

                    writer.write_packet(eth_if, timestamp_ns, &frame, Some(pkt_dir));
                }
                TraceRecord::TcpProxy {
                    timestamp_ns,
                    direction,
                    connection_id,
                    data,
                } => {
                    let Some(tcp_proxy_if) = tcp_proxy_if else {
                        continue;
                    };

                    let pkt_dir = match direction {
                        ProxyDirection::GuestToRemote => pcapng::PacketDirection::Outbound,
                        ProxyDirection::RemoteToGuest => pcapng::PacketDirection::Inbound,
                    };

                    let pseudo = tcp_proxy_pseudo_packet(connection_id, direction, &data);
                    writer.write_packet(tcp_proxy_if, timestamp_ns, &pseudo, Some(pkt_dir));
                }
            }
        }

        writer.into_bytes()
    }
}

pub struct TracedNetworkStack<U: DnsUpstream> {
    tracer: Arc<NetTracer>,
    inner: NetworkStack<U>,
}

impl<U: DnsUpstream> TracedNetworkStack<U> {
    pub fn new(tracer: Arc<NetTracer>, cfg: NetConfig, dns_upstream: U) -> Self {
        Self {
            tracer,
            inner: NetworkStack::new(cfg, dns_upstream),
        }
    }

    pub fn tracer(&self) -> &NetTracer {
        &self.tracer
    }

    pub fn inner(&self) -> &NetworkStack<U> {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut NetworkStack<U> {
        &mut self.inner
    }

    pub fn into_inner(self) -> NetworkStack<U> {
        self.inner
    }

    pub fn config(&self) -> &NetConfig {
        self.inner.config()
    }

    pub fn counters(&self) -> NetCounters {
        self.inner.counters()
    }

    pub fn process_frame_from_guest(&mut self, frame: &[u8]) -> StackOutput {
        self.tracer.record_ethernet(FrameDirection::GuestTx, frame);
        let out = self.inner.process_frame_from_guest(frame);
        self.record_stack_output(&out);
        out
    }

    pub fn process_proxy_event(&mut self, event: ProxyEvent) -> StackOutput {
        self.record_proxy_event(&event);
        let out = self.inner.process_proxy_event(event);
        self.record_stack_output(&out);
        out
    }

    fn record_stack_output(&self, out: &StackOutput) {
        for frame in &out.frames_to_guest {
            self.tracer.record_ethernet(FrameDirection::GuestRx, frame);
        }

        for action in &out.proxy_actions {
            self.record_proxy_action(action);
        }
    }

    fn record_proxy_action(&self, action: &ProxyAction) {
        match action {
            ProxyAction::TcpSend { conn_id, data } => {
                self.tracer
                    .record_tcp_proxy(ProxyDirection::GuestToRemote, *conn_id, data);
            }
            ProxyAction::TcpConnect { .. } | ProxyAction::TcpClose { .. } | ProxyAction::UdpSend { .. } => {}
        }
    }

    fn record_proxy_event(&self, event: &ProxyEvent) {
        match event {
            ProxyEvent::TcpData { conn_id, data } => {
                self.tracer
                    .record_tcp_proxy(ProxyDirection::RemoteToGuest, *conn_id, data);
            }
            ProxyEvent::TcpConnected { .. }
            | ProxyEvent::TcpConnectFailed { .. }
            | ProxyEvent::TcpClosed { .. }
            | ProxyEvent::UdpData { .. } => {}
        }
    }
}

pub struct TracingBackend<'a, B> {
    tracer: &'a NetTracer,
    inner: &'a mut B,
}

impl<'a, B> TracingBackend<'a, B> {
    pub fn new(tracer: &'a NetTracer, inner: &'a mut B) -> Self {
        Self { tracer, inner }
    }
}

impl<B: NetworkBackend> NetworkBackend for TracingBackend<'_, B> {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.tracer.record_ethernet(FrameDirection::GuestTx, &frame);
        self.inner.transmit(frame);
    }
}

pub trait E1000DeviceTraceExt {
    fn enqueue_rx_frame_traced(&mut self, tracer: &NetTracer, frame: Vec<u8>);
}

impl E1000DeviceTraceExt for E1000Device {
    fn enqueue_rx_frame_traced(&mut self, tracer: &NetTracer, frame: Vec<u8>) {
        tracer.record_ethernet(FrameDirection::GuestRx, &frame);
        self.enqueue_rx_frame(frame);
    }
}

pub trait VirtioNetDeviceTraceExt {
    fn inject_rx_frame_traced(
        &mut self,
        tracer: &NetTracer,
        mem: &mut impl GuestMemory,
        frame: &[u8],
    ) -> Result<bool, VirtQueueError>;
}

impl VirtioNetDeviceTraceExt for VirtioNetDevice {
    fn inject_rx_frame_traced(
        &mut self,
        tracer: &NetTracer,
        mem: &mut impl GuestMemory,
        frame: &[u8],
    ) -> Result<bool, VirtQueueError> {
        tracer.record_ethernet(FrameDirection::GuestRx, frame);
        self.inject_rx_frame(mem, frame)
    }
}

fn now_unix_timestamp_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => duration_to_ns(dur),
        Err(err) => duration_to_ns(err.duration()),
    }
}

fn duration_to_ns(dur: Duration) -> u64 {
    dur.as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(dur.subsec_nanos()))
}

fn tcp_proxy_pseudo_packet(connection_id: u64, direction: ProxyDirection, payload: &[u8]) -> Vec<u8> {
    const MAGIC: [u8; 4] = *b"ATCP";

    let dir = match direction {
        ProxyDirection::GuestToRemote => 0u8,
        ProxyDirection::RemoteToGuest => 1u8,
    };

    let mut buf = Vec::with_capacity(16 + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(dir);
    buf.extend_from_slice(&[0u8; 3]);
    buf.extend_from_slice(&connection_id.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

#[cfg(not(target_arch = "wasm32"))]
pub struct CaptureArtifactOnPanic<'a> {
    tracer: &'a NetTracer,
    path: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a> CaptureArtifactOnPanic<'a> {
    pub fn new(tracer: &'a NetTracer, path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            tracer,
            path: path.into(),
        }
    }

    pub fn for_test(tracer: &'a NetTracer, test_name: &str) -> Self {
        Self::new(tracer, default_capture_artifact_path(test_name))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for CaptureArtifactOnPanic<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }

        let bytes = self.tracer.export_pcapng();
        if bytes.is_empty() {
            return;
        }

        if let Some(parent) = self.path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!("failed to create capture artifact directory {parent:?}: {err}");
                return;
            }
        }

        if let Err(err) = std::fs::write(&self.path, bytes) {
            eprintln!("failed to write capture artifact {:?}: {err}", self.path);
            return;
        }

        eprintln!("wrote network capture artifact to {:?}", self.path);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn default_capture_artifact_path(test_name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("target")
        .join("nt-test-artifacts")
        .join(format!("{test_name}.pcapng"))
}

#[cfg(any(debug_assertions, feature = "net-trace"))]
pub fn net_tracing_available() -> bool {
    true
}

#[cfg(not(any(debug_assertions, feature = "net-trace")))]
pub fn net_tracing_available() -> bool {
    false
}

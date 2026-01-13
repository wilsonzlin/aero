use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::stack::{
    Action, DnsResolved, Millis, NetStackBackend, StackConfig, TcpProxyEvent, UdpProxyEvent,
    UdpTransport,
};
use super::NetworkBackend;
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
        connection_id: u32,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, connection_id);
        Some(data.to_vec())
    }

    fn redact_udp_proxy(
        &self,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, transport, remote_ip, src_port, dst_port);
        Some(data.to_vec())
    }
}

fn copy_prefix_bytes(data: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    // Avoid allocating the full payload and then truncating (which could capture sensitive data
    // and/or blow up memory usage when tracing untrusted guests). By reserving exactly the
    // truncated length, we ensure the allocation is bounded by `max_bytes`.
    let n = data.len().min(max_bytes);
    let mut out = Vec::new();
    out.try_reserve_exact(n).ok()?;
    out.extend_from_slice(&data[..n]);
    Some(out)
}

/// Redactor that truncates captured payloads to a fixed maximum size.
///
/// This is the safest "drop-in" option: it keeps the first `N` bytes of each payload (which is
/// usually enough to retain protocol metadata) while bounding memory usage and reducing the chance
/// of capturing full sensitive payloads.
///
/// Enable via [`NetTraceConfig::redactor`].
#[derive(Debug, Clone)]
pub struct TruncateRedactor {
    pub max_ethernet_bytes: usize,
    pub max_tcp_proxy_bytes: usize,
    pub max_udp_proxy_bytes: usize,
}

impl TruncateRedactor {
    pub fn new(max_ethernet_bytes: usize, max_tcp_proxy_bytes: usize, max_udp_proxy_bytes: usize) -> Self {
        Self {
            max_ethernet_bytes,
            max_tcp_proxy_bytes,
            max_udp_proxy_bytes,
        }
    }
}

impl NetTraceRedactor for TruncateRedactor {
    fn redact_ethernet(&self, direction: FrameDirection, frame: &[u8]) -> Option<Vec<u8>> {
        let _ = direction;
        copy_prefix_bytes(frame, self.max_ethernet_bytes)
    }

    fn redact_tcp_proxy(
        &self,
        direction: ProxyDirection,
        connection_id: u32,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, connection_id);
        copy_prefix_bytes(data, self.max_tcp_proxy_bytes)
    }

    fn redact_udp_proxy(
        &self,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, transport, remote_ip, src_port, dst_port);
        copy_prefix_bytes(data, self.max_udp_proxy_bytes)
    }
}

/// Redactor that keeps only L2+L3+L4 headers for Ethernet frames (when parseable) and drops all
/// proxy payloads.
///
/// This is a more aggressive option than [`TruncateRedactor`]: it is intended to capture enough
/// information for flow-level debugging (addresses / ports / flags), but without application-layer
/// bytes.
///
/// Enable via [`NetTraceConfig::redactor`].
#[derive(Debug, Clone)]
pub struct HeadersOnlyRedactor {
    /// Hard upper bound for captured Ethernet bytes.
    pub max_ethernet_bytes: usize,
}

impl HeadersOnlyRedactor {
    pub fn new(max_ethernet_bytes: usize) -> Self {
        Self { max_ethernet_bytes }
    }
}

impl NetTraceRedactor for HeadersOnlyRedactor {
    fn redact_ethernet(&self, direction: FrameDirection, frame: &[u8]) -> Option<Vec<u8>> {
        let _ = direction;
        let header_len = ethernet_l2_l3_l4_header_len(frame)?;
        copy_prefix_bytes(frame, header_len.min(self.max_ethernet_bytes))
    }

    fn redact_tcp_proxy(
        &self,
        direction: ProxyDirection,
        connection_id: u32,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, connection_id, data);
        None
    }

    fn redact_udp_proxy(
        &self,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let _ = (direction, transport, remote_ip, src_port, dst_port, data);
        None
    }
}

fn ethernet_l2_l3_l4_header_len(frame: &[u8]) -> Option<usize> {
    // Ethernet II base header: 6 dst + 6 src + 2 ethertype.
    const ETH_HDR_LEN: usize = 14;
    if frame.len() < ETH_HDR_LEN {
        return None;
    }

    let mut ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let mut l3_off = ETH_HDR_LEN;

    // Optional 802.1Q / 802.1ad VLAN tag. Keep parsing simple and support a single tag; if the
    // frame is more exotic, we conservatively drop it.
    if ethertype == 0x8100 || ethertype == 0x88a8 {
        if frame.len() < l3_off + 4 {
            return None;
        }
        ethertype = u16::from_be_bytes([frame[l3_off + 2], frame[l3_off + 3]]);
        l3_off += 4;
    }

    match ethertype {
        // IPv4
        0x0800 => ipv4_l3_l4_header_len(frame, l3_off),
        // ARP (no L4, but still a "headers only" view)
        0x0806 => arp_header_len(frame, l3_off),
        _ => None,
    }
}

fn ipv4_l3_l4_header_len(frame: &[u8], ip_off: usize) -> Option<usize> {
    // Minimum IPv4 header is 20 bytes.
    if frame.len() < ip_off + 20 {
        return None;
    }
    let ver_ihl = frame[ip_off];
    let version = ver_ihl >> 4;
    if version != 4 {
        return None;
    }
    let ihl = (ver_ihl & 0x0f) as usize;
    let ip_header_len = ihl.checked_mul(4)?;
    if ip_header_len < 20 || ip_header_len > 60 {
        return None;
    }
    if frame.len() < ip_off + ip_header_len {
        return None;
    }

    let proto = frame[ip_off + 9];
    let l4_off = ip_off + ip_header_len;
    match proto {
        // TCP
        6 => tcp_header_len(frame, l4_off),
        // UDP
        17 => {
            if frame.len() < l4_off + 8 {
                return None;
            }
            Some(l4_off + 8)
        }
        // ICMPv4 (8 byte header)
        1 => {
            if frame.len() < l4_off + 8 {
                return None;
            }
            Some(l4_off + 8)
        }
        _ => None,
    }
}

fn tcp_header_len(frame: &[u8], tcp_off: usize) -> Option<usize> {
    if frame.len() < tcp_off + 20 {
        return None;
    }
    let data_offset_words = frame[tcp_off + 12] >> 4;
    let tcp_header_len = (data_offset_words as usize).checked_mul(4)?;
    if tcp_header_len < 20 || tcp_header_len > 60 {
        return None;
    }
    if frame.len() < tcp_off + tcp_header_len {
        return None;
    }
    Some(tcp_off + tcp_header_len)
}

fn arp_header_len(frame: &[u8], arp_off: usize) -> Option<usize> {
    if frame.len() < arp_off + 8 {
        return None;
    }
    let hw_len = frame[arp_off + 4] as usize;
    let proto_len = frame[arp_off + 5] as usize;
    let total = 8usize
        .checked_add(hw_len.checked_mul(2)?)?
        .checked_add(proto_len.checked_mul(2)?)?;
    if frame.len() < arp_off + total {
        return None;
    }
    Some(arp_off + total)
}

#[derive(Clone)]
pub struct NetTraceConfig {
    /// Hard cap on total captured payload bytes (not including PCAPNG overhead).
    ///
    /// - When exceeded, new frames/records are dropped.
    /// - `0` disables capture (all records are dropped).
    pub max_bytes: usize,
    pub capture_ethernet: bool,
    pub capture_tcp_proxy: bool,
    pub capture_udp_proxy: bool,
    /// Optional redactor that is applied to payload bytes before they are stored in memory.
    ///
    /// Returning `None` from a redactor method drops the record entirely.
    ///
    /// Enable a built-in redactor like this:
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use emulator::io::net::trace::{NetTraceConfig, TruncateRedactor};
    ///
    /// let cfg = NetTraceConfig {
    ///     capture_ethernet: true,
    ///     capture_tcp_proxy: true,
    ///     capture_udp_proxy: true,
    ///     redactor: Some(Arc::new(TruncateRedactor {
    ///         max_ethernet_bytes: 128,
    ///         max_tcp_proxy_bytes: 256,
    ///         max_udp_proxy_bytes: 256,
    ///     })),
    /// };
    /// ```
    pub redactor: Option<Arc<dyn NetTraceRedactor>>,
}

impl Default for NetTraceConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            capture_ethernet: true,
            capture_tcp_proxy: false,
            capture_udp_proxy: false,
            redactor: None,
        }
    }
}

const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;
const PROXY_PSEUDO_HEADER_LEN: usize = 16;

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
        connection_id: u32,
        data: Vec<u8>,
    },
    UdpProxy {
        timestamp_ns: u64,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetTraceStats {
    pub enabled: bool,
    pub records: usize,
    pub bytes: usize,
    pub dropped_records: u64,
    pub dropped_bytes: u64,
}

#[derive(Debug, Default)]
struct TraceState {
    records: Vec<TraceRecord>,
    bytes: usize,
    dropped_records: u64,
    dropped_bytes: u64,
}

pub struct NetTracer {
    enabled: AtomicBool,
    cfg: NetTraceConfig,
    state: Mutex<TraceState>,
}

impl NetTracer {
    pub fn new(cfg: NetTraceConfig) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            cfg,
            state: Mutex::new(TraceState::default()),
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
        let mut state = self.state.lock().expect("net trace lock poisoned");
        state.records.clear();
        state.bytes = 0;
        state.dropped_records = 0;
        state.dropped_bytes = 0;
    }

    pub fn stats(&self) -> NetTraceStats {
        let state = self.state.lock().expect("net trace lock poisoned");
        NetTraceStats {
            enabled: self.is_enabled(),
            records: state.records.len(),
            bytes: state.bytes,
            dropped_records: state.dropped_records,
            dropped_bytes: state.dropped_bytes,
        }
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

        let len = frame.len();
        let mut state = self.state.lock().expect("net trace lock poisoned");
        if len > self.cfg.max_bytes || state.bytes.saturating_add(len) > self.cfg.max_bytes {
            state.dropped_records = state.dropped_records.saturating_add(1);
            state.dropped_bytes = state.dropped_bytes.saturating_add(len as u64);
            return;
        }
        state.records.push(TraceRecord::Ethernet {
            timestamp_ns,
            direction,
            frame,
        });
        state.bytes = state.bytes.saturating_add(len);
    }

    pub fn record_tcp_proxy(&self, direction: ProxyDirection, connection_id: u32, data: &[u8]) {
        let ts = now_unix_timestamp_ns();
        self.record_tcp_proxy_at(ts, direction, connection_id, data);
    }

    pub fn record_tcp_proxy_at(
        &self,
        timestamp_ns: u64,
        direction: ProxyDirection,
        connection_id: u32,
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

        let len = PROXY_PSEUDO_HEADER_LEN.saturating_add(data.len());
        let mut state = self.state.lock().expect("net trace lock poisoned");
        if len > self.cfg.max_bytes || state.bytes.saturating_add(len) > self.cfg.max_bytes {
            state.dropped_records = state.dropped_records.saturating_add(1);
            state.dropped_bytes = state.dropped_bytes.saturating_add(len as u64);
            return;
        }
        state.records.push(TraceRecord::TcpProxy {
            timestamp_ns,
            direction,
            connection_id,
            data,
        });
        state.bytes = state.bytes.saturating_add(len);
    }

    pub fn record_udp_proxy(
        &self,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: &[u8],
    ) {
        let ts = now_unix_timestamp_ns();
        self.record_udp_proxy_at(
            ts,
            direction,
            transport,
            remote_ip,
            (src_port, dst_port),
            data,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_udp_proxy_at(
        &self,
        timestamp_ns: u64,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        ports: (u16, u16),
        data: &[u8],
    ) {
        if !self.is_enabled() || !self.cfg.capture_udp_proxy {
            return;
        }

        let (src_port, dst_port) = ports;
        let data = match &self.cfg.redactor {
            Some(redactor) => match redactor.redact_udp_proxy(
                direction,
                transport.clone(),
                remote_ip,
                src_port,
                dst_port,
                data,
            ) {
                Some(data) => data,
                None => return,
            },
            None => data.to_vec(),
        };

        let len = PROXY_PSEUDO_HEADER_LEN.saturating_add(data.len());
        let mut state = self.state.lock().expect("net trace lock poisoned");
        if len > self.cfg.max_bytes || state.bytes.saturating_add(len) > self.cfg.max_bytes {
            state.dropped_records = state.dropped_records.saturating_add(1);
            state.dropped_bytes = state.dropped_bytes.saturating_add(len as u64);
            return;
        }
        state.records.push(TraceRecord::UdpProxy {
            timestamp_ns,
            direction,
            transport,
            remote_ip,
            src_port,
            dst_port,
            data,
        });
        state.bytes = state.bytes.saturating_add(len);
    }

    pub fn export_pcapng(&self) -> Vec<u8> {
        self.export_pcapng_inner(false)
    }

    pub fn take_pcapng(&self) -> Vec<u8> {
        self.export_pcapng_inner(true)
    }

    fn export_pcapng_inner(&self, drain: bool) -> Vec<u8> {
        let records = {
            let mut guard = self.state.lock().expect("net trace lock poisoned");
            if drain {
                guard.bytes = 0;
                std::mem::take(&mut guard.records)
            } else {
                guard.records.clone()
            }
        };

        let mut writer = pcapng::PcapngWriter::new("aero");
        let eth_if = writer.add_interface(pcapng::LinkType::Ethernet, "guest-eth0");
        let tcp_proxy_if = records
            .iter()
            .any(|r| matches!(r, TraceRecord::TcpProxy { .. }))
            .then(|| writer.add_interface(pcapng::LinkType::User0, "tcp-proxy"));
        let udp_proxy_if = records
            .iter()
            .any(|r| matches!(r, TraceRecord::UdpProxy { .. }))
            .then(|| writer.add_interface(pcapng::LinkType::User1, "udp-proxy"));

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
                TraceRecord::UdpProxy {
                    timestamp_ns,
                    direction,
                    transport,
                    remote_ip,
                    src_port,
                    dst_port,
                    data,
                } => {
                    let Some(udp_proxy_if) = udp_proxy_if else {
                        continue;
                    };

                    let pkt_dir = match direction {
                        ProxyDirection::GuestToRemote => pcapng::PacketDirection::Outbound,
                        ProxyDirection::RemoteToGuest => pcapng::PacketDirection::Inbound,
                    };

                    let pseudo = udp_proxy_pseudo_packet(
                        transport, remote_ip, src_port, dst_port, direction, &data,
                    );
                    writer.write_packet(udp_proxy_if, timestamp_ns, &pseudo, Some(pkt_dir));
                }
            }
        }

        writer.into_bytes()
    }
}

pub struct TracedNetworkStack {
    tracer: Arc<NetTracer>,
    inner: NetStackBackend,
}

impl TracedNetworkStack {
    pub fn new(tracer: Arc<NetTracer>, cfg: StackConfig) -> Self {
        Self::from_backend(tracer, NetStackBackend::new(cfg))
    }

    pub fn from_backend(tracer: Arc<NetTracer>, inner: NetStackBackend) -> Self {
        Self { tracer, inner }
    }

    pub fn tracer(&self) -> &NetTracer {
        &self.tracer
    }

    pub fn inner(&self) -> &NetStackBackend {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut NetStackBackend {
        &mut self.inner
    }

    pub fn into_inner(self) -> NetStackBackend {
        self.inner
    }

    pub fn now_ms(&self) -> Millis {
        self.inner.now_ms()
    }

    pub fn transmit_at(&mut self, frame: Vec<u8>, now_ms: Millis) {
        self.tracer.record_ethernet(FrameDirection::GuestTx, &frame);
        self.inner.transmit_at(frame, now_ms);
    }

    pub fn push_tcp_event(&mut self, event: TcpProxyEvent, now_ms: Millis) {
        self.record_tcp_proxy_event(&event);
        self.inner.push_tcp_event(event, now_ms);
    }

    pub fn push_udp_event(&mut self, event: UdpProxyEvent, now_ms: Millis) {
        self.record_udp_proxy_event(&event);
        self.inner.push_udp_event(event, now_ms);
    }

    pub fn push_dns_resolved(&mut self, resolved: DnsResolved, now_ms: Millis) {
        self.inner.push_dns_resolved(resolved, now_ms);
    }

    pub fn drain_actions(&mut self) -> Vec<Action> {
        let actions = self.inner.drain_actions();
        for action in &actions {
            self.record_action(action);
        }
        actions
    }

    pub fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        let frames = self.inner.drain_frames();
        for frame in &frames {
            self.tracer.record_ethernet(FrameDirection::GuestRx, frame);
        }
        frames
    }

    fn record_action(&self, action: &Action) {
        match action {
            Action::TcpProxySend {
                connection_id,
                data,
            } => {
                self.tracer
                    .record_tcp_proxy(ProxyDirection::GuestToRemote, *connection_id, data);
            }
            Action::UdpProxySend {
                transport,
                src_port,
                dst_ip,
                dst_port,
                data,
            } => {
                self.tracer.record_udp_proxy(
                    ProxyDirection::GuestToRemote,
                    transport.clone(),
                    *dst_ip,
                    *src_port,
                    *dst_port,
                    data,
                );
            }
            Action::EmitFrame(_)
            | Action::TcpProxyConnect { .. }
            | Action::TcpProxyClose { .. }
            | Action::DnsResolve { .. } => {}
        }
    }

    fn record_tcp_proxy_event(&self, event: &TcpProxyEvent) {
        if let TcpProxyEvent::Data {
            connection_id,
            data,
        } = event
        {
            self.tracer
                .record_tcp_proxy(ProxyDirection::RemoteToGuest, *connection_id, data);
        }
    }

    fn record_udp_proxy_event(&self, event: &UdpProxyEvent) {
        self.tracer.record_udp_proxy(
            ProxyDirection::RemoteToGuest,
            self.udp_transport_hint(),
            event.src_ip,
            event.src_port,
            event.dst_port,
            &event.data,
        );
    }

    fn udp_transport_hint(&self) -> UdpTransport {
        if self.inner.stack().config().webrtc_udp {
            UdpTransport::WebRtc
        } else {
            UdpTransport::Proxy
        }
    }
}

impl NetworkBackend for TracedNetworkStack {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.tracer.record_ethernet(FrameDirection::GuestTx, &frame);
        self.inner.transmit(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        let frame = NetworkBackend::poll_receive(&mut self.inner)?;
        self.tracer.record_ethernet(FrameDirection::GuestRx, &frame);
        Some(frame)
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

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        let frame = self.inner.poll_receive()?;
        self.tracer.record_ethernet(FrameDirection::GuestRx, &frame);
        Some(frame)
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

fn tcp_proxy_pseudo_packet(
    connection_id: u32,
    direction: ProxyDirection,
    payload: &[u8],
) -> Vec<u8> {
    const MAGIC: [u8; 4] = *b"ATCP";

    let dir = match direction {
        ProxyDirection::GuestToRemote => 0u8,
        ProxyDirection::RemoteToGuest => 1u8,
    };

    let mut buf = Vec::with_capacity(PROXY_PSEUDO_HEADER_LEN + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(dir);
    buf.extend_from_slice(&[0u8; 3]);
    buf.extend_from_slice(&(connection_id as u64).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn udp_proxy_pseudo_packet(
    transport: UdpTransport,
    remote_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    direction: ProxyDirection,
    payload: &[u8],
) -> Vec<u8> {
    const MAGIC: [u8; 4] = *b"AUDP";

    let dir = match direction {
        ProxyDirection::GuestToRemote => 0u8,
        ProxyDirection::RemoteToGuest => 1u8,
    };
    let transport = match transport {
        UdpTransport::WebRtc => 0u8,
        UdpTransport::Proxy => 1u8,
    };

    let mut buf = Vec::with_capacity(PROXY_PSEUDO_HEADER_LEN + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(dir);
    buf.push(transport);
    buf.extend_from_slice(&[0u8; 2]);
    buf.extend_from_slice(&remote_ip.octets());
    buf.extend_from_slice(&src_port.to_le_bytes());
    buf.extend_from_slice(&dst_port.to_le_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn truncate_redactor_truncates_each_record_type() {
        let tracer = NetTracer::new(NetTraceConfig {
            max_bytes: DEFAULT_MAX_BYTES,
            capture_ethernet: true,
            capture_tcp_proxy: true,
            capture_udp_proxy: true,
            redactor: Some(Arc::new(TruncateRedactor {
                max_ethernet_bytes: 4,
                max_tcp_proxy_bytes: 3,
                max_udp_proxy_bytes: 2,
            })),
        });
        tracer.enable();

        tracer.record_ethernet_at(1, FrameDirection::GuestTx, &[1, 2, 3, 4, 5, 6]);
        tracer.record_tcp_proxy_at(2, ProxyDirection::GuestToRemote, 99, &[10, 11, 12, 13]);
        tracer.record_udp_proxy_at(
            3,
            ProxyDirection::RemoteToGuest,
            UdpTransport::Proxy,
            Ipv4Addr::new(1, 2, 3, 4),
            (1000, 2000),
            &[0xff, 0xfe, 0xfd],
        );

        let guard = tracer.state.lock().expect("net trace lock poisoned");
        assert_eq!(guard.records.len(), 3);

        match &guard.records[0] {
            TraceRecord::Ethernet { frame, .. } => assert_eq!(frame.as_slice(), &[1, 2, 3, 4]),
            other => panic!("unexpected record: {other:?}"),
        }
        match &guard.records[1] {
            TraceRecord::TcpProxy { data, .. } => assert_eq!(data.as_slice(), &[10, 11, 12]),
            other => panic!("unexpected record: {other:?}"),
        }
        match &guard.records[2] {
            TraceRecord::UdpProxy { data, .. } => assert_eq!(data.as_slice(), &[0xff, 0xfe]),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn redactor_none_drops_records() {
        #[derive(Debug)]
        struct DropAll;

        impl NetTraceRedactor for DropAll {
            fn redact_ethernet(&self, _: FrameDirection, _: &[u8]) -> Option<Vec<u8>> {
                None
            }
            fn redact_tcp_proxy(
                &self,
                _: ProxyDirection,
                _: u32,
                _: &[u8],
            ) -> Option<Vec<u8>> {
                None
            }
            fn redact_udp_proxy(
                &self,
                _: ProxyDirection,
                _: UdpTransport,
                _: Ipv4Addr,
                _: u16,
                _: u16,
                _: &[u8],
            ) -> Option<Vec<u8>> {
                None
            }
        }

        let tracer = NetTracer::new(NetTraceConfig {
            max_bytes: DEFAULT_MAX_BYTES,
            capture_ethernet: true,
            capture_tcp_proxy: true,
            capture_udp_proxy: true,
            redactor: Some(Arc::new(DropAll)),
        });
        tracer.enable();

        tracer.record_ethernet_at(1, FrameDirection::GuestTx, &[1, 2, 3]);
        tracer.record_tcp_proxy_at(2, ProxyDirection::GuestToRemote, 1, &[4, 5, 6]);
        tracer.record_udp_proxy_at(
            3,
            ProxyDirection::GuestToRemote,
            UdpTransport::WebRtc,
            Ipv4Addr::new(8, 8, 8, 8),
            (1234, 53),
            &[7, 8, 9],
        );

        assert!(
            tracer
                .state
                .lock()
                .expect("net trace lock poisoned")
                .records
                .is_empty(),
            "records should be dropped when redactor returns None"
        );
    }
}

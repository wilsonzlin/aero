#![forbid(unsafe_code)]

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// `std::time::SystemTime::now()` can panic on `wasm32-unknown-unknown` in some configurations.
// Use `web-time`'s `SystemTime` for wasm targets so network tracing can run in browser/Node
// environments.
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(target_arch = "wasm32")]
use web_time::{SystemTime, UNIX_EPOCH};

use aero_net_backend::NetworkBackend;
use aero_net_stack::{
    Action, DnsResolved, Millis, NetStackBackend, StackConfig, TcpProxyEvent, UdpProxyEvent,
    UdpTransport,
};

pub mod pcapng;

// Convenience re-exports so downstream users don't have to reach into `pcapng::`.
pub use pcapng::{LinkType, PcapngWriter};

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
    pub fn new(
        max_ethernet_bytes: usize,
        max_tcp_proxy_bytes: usize,
        max_udp_proxy_bytes: usize,
    ) -> Self {
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

    // Optional 802.1Q / 802.1ad VLAN tags. Support up to two tags (Q-in-Q). If the frame is more
    // exotic, we conservatively drop it.
    for _ in 0..2 {
        if ethertype == 0x8100 || ethertype == 0x88a8 {
            if frame.len() < l3_off + 4 {
                return None;
            }
            ethertype = u16::from_be_bytes([frame[l3_off + 2], frame[l3_off + 3]]);
            l3_off += 4;
        } else {
            break;
        }
    }
    if ethertype == 0x8100 || ethertype == 0x88a8 {
        return None;
    }

    match ethertype {
        // IPv4
        0x0800 => ipv4_l3_l4_header_len(frame, l3_off),
        // IPv6
        0x86dd => ipv6_l3_l4_header_len(frame, l3_off),
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
    if !(20..=60).contains(&ip_header_len) {
        return None;
    }
    if frame.len() < ip_off + ip_header_len {
        return None;
    }

    let proto = frame[ip_off + 9];
    let l4_off = ip_off + ip_header_len;

    // If this is a non-initial IPv4 fragment (fragment offset != 0), we can't reliably parse L4
    // headers without accidentally capturing payload bytes. Keep only the IP header.
    let flags_fragment = u16::from_be_bytes([frame[ip_off + 6], frame[ip_off + 7]]);
    if (flags_fragment & 0x1fff) != 0 {
        return Some(l4_off);
    }

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

fn ipv6_l3_l4_header_len(frame: &[u8], ip_off: usize) -> Option<usize> {
    const IPV6_BASE_HDR_LEN: usize = 40;

    let base_end = ip_off.checked_add(IPV6_BASE_HDR_LEN)?;
    if frame.len() < base_end {
        return None;
    }

    let version = frame[ip_off] >> 4;
    if version != 6 {
        return None;
    }

    let mut next = frame[ip_off + 6];
    let mut off = base_end;

    // Walk extension headers to find the L4 header. We cap the number of iterations to avoid
    // pathological packets causing unbounded work.
    for _ in 0..8 {
        match next {
            // TCP
            6 => return tcp_header_len(frame, off),
            // UDP
            17 => {
                let end = off.checked_add(8)?;
                if frame.len() < end {
                    return None;
                }
                return Some(end);
            }
            // ICMPv6 (minimum 8 bytes for common message types)
            58 => {
                let end = off.checked_add(8)?;
                if frame.len() < end {
                    return None;
                }
                return Some(end);
            }
            // No Next Header
            59 => return Some(off),
            // Fragment header (fixed 8 bytes)
            44 => {
                let end = off.checked_add(8)?;
                if frame.len() < end {
                    return None;
                }

                let next_after = frame[off];
                let flags_fragment = u16::from_be_bytes([frame[off + 2], frame[off + 3]]);
                let fragment_offset = (flags_fragment & 0xfff8) >> 3;

                off = end;
                if fragment_offset != 0 {
                    // Non-initial fragment: don't attempt to parse L4 headers.
                    return Some(off);
                }

                next = next_after;
                continue;
            }
            // Hop-by-Hop Options, Routing, Destination Options.
            0 | 43 | 60 => {
                let min_end = off.checked_add(2)?;
                if frame.len() < min_end {
                    return None;
                }

                let next_after = frame[off];
                let hdr_ext_len = frame[off + 1] as usize;
                let ext_len = hdr_ext_len.checked_add(1)?.checked_mul(8)?;
                let end = off.checked_add(ext_len)?;
                if frame.len() < end {
                    return None;
                }

                off = end;
                next = next_after;
                continue;
            }
            _ => return None,
        }
    }

    None
}

fn tcp_header_len(frame: &[u8], tcp_off: usize) -> Option<usize> {
    if frame.len() < tcp_off + 20 {
        return None;
    }
    let data_offset_words = frame[tcp_off + 12] >> 4;
    let tcp_header_len = (data_offset_words as usize).checked_mul(4)?;
    if !(20..=60).contains(&tcp_header_len) {
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
    /// Hard cap on the number of buffered records.
    ///
    /// - When exceeded, new records are dropped.
    /// - `0` disables capture (all records are dropped).
    pub max_records: usize,
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
    /// use aero_net_trace::{NetTraceConfig, TruncateRedactor};
    ///
    /// let cfg = NetTraceConfig {
    ///     capture_tcp_proxy: true,
    ///     capture_udp_proxy: true,
    ///     redactor: Some(Arc::new(TruncateRedactor {
    ///         max_ethernet_bytes: 128,
    ///         max_tcp_proxy_bytes: 256,
    ///         max_udp_proxy_bytes: 256,
    ///     })),
    ///     ..NetTraceConfig::default()
    /// };
    /// ```
    pub redactor: Option<Arc<dyn NetTraceRedactor>>,
}

impl Default for NetTraceConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_records: DEFAULT_MAX_RECORDS,
            capture_ethernet: true,
            capture_tcp_proxy: false,
            capture_udp_proxy: false,
            redactor: None,
        }
    }
}

const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_RECORDS: usize = 100_000;
const PROXY_PSEUDO_HEADER_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
enum TraceRecord {
    Ethernet {
        timestamp_ns: u64,
        direction: FrameDirection,
        frame: Arc<[u8]>,
    },
    TcpProxy {
        timestamp_ns: u64,
        direction: ProxyDirection,
        connection_id: u32,
        data: Arc<[u8]>,
    },
    UdpProxy {
        timestamp_ns: u64,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: Arc<[u8]>,
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

        let attempted_len = frame.len();
        let strict_precheck = self.cfg.redactor.is_none();
        self.try_push_record(attempted_len, strict_precheck, || {
            let frame = self.redact_ethernet(direction, frame)?;
            let len = frame.len();
            Some((
                len,
                TraceRecord::Ethernet {
                    timestamp_ns,
                    direction,
                    frame,
                },
            ))
        });
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

        let attempted_len = PROXY_PSEUDO_HEADER_LEN.saturating_add(data.len());
        let strict_precheck = self.cfg.redactor.is_none();
        self.try_push_record(attempted_len, strict_precheck, || {
            let data = self.redact_tcp_proxy(direction, connection_id, data)?;
            let len = PROXY_PSEUDO_HEADER_LEN.saturating_add(data.len());
            Some((
                len,
                TraceRecord::TcpProxy {
                    timestamp_ns,
                    direction,
                    connection_id,
                    data,
                },
            ))
        });
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
        let attempted_len = PROXY_PSEUDO_HEADER_LEN.saturating_add(data.len());
        let strict_precheck = self.cfg.redactor.is_none();
        self.try_push_record(attempted_len, strict_precheck, || {
            let data = self.redact_udp_proxy(
                direction,
                transport.clone(),
                remote_ip,
                src_port,
                dst_port,
                data,
            )?;
            let len = PROXY_PSEUDO_HEADER_LEN.saturating_add(data.len());
            Some((
                len,
                TraceRecord::UdpProxy {
                    timestamp_ns,
                    direction,
                    transport,
                    remote_ip,
                    src_port,
                    dst_port,
                    data,
                },
            ))
        });
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
                // `take_pcapng()` drains buffered records and resets the live `bytes` counter, but
                // deliberately keeps the drop counters. This matches the web tracer
                // (`web/src/net/net_tracer.ts`).
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

    fn redact_ethernet(&self, direction: FrameDirection, frame: &[u8]) -> Option<Arc<[u8]>> {
        match &self.cfg.redactor {
            Some(redactor) => redactor
                .redact_ethernet(direction, frame)
                .map(|frame| frame.into()),
            None => Some(Arc::from(frame)),
        }
    }

    fn redact_tcp_proxy(
        &self,
        direction: ProxyDirection,
        connection_id: u32,
        data: &[u8],
    ) -> Option<Arc<[u8]>> {
        match &self.cfg.redactor {
            Some(redactor) => redactor
                .redact_tcp_proxy(direction, connection_id, data)
                .map(|data| data.into()),
            None => Some(Arc::from(data)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn redact_udp_proxy(
        &self,
        direction: ProxyDirection,
        transport: UdpTransport,
        remote_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        data: &[u8],
    ) -> Option<Arc<[u8]>> {
        match &self.cfg.redactor {
            Some(redactor) => redactor
                .redact_udp_proxy(direction, transport, remote_ip, src_port, dst_port, data)
                .map(|data| data.into()),
            None => Some(Arc::from(data)),
        }
    }

    fn try_push_record(
        &self,
        attempted_len: usize,
        strict_precheck: bool,
        make: impl FnOnce() -> Option<(usize, TraceRecord)>,
    ) {
        // Fast path: avoid allocating/storing anything when the cap is already hit.
        {
            let mut state = self.state.lock().expect("net trace lock poisoned");
            if state.records.len() >= self.cfg.max_records || state.bytes >= self.cfg.max_bytes {
                state.dropped_records = state.dropped_records.saturating_add(1);
                state.dropped_bytes = state.dropped_bytes.saturating_add(attempted_len as u64);
                return;
            }
            if strict_precheck && would_exceed_bytes(state.bytes, self.cfg.max_bytes, attempted_len)
            {
                state.dropped_records = state.dropped_records.saturating_add(1);
                state.dropped_bytes = state.dropped_bytes.saturating_add(attempted_len as u64);
                return;
            }
        }

        let Some((len, record)) = make() else {
            // Redacted out.
            return;
        };

        let mut state = self.state.lock().expect("net trace lock poisoned");
        if should_drop_record(&state, &self.cfg, len) {
            state.dropped_records = state.dropped_records.saturating_add(1);
            state.dropped_bytes = state.dropped_bytes.saturating_add(len as u64);
            return;
        }

        state.records.push(record);
        state.bytes = state.bytes.saturating_add(len);
    }
}

fn would_exceed_bytes(cur: usize, max: usize, len: usize) -> bool {
    if len > max {
        return true;
    }
    match cur.checked_add(len) {
        Some(sum) => sum > max,
        None => true,
    }
}

fn should_drop_record(state: &TraceState, cfg: &NetTraceConfig, len: usize) -> bool {
    if state.records.len() >= cfg.max_records {
        return true;
    }
    would_exceed_bytes(state.bytes, cfg.max_bytes, len)
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

    fn make_ipv4_tcp_frame(payload: &[u8], flags_fragment: u16) -> Vec<u8> {
        make_ipv4_tcp_frame_with_options(payload, flags_fragment, 0, 0)
    }

    fn make_ipv4_tcp_frame_with_options(
        payload: &[u8],
        flags_fragment: u16,
        ip_options_len: usize,
        tcp_options_len: usize,
    ) -> Vec<u8> {
        debug_assert!(ip_options_len.is_multiple_of(4));
        debug_assert!(tcp_options_len.is_multiple_of(4));

        let ip_header_len = 20usize;
        let tcp_header_len = 20usize;
        let ip_header_len = ip_header_len + ip_options_len;
        let tcp_header_len = tcp_header_len + tcp_options_len;
        let total_len = ip_header_len + tcp_header_len + payload.len();

        let mut buf = Vec::with_capacity(14 + total_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype: IPv4

        let ihl_words = (ip_header_len / 4) as u8;
        buf.push((4u8 << 4) | (ihl_words & 0x0f)); // version + IHL
        buf.push(0); // dscp/ecn
        buf.extend_from_slice(&(total_len as u16).to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // identification
        buf.extend_from_slice(&flags_fragment.to_be_bytes());
        buf.push(64); // ttl
        buf.push(6); // protocol: TCP
        buf.extend_from_slice(&0u16.to_be_bytes()); // hdr checksum (ignored)
        buf.extend_from_slice(&[10, 0, 2, 15]); // src ip
        buf.extend_from_slice(&[93, 184, 216, 34]); // dst ip

        buf.resize(buf.len() + ip_options_len, 0);

        buf.extend_from_slice(&12345u16.to_be_bytes()); // src port
        buf.extend_from_slice(&80u16.to_be_bytes()); // dst port
        buf.extend_from_slice(&0u32.to_be_bytes()); // seq
        buf.extend_from_slice(&0u32.to_be_bytes()); // ack
        buf.push((tcp_header_len as u8 / 4) << 4); // data offset
        buf.push(0x18); // PSH+ACK
        buf.extend_from_slice(&0u16.to_be_bytes()); // window
        buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)
        buf.extend_from_slice(&0u16.to_be_bytes()); // urgent

        buf.resize(buf.len() + tcp_options_len, 0);

        buf.extend_from_slice(payload);
        buf
    }

    fn make_vlan_ipv4_tcp_frame(payload: &[u8]) -> Vec<u8> {
        let inner = make_ipv4_tcp_frame(payload, 0);
        let mut out = Vec::with_capacity(inner.len() + 4);
        // dst/src MACs (12 bytes)
        out.extend_from_slice(&inner[..12]);
        // outer ethertype: 802.1Q VLAN
        out.extend_from_slice(&0x8100u16.to_be_bytes());
        // VLAN tag (TCI=0)
        out.extend_from_slice(&0u16.to_be_bytes());
        // inner ethertype: IPv4
        out.extend_from_slice(&0x0800u16.to_be_bytes());
        // rest of frame (starts at IPv4 header in `inner`)
        out.extend_from_slice(&inner[14..]);
        out
    }

    fn wrap_vlan(frame: &[u8], outer_ethertype: u16) -> Vec<u8> {
        assert!(
            frame.len() >= 14,
            "need at least an Ethernet header to wrap with VLAN"
        );
        let mut out = Vec::with_capacity(frame.len() + 4);
        // dst/src MACs (12 bytes)
        out.extend_from_slice(&frame[..12]);
        // outer ethertype: VLAN / provider bridging
        out.extend_from_slice(&outer_ethertype.to_be_bytes());
        // VLAN tag (TCI=0)
        out.extend_from_slice(&0u16.to_be_bytes());
        // inner ethertype: preserved from the wrapped frame
        out.extend_from_slice(&frame[12..14]);
        // rest of frame after the original ethertype
        out.extend_from_slice(&frame[14..]);
        out
    }

    fn make_qinq_ipv4_tcp_frame(payload: &[u8]) -> Vec<u8> {
        // Typical Q-in-Q: outer provider tag (0x88A8), inner customer tag (0x8100).
        let inner = make_ipv4_tcp_frame(payload, 0);
        let inner = wrap_vlan(&inner, 0x8100);
        wrap_vlan(&inner, 0x88a8)
    }

    fn make_arp_request_frame() -> Vec<u8> {
        let mut buf = Vec::with_capacity(14 + 28);

        // Ethernet header (dst broadcast, src 02:00:00:00:00:02, ethertype ARP)
        buf.extend_from_slice(&[0xff; 6]);
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        buf.extend_from_slice(&0x0806u16.to_be_bytes());

        // ARP payload (Ethernet/IPv4 request).
        buf.extend_from_slice(&1u16.to_be_bytes()); // HTYPE Ethernet
        buf.extend_from_slice(&0x0800u16.to_be_bytes()); // PTYPE IPv4
        buf.push(6); // HW len
        buf.push(4); // Proto len
        buf.extend_from_slice(&1u16.to_be_bytes()); // Opcode request
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // sender MAC
        buf.extend_from_slice(&[10, 0, 2, 15]); // sender IP
        buf.extend_from_slice(&[0u8; 6]); // target MAC
        buf.extend_from_slice(&[10, 0, 2, 2]); // target IP (gateway)

        buf
    }

    fn make_tcp_segment(payload: &[u8]) -> Vec<u8> {
        let tcp_header_len = 20usize;
        let mut buf = Vec::with_capacity(tcp_header_len + payload.len());

        buf.extend_from_slice(&12345u16.to_be_bytes()); // src port
        buf.extend_from_slice(&80u16.to_be_bytes()); // dst port
        buf.extend_from_slice(&0u32.to_be_bytes()); // seq
        buf.extend_from_slice(&0u32.to_be_bytes()); // ack
        buf.push((tcp_header_len as u8 / 4) << 4); // data offset
        buf.push(0x18); // PSH+ACK
        buf.extend_from_slice(&0u16.to_be_bytes()); // window
        buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)
        buf.extend_from_slice(&0u16.to_be_bytes()); // urgent

        buf.extend_from_slice(payload);
        buf
    }

    fn make_ipv6_tcp_frame(payload: &[u8]) -> Vec<u8> {
        let tcp = make_tcp_segment(payload);
        let payload_len = tcp.len();

        let mut buf = Vec::with_capacity(14 + 40 + payload_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x86ddu16.to_be_bytes()); // ethertype: IPv6

        buf.push(0x60); // version + traffic class (top bits)
        buf.extend_from_slice(&[0, 0, 0]); // traffic class (low bits) + flow label
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
        buf.push(6); // next header: TCP
        buf.push(64); // hop limit
        buf.extend_from_slice(&[0u8; 16]); // src ip
        buf.extend_from_slice(&[0u8; 16]); // dst ip

        buf.extend_from_slice(&tcp);
        buf
    }

    fn make_ipv4_udp_frame(payload: &[u8]) -> Vec<u8> {
        let ip_header_len = 20usize;
        let udp_header_len = 8usize;
        let total_len = ip_header_len + udp_header_len + payload.len();

        let mut buf = Vec::with_capacity(14 + total_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype: IPv4

        buf.push(0x45); // version + IHL
        buf.push(0); // dscp/ecn
        buf.extend_from_slice(&(total_len as u16).to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // identification
        buf.extend_from_slice(&0u16.to_be_bytes()); // flags/fragment
        buf.push(64); // ttl
        buf.push(17); // protocol: UDP
        buf.extend_from_slice(&0u16.to_be_bytes()); // hdr checksum (ignored)
        buf.extend_from_slice(&[10, 0, 2, 15]); // src ip
        buf.extend_from_slice(&[93, 184, 216, 34]); // dst ip

        buf.extend_from_slice(&12345u16.to_be_bytes()); // src port
        buf.extend_from_slice(&53u16.to_be_bytes()); // dst port
        buf.extend_from_slice(&((udp_header_len + payload.len()) as u16).to_be_bytes()); // length
        buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)

        buf.extend_from_slice(payload);
        buf
    }

    fn make_ipv4_icmp_frame(payload: &[u8]) -> Vec<u8> {
        let ip_header_len = 20usize;
        let icmp_header_len = 8usize;
        let total_len = ip_header_len + icmp_header_len + payload.len();

        let mut buf = Vec::with_capacity(14 + total_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype: IPv4

        buf.push(0x45); // version + IHL
        buf.push(0); // dscp/ecn
        buf.extend_from_slice(&(total_len as u16).to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // identification
        buf.extend_from_slice(&0u16.to_be_bytes()); // flags/fragment
        buf.push(64); // ttl
        buf.push(1); // protocol: ICMPv4
        buf.extend_from_slice(&0u16.to_be_bytes()); // hdr checksum (ignored)
        buf.extend_from_slice(&[10, 0, 2, 15]); // src ip
        buf.extend_from_slice(&[93, 184, 216, 34]); // dst ip

        // ICMP echo request header (8 bytes)
        buf.push(8); // type
        buf.push(0); // code
        buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)
        buf.extend_from_slice(&0u16.to_be_bytes()); // identifier
        buf.extend_from_slice(&0u16.to_be_bytes()); // sequence

        buf.extend_from_slice(payload);
        buf
    }

    fn make_ipv6_udp_frame(payload: &[u8]) -> Vec<u8> {
        let udp_header_len = 8usize;
        let payload_len = udp_header_len + payload.len();

        let mut buf = Vec::with_capacity(14 + 40 + payload_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x86ddu16.to_be_bytes()); // ethertype: IPv6

        buf.push(0x60);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
        buf.push(17); // next header: UDP
        buf.push(64);
        buf.extend_from_slice(&[0u8; 16]); // src ip
        buf.extend_from_slice(&[0u8; 16]); // dst ip

        buf.extend_from_slice(&12345u16.to_be_bytes()); // src port
        buf.extend_from_slice(&53u16.to_be_bytes()); // dst port
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes()); // length
        buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)

        buf.extend_from_slice(payload);
        buf
    }

    fn make_ipv6_icmp_frame(payload: &[u8]) -> Vec<u8> {
        let icmp_header_len = 8usize;
        let payload_len = icmp_header_len + payload.len();

        let mut buf = Vec::with_capacity(14 + 40 + payload_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x86ddu16.to_be_bytes()); // ethertype: IPv6

        buf.push(0x60);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
        buf.push(58); // next header: ICMPv6
        buf.push(64);
        buf.extend_from_slice(&[0u8; 16]); // src ip
        buf.extend_from_slice(&[0u8; 16]); // dst ip

        // ICMPv6 echo request header (8 bytes)
        buf.push(128); // type
        buf.push(0); // code
        buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)
        buf.extend_from_slice(&0u16.to_be_bytes()); // identifier
        buf.extend_from_slice(&0u16.to_be_bytes()); // sequence

        buf.extend_from_slice(payload);
        buf
    }

    fn make_ipv6_hop_by_hop_tcp_frame(payload: &[u8]) -> Vec<u8> {
        let tcp = make_tcp_segment(payload);
        let ext_len = 8usize;
        let payload_len = ext_len + tcp.len();

        let mut buf = Vec::with_capacity(14 + 40 + payload_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x86ddu16.to_be_bytes()); // ethertype: IPv6

        buf.push(0x60);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
        buf.push(0); // next header: Hop-by-Hop Options
        buf.push(64);
        buf.extend_from_slice(&[0u8; 16]); // src ip
        buf.extend_from_slice(&[0u8; 16]); // dst ip

        // Hop-by-Hop header (8 bytes): next header + hdr ext len + 6 bytes options/pad.
        buf.push(6); // next header: TCP
        buf.push(0); // hdr ext len = 0 => 8 bytes total
        buf.extend_from_slice(&[0u8; 6]);

        buf.extend_from_slice(&tcp);
        buf
    }

    fn make_ipv6_fragment_tcp_frame(payload: &[u8], fragment_offset_units: u16) -> Vec<u8> {
        let tcp = make_tcp_segment(payload);
        let payload_len = 8usize + tcp.len();

        let mut buf = Vec::with_capacity(14 + 40 + payload_len);

        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst mac
        buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src mac
        buf.extend_from_slice(&0x86ddu16.to_be_bytes()); // ethertype: IPv6

        buf.push(0x60);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
        buf.push(44); // next header: Fragment
        buf.push(64);
        buf.extend_from_slice(&[0u8; 16]); // src ip
        buf.extend_from_slice(&[0u8; 16]); // dst ip

        // Fragment header (8 bytes).
        buf.push(6); // next header: TCP
        buf.push(0); // reserved
        let flags_fragment: u16 = fragment_offset_units << 3;
        buf.extend_from_slice(&flags_fragment.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // identification

        buf.extend_from_slice(&tcp);
        buf
    }

    #[test]
    fn truncate_redactor_truncates_each_record_type() {
        let tracer = NetTracer::new(NetTraceConfig {
            max_bytes: 1024,
            capture_ethernet: true,
            capture_tcp_proxy: true,
            capture_udp_proxy: true,
            redactor: Some(Arc::new(TruncateRedactor {
                max_ethernet_bytes: 4,
                max_tcp_proxy_bytes: 3,
                max_udp_proxy_bytes: 2,
            })),
            ..NetTraceConfig::default()
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
            TraceRecord::Ethernet { frame, .. } => assert_eq!(frame.as_ref(), &[1, 2, 3, 4]),
            other => panic!("unexpected record: {other:?}"),
        }
        match &guard.records[1] {
            TraceRecord::TcpProxy { data, .. } => assert_eq!(data.as_ref(), &[10, 11, 12]),
            other => panic!("unexpected record: {other:?}"),
        }
        match &guard.records[2] {
            TraceRecord::UdpProxy { data, .. } => assert_eq!(data.as_ref(), &[0xff, 0xfe]),
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
            fn redact_tcp_proxy(&self, _: ProxyDirection, _: u32, _: &[u8]) -> Option<Vec<u8>> {
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
            max_bytes: 1024,
            capture_ethernet: true,
            capture_tcp_proxy: true,
            capture_udp_proxy: true,
            redactor: Some(Arc::new(DropAll)),
            ..NetTraceConfig::default()
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

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_tcp_ipv4() {
        let frame = make_ipv4_tcp_frame(b"hello world", 0);
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable IPv4/TCP frame");

        assert_eq!(out.len(), 14 + 20 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_tcp_ipv4_with_options() {
        let frame = make_ipv4_tcp_frame_with_options(b"hello opts", 0, 12, 12);
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable IPv4/TCP frame with options");

        assert_eq!(out.len(), 14 + (20 + 12) + (20 + 12));
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_vlan_l3_l4_headers_for_vlan_ipv4_tcp() {
        let frame = make_vlan_ipv4_tcp_frame(b"hello vlan");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable VLAN IPv4/TCP frame");

        assert_eq!(out.len(), 14 + 4 + 20 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_qinq_vlan_l3_l4_headers_for_qinq_ipv4_tcp() {
        let frame = make_qinq_ipv4_tcp_frame(b"hello qinq");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable Q-in-Q VLAN IPv4/TCP frame");

        assert_eq!(out.len(), 14 + 8 + 20 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_headers_for_arp() {
        let frame = make_arp_request_frame();
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestRx, &frame)
            .expect("expected parseable ARP frame");

        assert_eq!(out.len(), 14 + 28);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_tcp_ipv6() {
        let frame = make_ipv6_tcp_frame(b"hello ipv6");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable IPv6/TCP frame");

        assert_eq!(out.len(), 14 + 40 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_udp_ipv4() {
        let frame = make_ipv4_udp_frame(b"hello udp");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable IPv4/UDP frame");

        assert_eq!(out.len(), 14 + 20 + 8);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_icmp_ipv4() {
        let frame = make_ipv4_icmp_frame(b"hello icmp");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestRx, &frame)
            .expect("expected parseable IPv4/ICMP frame");

        assert_eq!(out.len(), 14 + 20 + 8);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_tcp_ipv6_with_hop_by_hop() {
        let frame = make_ipv6_hop_by_hop_tcp_frame(b"hello ipv6 ext");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable IPv6 Hop-by-Hop/TCP frame");

        assert_eq!(out.len(), 14 + 40 + 8 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_udp_ipv6() {
        let frame = make_ipv6_udp_frame(b"hello udp6");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestTx, &frame)
            .expect("expected parseable IPv6/UDP frame");

        assert_eq!(out.len(), 14 + 40 + 8);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l2_l3_l4_headers_for_icmp_ipv6() {
        let frame = make_ipv6_icmp_frame(b"hello icmp6");
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestRx, &frame)
            .expect("expected parseable IPv6/ICMPv6 frame");

        assert_eq!(out.len(), 14 + 40 + 8);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_does_not_capture_payload_for_non_initial_ipv6_fragments() {
        let frame = make_ipv6_fragment_tcp_frame(b"sensitive payload", 1);
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestRx, &frame)
            .expect("expected parseable IPv6 fragment header");

        assert_eq!(out.len(), 14 + 40 + 8);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_keeps_l4_for_initial_ipv6_fragments() {
        let frame = make_ipv6_fragment_tcp_frame(b"payload", 0);
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestRx, &frame)
            .expect("expected parseable IPv6 fragment + TCP frame");

        assert_eq!(out.len(), 14 + 40 + 8 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_does_not_capture_payload_for_non_initial_ipv4_fragments() {
        // Fragment offset != 0 (lower 13 bits). Even though the frame contains bytes after the IPv4
        // header, the redactor must not treat them as a TCP header.
        let frame = make_ipv4_tcp_frame(b"sensitive payload", 1);
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        let out = redactor
            .redact_ethernet(FrameDirection::GuestRx, &frame)
            .expect("expected parseable IPv4 fragment");

        assert_eq!(out.len(), 14 + 20);
        assert_eq!(out.as_slice(), &frame[..out.len()]);
    }

    #[test]
    fn headers_only_redactor_drops_unparseable_frames_and_proxy_payloads() {
        let redactor = HeadersOnlyRedactor {
            max_ethernet_bytes: 2048,
        };

        assert!(
            redactor
                .redact_ethernet(FrameDirection::GuestTx, &[0u8; 13])
                .is_none(),
            "should drop truncated ethernet frames"
        );

        assert!(
            redactor
                .redact_tcp_proxy(ProxyDirection::GuestToRemote, 1, b"secret")
                .is_none(),
            "should drop TCP proxy payloads"
        );

        assert!(
            redactor
                .redact_udp_proxy(
                    ProxyDirection::GuestToRemote,
                    UdpTransport::Proxy,
                    Ipv4Addr::new(1, 1, 1, 1),
                    1,
                    2,
                    b"secret"
                )
                .is_none(),
            "should drop UDP proxy payloads"
        );
    }
}

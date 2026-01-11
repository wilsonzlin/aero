use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    next_session_id: AtomicU64,

    // Sessions
    sessions_active: AtomicU64,
    sessions_total: AtomicU64,

    // Frames/bytes
    frames_rx_total: AtomicU64,
    frames_tx_total: AtomicU64,
    bytes_rx_total: AtomicU64,
    bytes_tx_total: AtomicU64,
    frames_dropped_total: AtomicU64,

    // Policy
    policy_denied_total: AtomicU64,

    // TCP proxy
    tcp_conns_active: AtomicU64,
    tcp_connect_fail_total: AtomicU64,

    // UDP proxy
    udp_flows_active: AtomicU64,
    udp_send_fail_total: AtomicU64,

    // DNS
    dns_queries_total: AtomicU64,
    dns_fail_total: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                next_session_id: AtomicU64::new(1),
                sessions_active: AtomicU64::new(0),
                sessions_total: AtomicU64::new(0),
                frames_rx_total: AtomicU64::new(0),
                frames_tx_total: AtomicU64::new(0),
                bytes_rx_total: AtomicU64::new(0),
                bytes_tx_total: AtomicU64::new(0),
                frames_dropped_total: AtomicU64::new(0),
                policy_denied_total: AtomicU64::new(0),
                tcp_conns_active: AtomicU64::new(0),
                tcp_connect_fail_total: AtomicU64::new(0),
                udp_flows_active: AtomicU64::new(0),
                udp_send_fail_total: AtomicU64::new(0),
                dns_queries_total: AtomicU64::new(0),
                dns_fail_total: AtomicU64::new(0),
            }),
        }
    }

    pub fn next_session_id(&self) -> u64 {
        self.inner.next_session_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn session_opened(&self) {
        self.inner.sessions_total.fetch_add(1, Ordering::Relaxed);
        self.inner.sessions_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_closed(&self) {
        self.inner.sessions_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn tcp_conn_opened(&self) {
        self.inner.tcp_conns_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn tcp_conn_closed(&self) {
        self.inner.tcp_conns_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn tcp_connect_failed(&self) {
        self.inner
            .tcp_connect_fail_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn udp_flow_opened(&self) {
        self.inner.udp_flows_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn udp_flow_closed(&self) {
        self.inner.udp_flows_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn udp_send_failed(&self) {
        self.inner
            .udp_send_fail_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn frame_rx(&self, bytes: usize) {
        self.inner.frames_rx_total.fetch_add(1, Ordering::Relaxed);
        self.inner
            .bytes_rx_total
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn frame_tx(&self, bytes: usize) {
        self.inner.frames_tx_total.fetch_add(1, Ordering::Relaxed);
        self.inner
            .bytes_tx_total
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn frame_dropped(&self) {
        self.inner
            .frames_dropped_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn policy_denied(&self) {
        self.inner
            .policy_denied_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn dns_query(&self) {
        self.inner.dns_queries_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dns_fail(&self) {
        self.inner.dns_fail_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn render_prometheus(&self) -> String {
        let sessions_active = self.inner.sessions_active.load(Ordering::Relaxed);
        let sessions_total = self.inner.sessions_total.load(Ordering::Relaxed);
        let frames_rx_total = self.inner.frames_rx_total.load(Ordering::Relaxed);
        let frames_tx_total = self.inner.frames_tx_total.load(Ordering::Relaxed);
        let bytes_rx_total = self.inner.bytes_rx_total.load(Ordering::Relaxed);
        let bytes_tx_total = self.inner.bytes_tx_total.load(Ordering::Relaxed);
        let frames_dropped_total = self.inner.frames_dropped_total.load(Ordering::Relaxed);
        let policy_denied_total = self.inner.policy_denied_total.load(Ordering::Relaxed);
        let tcp_conns_active = self.inner.tcp_conns_active.load(Ordering::Relaxed);
        let tcp_connect_fail_total = self.inner.tcp_connect_fail_total.load(Ordering::Relaxed);
        let udp_flows_active = self.inner.udp_flows_active.load(Ordering::Relaxed);
        let udp_send_fail_total = self.inner.udp_send_fail_total.load(Ordering::Relaxed);
        let dns_queries_total = self.inner.dns_queries_total.load(Ordering::Relaxed);
        let dns_fail_total = self.inner.dns_fail_total.load(Ordering::Relaxed);

        let mut out = String::new();

        push_gauge(&mut out, "l2_sessions_active", sessions_active);
        push_counter(&mut out, "l2_sessions_total", sessions_total);

        push_counter(&mut out, "l2_frames_rx_total", frames_rx_total);
        push_counter(&mut out, "l2_frames_tx_total", frames_tx_total);
        push_counter(&mut out, "l2_bytes_rx_total", bytes_rx_total);
        push_counter(&mut out, "l2_bytes_tx_total", bytes_tx_total);
        push_counter(&mut out, "l2_frames_dropped_total", frames_dropped_total);

        push_counter(&mut out, "l2_policy_denied_total", policy_denied_total);

        push_gauge(&mut out, "l2_tcp_conns_active", tcp_conns_active);
        push_counter(
            &mut out,
            "l2_tcp_connect_fail_total",
            tcp_connect_fail_total,
        );

        push_gauge(&mut out, "l2_udp_flows_active", udp_flows_active);
        push_counter(&mut out, "l2_udp_send_fail_total", udp_send_fail_total);

        push_counter(&mut out, "l2_dns_queries_total", dns_queries_total);
        push_counter(&mut out, "l2_dns_fail_total", dns_fail_total);

        out
    }
}

fn push_gauge(out: &mut String, name: &str, val: u64) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" gauge\n");
    out.push_str(name);
    out.push(' ');
    out.push_str(&val.to_string());
    out.push('\n');
}

fn push_counter(out: &mut String, name: &str, val: u64) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" counter\n");
    out.push_str(name);
    out.push(' ');
    out.push_str(&val.to_string());
    out.push('\n');
}

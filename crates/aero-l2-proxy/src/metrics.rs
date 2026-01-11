use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthRejectReason {
    MissingCredentials,
    InvalidApiKey,
    InvalidCookie,
    InvalidJwt,
    JwtOriginMismatch,
}

impl AuthRejectReason {
    const COUNT: usize = 5;

    const ALL: [AuthRejectReason; Self::COUNT] = [
        AuthRejectReason::MissingCredentials,
        AuthRejectReason::InvalidApiKey,
        AuthRejectReason::InvalidCookie,
        AuthRejectReason::InvalidJwt,
        AuthRejectReason::JwtOriginMismatch,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            AuthRejectReason::MissingCredentials => "missing_credentials",
            AuthRejectReason::InvalidApiKey => "invalid_api_key",
            AuthRejectReason::InvalidCookie => "invalid_cookie",
            AuthRejectReason::InvalidJwt => "invalid_jwt",
            AuthRejectReason::JwtOriginMismatch => "jwt_origin_mismatch",
        }
    }

    fn idx(self) -> usize {
        match self {
            AuthRejectReason::MissingCredentials => 0,
            AuthRejectReason::InvalidApiKey => 1,
            AuthRejectReason::InvalidCookie => 2,
            AuthRejectReason::InvalidJwt => 3,
            AuthRejectReason::JwtOriginMismatch => 4,
        }
    }
}

#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    next_session_id: AtomicU64,

    // WebSocket upgrade
    upgrade_rejected_total: AtomicU64,
    upgrade_reject_max_connections_per_session_total: AtomicU64,
    auth_reject_total: [AtomicU64; AuthRejectReason::COUNT],

    // Sessions
    sessions_active: AtomicU64,
    sessions_total: AtomicU64,
    idle_timeouts_total: AtomicU64,

    // Upgrade rejections
    upgrade_reject_origin_missing_total: AtomicU64,
    upgrade_reject_origin_not_allowed_total: AtomicU64,
    upgrade_reject_host_missing_total: AtomicU64,
    upgrade_reject_host_invalid_total: AtomicU64,
    upgrade_reject_host_not_allowed_total: AtomicU64,
    upgrade_reject_auth_missing_total: AtomicU64,
    upgrade_reject_auth_invalid_total: AtomicU64,
    upgrade_reject_max_connections_total: AtomicU64,
    upgrade_reject_max_tunnels_per_session_total: AtomicU64,
    upgrade_ip_limit_exceeded_total: AtomicU64,

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
    udp_flow_limit_exceeded_total: AtomicU64,

    // DNS
    dns_queries_total: AtomicU64,
    dns_fail_total: AtomicU64,

    // PING RTT histogram (ms)
    ping_rtt_ms_bucket_counts: [AtomicU64; PING_RTT_MS_BUCKETS.len() + 1],
    ping_rtt_ms_sum_ms: AtomicU64,
    ping_rtt_ms_count: AtomicU64,
}

const PING_RTT_MS_BUCKETS: [u64; 10] = [1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500];

impl Metrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                next_session_id: AtomicU64::new(1),
                upgrade_rejected_total: AtomicU64::new(0),
                upgrade_reject_max_connections_per_session_total: AtomicU64::new(0),
                auth_reject_total: std::array::from_fn(|_| AtomicU64::new(0)),
                sessions_active: AtomicU64::new(0),
                sessions_total: AtomicU64::new(0),
                idle_timeouts_total: AtomicU64::new(0),
                upgrade_reject_origin_missing_total: AtomicU64::new(0),
                upgrade_reject_origin_not_allowed_total: AtomicU64::new(0),
                upgrade_reject_host_missing_total: AtomicU64::new(0),
                upgrade_reject_host_invalid_total: AtomicU64::new(0),
                upgrade_reject_host_not_allowed_total: AtomicU64::new(0),
                upgrade_reject_auth_missing_total: AtomicU64::new(0),
                upgrade_reject_auth_invalid_total: AtomicU64::new(0),
                upgrade_reject_max_connections_total: AtomicU64::new(0),
                upgrade_reject_max_tunnels_per_session_total: AtomicU64::new(0),
                upgrade_ip_limit_exceeded_total: AtomicU64::new(0),
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
                udp_flow_limit_exceeded_total: AtomicU64::new(0),
                dns_queries_total: AtomicU64::new(0),
                dns_fail_total: AtomicU64::new(0),
                ping_rtt_ms_bucket_counts: std::array::from_fn(|_| AtomicU64::new(0)),
                ping_rtt_ms_sum_ms: AtomicU64::new(0),
                ping_rtt_ms_count: AtomicU64::new(0),
            }),
        }
    }

    pub fn next_session_id(&self) -> u64 {
        self.inner.next_session_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn upgrade_rejected(&self) {
        self.inner
            .upgrade_rejected_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_opened(&self) {
        self.inner.sessions_total.fetch_add(1, Ordering::Relaxed);
        self.inner.sessions_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_closed(&self) {
        let _ =
            self.inner
                .sessions_active
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                    Some(val.saturating_sub(1))
                });
    }

    pub fn upgrade_reject_origin_missing(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_origin_missing_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_origin_not_allowed(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_origin_not_allowed_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_host_missing(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_host_missing_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_host_invalid(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_host_invalid_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_host_not_allowed(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_host_not_allowed_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_auth_missing(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_auth_missing_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_auth_invalid(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_auth_invalid_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_max_connections(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_max_connections_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upgrade_reject_max_tunnels_per_session(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_reject_max_tunnels_per_session_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .upgrade_reject_max_connections_per_session_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn auth_rejected(&self, reason: AuthRejectReason) {
        self.inner.auth_reject_total[reason.idx()].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn upgrade_reject_max_connections_per_session(&self) {
        // Kept as an alias for "tunnels per session" for historical naming reasons.
        self.upgrade_reject_max_tunnels_per_session();
    }

    pub fn upgrade_ip_limit_exceeded(&self) {
        self.upgrade_rejected();
        self.inner
            .upgrade_ip_limit_exceeded_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn idle_timeout_closed(&self) {
        self.inner
            .idle_timeouts_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn tcp_conn_opened(&self) {
        self.inner.tcp_conns_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn tcp_conn_closed(&self) {
        let _ =
            self.inner
                .tcp_conns_active
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                    Some(val.saturating_sub(1))
                });
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
        let _ =
            self.inner
                .udp_flows_active
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                    Some(val.saturating_sub(1))
                });
    }

    pub fn udp_send_failed(&self) {
        self.inner
            .udp_send_fail_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn udp_flow_limit_exceeded(&self) {
        self.inner
            .udp_flow_limit_exceeded_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ping_rtt_ms(&self, ms: u64) {
        self.inner.ping_rtt_ms_count.fetch_add(1, Ordering::Relaxed);
        self.inner
            .ping_rtt_ms_sum_ms
            .fetch_add(ms, Ordering::Relaxed);

        let idx = PING_RTT_MS_BUCKETS
            .iter()
            .position(|bound| ms <= *bound)
            .unwrap_or(PING_RTT_MS_BUCKETS.len());
        self.inner.ping_rtt_ms_bucket_counts[idx].fetch_add(1, Ordering::Relaxed);
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
        let upgrade_rejected_total = self.inner.upgrade_rejected_total.load(Ordering::Relaxed);
        let upgrade_reject_max_connections_per_session_total = self
            .inner
            .upgrade_reject_max_connections_per_session_total
            .load(Ordering::Relaxed);
        let sessions_active = self.inner.sessions_active.load(Ordering::Relaxed);
        let sessions_total = self.inner.sessions_total.load(Ordering::Relaxed);
        let idle_timeouts_total = self.inner.idle_timeouts_total.load(Ordering::Relaxed);
        let upgrade_reject_origin_missing_total = self
            .inner
            .upgrade_reject_origin_missing_total
            .load(Ordering::Relaxed);
        let upgrade_reject_origin_not_allowed_total = self
            .inner
            .upgrade_reject_origin_not_allowed_total
            .load(Ordering::Relaxed);
        let upgrade_reject_host_missing_total = self
            .inner
            .upgrade_reject_host_missing_total
            .load(Ordering::Relaxed);
        let upgrade_reject_host_invalid_total = self
            .inner
            .upgrade_reject_host_invalid_total
            .load(Ordering::Relaxed);
        let upgrade_reject_host_not_allowed_total = self
            .inner
            .upgrade_reject_host_not_allowed_total
            .load(Ordering::Relaxed);
        let upgrade_reject_auth_missing_total = self
            .inner
            .upgrade_reject_auth_missing_total
            .load(Ordering::Relaxed);
        let upgrade_reject_auth_invalid_total = self
            .inner
            .upgrade_reject_auth_invalid_total
            .load(Ordering::Relaxed);
        let upgrade_reject_max_connections_total = self
            .inner
            .upgrade_reject_max_connections_total
            .load(Ordering::Relaxed);
        let upgrade_reject_max_tunnels_per_session_total = self
            .inner
            .upgrade_reject_max_tunnels_per_session_total
            .load(Ordering::Relaxed);
        let upgrade_ip_limit_exceeded_total = self
            .inner
            .upgrade_ip_limit_exceeded_total
            .load(Ordering::Relaxed);
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
        let udp_flow_limit_exceeded_total = self
            .inner
            .udp_flow_limit_exceeded_total
            .load(Ordering::Relaxed);
        let dns_queries_total = self.inner.dns_queries_total.load(Ordering::Relaxed);
        let dns_fail_total = self.inner.dns_fail_total.load(Ordering::Relaxed);

        let mut out = String::new();

        push_counter(
            &mut out,
            "l2_upgrade_rejected_total",
            upgrade_rejected_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_max_connections_per_session_total",
            upgrade_reject_max_connections_per_session_total,
        );
        push_auth_reject_counters(&mut out, &self.inner);

        push_gauge(&mut out, "l2_sessions_active", sessions_active);
        push_counter(&mut out, "l2_sessions_total", sessions_total);
        push_counter(&mut out, "aero_l2_idle_timeouts_total", idle_timeouts_total);

        push_counter(
            &mut out,
            "l2_upgrade_reject_origin_missing_total",
            upgrade_reject_origin_missing_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_origin_not_allowed_total",
            upgrade_reject_origin_not_allowed_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_host_missing_total",
            upgrade_reject_host_missing_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_host_invalid_total",
            upgrade_reject_host_invalid_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_host_not_allowed_total",
            upgrade_reject_host_not_allowed_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_auth_missing_total",
            upgrade_reject_auth_missing_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_auth_invalid_total",
            upgrade_reject_auth_invalid_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_max_connections_total",
            upgrade_reject_max_connections_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_reject_max_tunnels_per_session_total",
            upgrade_reject_max_tunnels_per_session_total,
        );
        push_counter(
            &mut out,
            "l2_upgrade_ip_limit_exceeded_total",
            upgrade_ip_limit_exceeded_total,
        );

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
        push_counter(
            &mut out,
            "l2_udp_flow_limit_exceeded_total",
            udp_flow_limit_exceeded_total,
        );

        push_counter(&mut out, "l2_dns_queries_total", dns_queries_total);
        push_counter(&mut out, "l2_dns_fail_total", dns_fail_total);

        push_ping_rtt_histogram(&mut out, &self.inner);

        out
    }
}

fn push_auth_reject_counters(out: &mut String, metrics: &MetricsInner) {
    out.push_str("# TYPE l2_auth_reject_total counter\n");
    for reason in AuthRejectReason::ALL {
        let val = metrics.auth_reject_total[reason.idx()].load(Ordering::Relaxed);
        out.push_str("l2_auth_reject_total{reason=\"");
        out.push_str(reason.label());
        out.push_str("\"} ");
        out.push_str(&val.to_string());
        out.push('\n');
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

fn push_ping_rtt_histogram(out: &mut String, metrics: &MetricsInner) {
    out.push_str("# TYPE l2_ping_rtt_ms histogram\n");

    let mut cumulative = 0u64;
    for (idx, bound) in PING_RTT_MS_BUCKETS.iter().enumerate() {
        cumulative += metrics.ping_rtt_ms_bucket_counts[idx].load(Ordering::Relaxed);
        out.push_str("l2_ping_rtt_ms_bucket{le=\"");
        out.push_str(&bound.to_string());
        out.push_str("\"} ");
        out.push_str(&cumulative.to_string());
        out.push('\n');
    }

    let total = metrics.ping_rtt_ms_count.load(Ordering::Relaxed);
    out.push_str("l2_ping_rtt_ms_bucket{le=\"+Inf\"} ");
    out.push_str(&total.to_string());
    out.push('\n');

    let sum_ms = metrics.ping_rtt_ms_sum_ms.load(Ordering::Relaxed);
    out.push_str("l2_ping_rtt_ms_sum ");
    out.push_str(&sum_ms.to_string());
    out.push('\n');

    out.push_str("l2_ping_rtt_ms_count ");
    out.push_str(&total.to_string());
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_metric(body: &str, name: &str) -> u64 {
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once(' ') else {
                continue;
            };
            if k == name {
                return v.parse().unwrap();
            }
        }
        panic!("metric {name:?} not found");
    }

    #[test]
    fn gauges_saturate_on_underflow() {
        let metrics = Metrics::new();

        // These should not wrap around to `u64::MAX` if called out of order.
        metrics.session_closed();
        metrics.tcp_conn_closed();
        metrics.udp_flow_closed();

        let body = metrics.render_prometheus();
        assert_eq!(parse_metric(&body, "l2_sessions_active"), 0);
        assert_eq!(parse_metric(&body, "l2_tcp_conns_active"), 0);
        assert_eq!(parse_metric(&body, "l2_udp_flows_active"), 0);
    }
}

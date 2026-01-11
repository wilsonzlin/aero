use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{Context, Result};

use crate::{overrides::TestOverrides, policy::EgressPolicy};

#[derive(Debug, Clone)]
pub struct SecurityConfig {
    /// Disable Origin enforcement (trusted local development only).
    pub open: bool,
    /// Comma-separated allowlist of exact Origin strings. `"*"` allows any Origin value (but still
    /// requires the header unless `open=1`).
    pub allowed_origins: AllowedOrigins,
    /// Optional pre-shared token required at upgrade time.
    pub token: Option<String>,
    /// Process-wide concurrent tunnel cap (`0` disables).
    pub max_connections: usize,
    /// Total bytes per connection (rx + tx, `0` disables).
    pub max_bytes_per_connection: u64,
    /// Inbound messages per second per connection (`0` disables).
    pub max_frames_per_second: u64,
}

#[derive(Debug, Clone)]
pub enum AllowedOrigins {
    List(Vec<String>),
    Any,
}

impl Default for AllowedOrigins {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            open: false,
            allowed_origins: AllowedOrigins::default(),
            token: None,
            max_connections: 64,
            max_bytes_per_connection: 0,
            max_frames_per_second: 0,
        }
    }
}

impl SecurityConfig {
    pub fn from_env() -> Self {
        let open = std::env::var("AERO_L2_OPEN")
            .ok()
            .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"))
            .unwrap_or(false);

        let allowed_origins_raw = std::env::var("AERO_L2_ALLOWED_ORIGINS").ok();
        let allowed_origins = match allowed_origins_raw {
            Some(raw) => {
                let mut out = Vec::new();
                let mut any = false;
                for entry in raw.split(',').map(str::trim) {
                    if entry.is_empty() {
                        continue;
                    }
                    if entry == "*" {
                        any = true;
                        break;
                    }
                    out.push(entry.to_string());
                }
                if any {
                    AllowedOrigins::Any
                } else {
                    AllowedOrigins::List(out)
                }
            }
            None => AllowedOrigins::List(Vec::new()),
        };

        let token = std::env::var("AERO_L2_TOKEN")
            .ok()
            .and_then(|v| (!v.trim().is_empty()).then(|| v));

        let max_connections = std::env::var("AERO_L2_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(Self::default().max_connections);

        let max_bytes_per_connection = std::env::var("AERO_L2_MAX_BYTES_PER_CONNECTION")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let max_frames_per_second = std::env::var("AERO_L2_MAX_FRAMES_PER_SECOND")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Self {
            open,
            allowed_origins,
            token,
            max_connections,
            max_bytes_per_connection,
            max_frames_per_second,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub bind_addr: SocketAddr,

    pub l2_max_frame_payload: usize,
    pub l2_max_control_payload: usize,

    /// Interval at which the proxy sends protocol-level PING messages. This is optional and
    /// disabled by default; clients may still implement their own keepalive.
    pub ping_interval: Option<Duration>,

    pub tcp_connect_timeout: Duration,
    pub tcp_send_buffer: usize,
    pub ws_send_buffer: usize,

    pub dns_default_ttl_secs: u32,
    pub dns_max_ttl_secs: u32,

    pub capture_dir: Option<PathBuf>,

    pub security: SecurityConfig,

    pub policy: EgressPolicy,
    pub test_overrides: TestOverrides,
}

impl ProxyConfig {
    pub fn from_env() -> Result<Self> {
        let bind_addr = std::env::var("AERO_L2_PROXY_LISTEN_ADDR")
            .or_else(|_| std::env::var("AERO_L2_PROXY_BIND_ADDR"))
            .ok()
            .and_then(|v| SocketAddr::from_str(&v).ok())
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 8090)));

        let l2_max_frame_payload = std::env::var("AERO_L2_MAX_FRAME_PAYLOAD")
            .or_else(|_| std::env::var("AERO_L2_MAX_FRAME_SIZE"))
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD);

        let l2_max_control_payload = std::env::var("AERO_L2_MAX_CONTROL_PAYLOAD")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD);

        let ping_interval = std::env::var("AERO_L2_PING_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .map(Duration::from_millis);

        let tcp_connect_timeout = std::env::var("AERO_L2_TCP_CONNECT_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_secs(5));

        let tcp_send_buffer = std::env::var("AERO_L2_TCP_SEND_BUFFER")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(32);

        let ws_send_buffer = std::env::var("AERO_L2_WS_SEND_BUFFER")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(64);

        let dns_default_ttl_secs = std::env::var("AERO_L2_DNS_DEFAULT_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(60);

        let dns_max_ttl_secs = std::env::var("AERO_L2_DNS_MAX_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(300);

        let capture_dir = std::env::var("AERO_L2_CAPTURE_DIR")
            .ok()
            .and_then(|v| (!v.trim().is_empty()).then(|| PathBuf::from(v)));

        let security = SecurityConfig::from_env();

        let policy = EgressPolicy::from_env().context("parse egress policy")?;
        let test_overrides = TestOverrides::from_env().context("parse test-mode overrides")?;

        Ok(Self {
            bind_addr,
            l2_max_frame_payload,
            l2_max_control_payload,
            ping_interval,
            tcp_connect_timeout,
            tcp_send_buffer,
            ws_send_buffer,
            dns_default_ttl_secs,
            dns_max_ttl_secs,
            capture_dir,
            security,
            policy,
            test_overrides,
        })
    }
}

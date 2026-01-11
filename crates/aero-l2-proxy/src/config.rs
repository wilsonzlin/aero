use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{anyhow, Context, Result};

use aero_net_stack::StackConfig;

use crate::{overrides::TestOverrides, policy::EgressPolicy};

#[derive(Debug, Clone)]
pub struct SecurityConfig {
    /// Disable Origin enforcement (trusted local development only).
    pub open: bool,
    /// Comma-separated allowlist of normalized Origin strings.
    ///
    /// `"*"` allows any Origin value (but still requires the header unless `open=1`).
    pub allowed_origins: AllowedOrigins,
    /// Comma-separated allowlist of exact Host values accepted at WebSocket upgrade time.
    ///
    /// - Compared case-insensitively (hostnames are lowercased).
    /// - Default ports are ignored during comparisons (`:80` for `http/ws`, `:443` for
    ///   `https/wss`).
    ///
    /// When unset/empty, Host validation is disabled.
    pub allowed_hosts: Vec<String>,
    /// When enabled, prefer proxy-provided host headers (`Forwarded: host=` or
    /// `X-Forwarded-Host`) over `Host` for allowlist validation.
    ///
    /// This should only be enabled when running behind a trusted reverse proxy that strips or
    /// overwrites these headers from untrusted clients.
    pub trust_proxy_host: bool,
    /// Authentication mode for `/l2` WebSocket upgrades.
    pub auth_mode: AuthMode,
    /// Static API key value (only used for `auth_mode=api_key`).
    pub api_key: Option<String>,
    /// HMAC secret for verifying JWTs (only used for `auth_mode=jwt` / `cookie_or_jwt`).
    pub jwt_secret: Option<Vec<u8>>,
    /// HMAC secret shared with `backend/aero-gateway` for verifying the `aero_session` cookie.
    /// (Only used for `auth_mode=cookie` / `cookie_or_jwt`.)
    pub session_secret: Option<Vec<u8>>,
    /// Process-wide concurrent tunnel cap (`0` disables).
    pub max_connections: usize,
    /// Concurrent tunnel cap per authenticated gateway session (`0` disables).
    pub max_tunnels_per_session: usize,
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
            allowed_hosts: Vec::new(),
            trust_proxy_host: false,
            auth_mode: AuthMode::None,
            api_key: None,
            jwt_secret: None,
            session_secret: None,
            max_connections: 64,
            max_tunnels_per_session: 1,
            max_bytes_per_connection: 0,
            max_frames_per_second: 0,
        }
    }
}

impl SecurityConfig {
    pub fn from_env() -> Result<Self> {
        // `AERO_L2_OPEN` is a security escape hatch; keep parsing strict so deployments don't
        // accidentally enable it via loose truthy values.
        let open = std::env::var("AERO_L2_OPEN")
            .ok()
            .map(|v| v.trim() == "1")
            .unwrap_or(false);

        let allowed_origins = {
            let base: Option<(&'static str, String)> =
                match std::env::var("AERO_L2_ALLOWED_ORIGINS") {
                    Ok(v) => Some(("AERO_L2_ALLOWED_ORIGINS", v)),
                    Err(_) => std::env::var("ALLOWED_ORIGINS")
                        .ok()
                        .map(|v| ("ALLOWED_ORIGINS", v)),
                };
            let extra = std::env::var("AERO_L2_ALLOWED_ORIGINS_EXTRA")
                .ok()
                .map(|v| ("AERO_L2_ALLOWED_ORIGINS_EXTRA", v));

            let mut sources = Vec::new();
            if let Some(base) = base {
                sources.push(base);
            }
            if let Some(extra) = extra {
                sources.push(extra);
            }
            parse_allowed_origins(sources)?
        };

        let legacy_token = std::env::var("AERO_L2_TOKEN").ok().and_then(|v| {
            let trimmed = v.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });

        let auth_mode_raw = std::env::var("AERO_L2_AUTH_MODE")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());

        let auth_mode = match auth_mode_raw.as_deref() {
            None => legacy_token
                .as_ref()
                .map(|_| AuthMode::ApiKey)
                .unwrap_or(AuthMode::None),
            Some("none") => AuthMode::None,
            Some("cookie") => AuthMode::Cookie,
            Some("api_key") => AuthMode::ApiKey,
            Some("jwt") => AuthMode::Jwt,
            Some("cookie_or_jwt") => AuthMode::CookieOrJwt,
            Some(other) => return Err(anyhow!("invalid AERO_L2_AUTH_MODE {other:?}")),
        };

        let api_key = if auth_mode == AuthMode::ApiKey {
            let key = std::env::var("AERO_L2_API_KEY")
                .ok()
                .and_then(|v| {
                    let trimmed = v.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                })
                .or(legacy_token);
            if key.is_none() {
                return Err(anyhow!(
                    "AERO_L2_API_KEY (or legacy AERO_L2_TOKEN) is required for AERO_L2_AUTH_MODE=api_key"
                ));
            }
            key
        } else {
            None
        };

        let jwt_secret = if matches!(auth_mode, AuthMode::Jwt | AuthMode::CookieOrJwt) {
            let secret = std::env::var("AERO_L2_JWT_SECRET").ok().and_then(|v| {
                let trimmed = v.trim();
                (!trimmed.is_empty()).then(|| trimmed.as_bytes().to_vec())
            });
            if secret.is_none() {
                return Err(anyhow!(
                    "AERO_L2_JWT_SECRET is required for AERO_L2_AUTH_MODE=jwt/cookie_or_jwt"
                ));
            }
            secret
        } else {
            None
        };

        let session_secret = if matches!(auth_mode, AuthMode::Cookie | AuthMode::CookieOrJwt) {
            let secret = std::env::var("AERO_L2_SESSION_SECRET")
                .or_else(|_| std::env::var("SESSION_SECRET"))
                .ok()
                .and_then(|v| {
                    let trimmed = v.trim();
                    (!trimmed.is_empty()).then(|| trimmed.as_bytes().to_vec())
                });
            if secret.is_none() {
                return Err(anyhow!(
                    "AERO_L2_SESSION_SECRET (or SESSION_SECRET) is required for AERO_L2_AUTH_MODE=cookie/cookie_or_jwt"
                ));
            }
            secret
        } else {
            None
        };

        let allowed_hosts = std::env::var("AERO_L2_ALLOWED_HOSTS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let trust_proxy_host = std::env::var("AERO_L2_TRUST_PROXY_HOST")
            .ok()
            .map(|v| {
                matches!(
                    v.trim(),
                    "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
                )
            })
            .unwrap_or(false);

        let max_connections = std::env::var("AERO_L2_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(Self::default().max_connections);

        let max_tunnels_per_session = std::env::var("AERO_L2_MAX_TUNNELS_PER_SESSION")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(Self::default().max_tunnels_per_session);

        let max_bytes_per_connection = std::env::var("AERO_L2_MAX_BYTES_PER_CONNECTION")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let max_frames_per_second = std::env::var("AERO_L2_MAX_FRAMES_PER_SECOND")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(Self {
            open,
            allowed_origins,
            allowed_hosts,
            trust_proxy_host,
            auth_mode,
            api_key,
            jwt_secret,
            session_secret,
            max_connections,
            max_tunnels_per_session,
            max_bytes_per_connection,
            max_frames_per_second,
        })
    }
}

fn parse_allowed_origins(sources: Vec<(&'static str, String)>) -> Result<AllowedOrigins> {
    let mut out = Vec::new();
    let mut any = false;

    for (name, raw) in sources {
        for entry in raw.split(',').map(str::trim) {
            if entry.is_empty() {
                continue;
            }
            if entry == "*" {
                any = true;
                continue;
            }
            let normalized = crate::origin::normalize_origin(entry).ok_or_else(|| {
                anyhow!(
                    "invalid origin entry {entry:?} in {name} (expected an origin like \"https://example.com\")"
                )
            })?;
            out.push(normalized);
        }
    }

    if any {
        Ok(AllowedOrigins::Any)
    } else {
        Ok(AllowedOrigins::List(out))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    None,
    Cookie,
    ApiKey,
    Jwt,
    CookieOrJwt,
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub bind_addr: SocketAddr,

    pub l2_max_frame_payload: usize,
    pub l2_max_control_payload: usize,

    /// Maximum time to wait for in-flight tunnels/requests to end during shutdown before aborting
    /// the server task.
    pub shutdown_grace: Duration,

    /// Interval at which the proxy sends protocol-level PING messages. This is optional and
    /// disabled by default; clients may still implement their own keepalive.
    pub ping_interval: Option<Duration>,

    pub tcp_connect_timeout: Duration,
    pub tcp_send_buffer: usize,
    pub ws_send_buffer: usize,

    /// Maximum number of concurrent UDP flows (unique (guest_port, dst_ip, dst_port)) tracked per
    /// tunnel (`0` disables).
    pub max_udp_flows_per_tunnel: usize,
    /// UDP flow idle timeout (`0` disables).
    pub udp_flow_idle_timeout: Option<Duration>,

    /// Stack-level limits (defense in depth) applied to `aero_net_stack::StackConfig`.
    pub stack_max_tcp_connections: u32,
    pub stack_max_pending_dns: u32,
    pub stack_max_dns_cache_entries: u32,
    pub stack_max_buffered_tcp_bytes_per_conn: u32,

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

        let shutdown_grace = std::env::var("AERO_L2_SHUTDOWN_GRACE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(3000));

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

        let max_udp_flows_per_tunnel = read_env_usize_clamped(
            "AERO_L2_MAX_UDP_FLOWS_PER_TUNNEL",
            256,
            0,
            65_535,
        );

        let udp_flow_idle_timeout_ms = read_env_u64_clamped(
            "AERO_L2_UDP_FLOW_IDLE_TIMEOUT_MS",
            60_000,
            0,
            86_400_000,
        );
        let udp_flow_idle_timeout =
            (udp_flow_idle_timeout_ms > 0).then(|| Duration::from_millis(udp_flow_idle_timeout_ms));

        // Stack limits (defense in depth).
        //
        // Note: use the stack defaults as the base so config.rs doesn't drift if the stack changes.
        let stack_defaults = StackConfig::default();
        let stack_max_tcp_connections = read_env_u32_clamped(
            "AERO_L2_STACK_MAX_TCP_CONNECTIONS",
            stack_defaults.max_tcp_connections,
            0,
            65_535,
        );
        let stack_max_pending_dns = read_env_u32_clamped(
            "AERO_L2_STACK_MAX_PENDING_DNS",
            stack_defaults.max_pending_dns,
            0,
            65_535,
        );
        let stack_max_dns_cache_entries = read_env_u32_clamped(
            "AERO_L2_STACK_MAX_DNS_CACHE_ENTRIES",
            stack_defaults.max_dns_cache_entries,
            0,
            1_000_000,
        );
        let stack_max_buffered_tcp_bytes_per_conn = read_env_u32_clamped(
            "AERO_L2_STACK_MAX_BUFFERED_TCP_BYTES_PER_CONN",
            stack_defaults.max_buffered_tcp_bytes_per_conn,
            0,
            64 * 1024 * 1024,
        );

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

        let security = SecurityConfig::from_env().context("parse security config")?;

        let policy = EgressPolicy::from_env().context("parse egress policy")?;
        let test_overrides = TestOverrides::from_env().context("parse test-mode overrides")?;

        Ok(Self {
            bind_addr,
            l2_max_frame_payload,
            l2_max_control_payload,
            shutdown_grace,
            ping_interval,
            tcp_connect_timeout,
            tcp_send_buffer,
            ws_send_buffer,
            max_udp_flows_per_tunnel,
            udp_flow_idle_timeout,
            stack_max_tcp_connections,
            stack_max_pending_dns,
            stack_max_dns_cache_entries,
            stack_max_buffered_tcp_bytes_per_conn,
            dns_default_ttl_secs,
            dns_max_ttl_secs,
            capture_dir,
            security,
            policy,
            test_overrides,
        })
    }
}

fn read_env_usize_clamped(key: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(min as u64, max as u64) as usize)
        .unwrap_or(default)
}

fn read_env_u32_clamped(key: &str, default: u32, min: u32, max: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(min as u64, max as u64) as u32)
        .unwrap_or(default)
}

fn read_env_u64_clamped(key: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(min, max))
        .unwrap_or(default)
}

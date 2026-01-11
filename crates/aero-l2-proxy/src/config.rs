use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{anyhow, Context, Result};

use crate::{overrides::TestOverrides, policy::EgressPolicy};

#[derive(Debug, Clone)]
pub struct SecurityConfig {
    /// Disable Origin enforcement (trusted local development only).
    pub open: bool,
    /// Comma-separated allowlist of exact Origin strings. `"*"` allows any Origin value (but still
    /// requires the header unless `open=1`).
    pub allowed_origins: AllowedOrigins,
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
            auth_mode: AuthMode::None,
            api_key: None,
            jwt_secret: None,
            session_secret: None,
            max_connections: 64,
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
                    let normalized = crate::origin::normalize_origin(entry).ok_or_else(|| {
                        anyhow::anyhow!(
                            "invalid origin {entry:?} (expected an origin like \"https://example.com\")"
                        )
                    })?;
                    out.push(normalized);
                }
                if any {
                    AllowedOrigins::Any
                } else {
                    AllowedOrigins::List(out)
                }
            }
            None => AllowedOrigins::List(Vec::new()),
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

        Ok(Self {
            open,
            allowed_origins,
            auth_mode,
            api_key,
            jwt_secret,
            session_secret,
            max_connections,
            max_bytes_per_connection,
            max_frames_per_second,
        })
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
            dns_default_ttl_secs,
            dns_max_ttl_secs,
            capture_dir,
            security,
            policy,
            test_overrides,
        })
    }
}

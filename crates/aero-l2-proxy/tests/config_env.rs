#![cfg(not(target_arch = "wasm32"))]

use std::sync::Mutex;

use aero_l2_proxy::{AuthMode, ProxyConfig, SecurityConfig};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }

    fn unset(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prior }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn reset_common_env() -> Vec<EnvVarGuard> {
    vec![
        EnvVarGuard::unset("AERO_L2_TOKEN"),
        EnvVarGuard::unset("AERO_L2_API_KEY"),
        EnvVarGuard::unset("AERO_L2_JWT_SECRET"),
        EnvVarGuard::unset("AERO_L2_AUTH_MODE"),
        EnvVarGuard::unset("AERO_L2_INSECURE_ALLOW_NO_AUTH"),
        EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET"),
        EnvVarGuard::unset("SESSION_SECRET"),
        EnvVarGuard::unset("AERO_L2_SESSION_SECRET"),
        EnvVarGuard::unset("AERO_L2_MAX_FRAME_PAYLOAD"),
        EnvVarGuard::unset("AERO_L2_MAX_FRAME_SIZE"),
        EnvVarGuard::unset("AERO_L2_MAX_CONTROL_PAYLOAD"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS"),
        EnvVarGuard::unset("ALLOWED_ORIGINS"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS"),
        EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST"),
        EnvVarGuard::unset("AERO_L2_OPEN"),
        EnvVarGuard::unset("AERO_L2_TCP_SEND_BUFFER"),
        EnvVarGuard::unset("AERO_L2_WS_SEND_BUFFER"),
    ]
}

#[test]
fn session_secret_prefers_gateway_secret() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session");
    let _explicit = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "l2");
    let _fallback = EnvVarGuard::set("SESSION_SECRET", "session");
    let _gateway = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "gateway");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
    assert_eq!(cfg.session_secret.as_deref(), Some(b"gateway".as_slice()));
}

#[test]
fn session_secret_falls_back_to_session_secret_when_gateway_blank() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session");
    // Blank values should be treated as "unset" so deployments can pass through empty env vars.
    let _gateway = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "   ");
    let _fallback = EnvVarGuard::set("SESSION_SECRET", "session");
    let _l2 = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "l2");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
    assert_eq!(cfg.session_secret.as_deref(), Some(b"session".as_slice()));
}

#[test]
fn session_secret_falls_back_to_aero_l2_session_secret() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session");
    let _gateway = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "");
    let _session = EnvVarGuard::set("SESSION_SECRET", "   ");
    let _l2 = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "l2");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
    assert_eq!(cfg.session_secret.as_deref(), Some(b"l2".as_slice()));
}

#[test]
fn auth_mode_session_requires_secret() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session");
    SecurityConfig::from_env().expect_err("expected session mode to require a session secret");
}

#[test]
fn auth_mode_cookie_alias_is_accepted() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    let _secret = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "sekrit");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
}

#[test]
fn auth_mode_token_requires_token() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "token");
    SecurityConfig::from_env().expect_err("expected token mode to require AERO_L2_TOKEN");
}

#[test]
fn auth_mode_api_key_alias_is_accepted() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "api_key");
    let _token = EnvVarGuard::set("AERO_L2_API_KEY", "sekrit");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::ApiKey);
    assert_eq!(cfg.api_key.as_deref(), Some("sekrit"));
}

#[test]
fn auth_mode_session_and_token_requires_secret_and_token() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session_and_token");

    {
        let _secret = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "sekrit");
        SecurityConfig::from_env()
            .expect_err("expected session_and_token mode to reject missing token");
    }

    {
        let _token = EnvVarGuard::set("AERO_L2_TOKEN", "tok");
        SecurityConfig::from_env()
            .expect_err("expected session_and_token mode to reject missing session secret");
    }

    let _secret = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "sekrit");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "tok");
    let cfg = SecurityConfig::from_env().expect("expected config to accept session_and_token");
    assert_eq!(cfg.auth_mode, AuthMode::CookieAndApiKey);
}

#[test]
fn auth_mode_empty_string_is_treated_as_unset() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");
    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "   ");
    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::ApiKey);
}

#[test]
fn default_auth_mode_requires_explicit_escape_hatch_when_unconfigured() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    // No auth mode, no secrets, no token should fail.
    SecurityConfig::from_env().expect_err("expected config to fail without any auth configured");

    // Explicitly allowing unauthenticated access is possible via the dev escape hatch.
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _insecure = EnvVarGuard::set("AERO_L2_INSECURE_ALLOW_NO_AUTH", "1");
    let cfg = SecurityConfig::from_env().expect("expected config to allow unauthenticated access");
    assert_eq!(cfg.auth_mode, AuthMode::None);
}

#[test]
fn proxy_config_clamps_zero_send_buffer_sizes() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _insecure = EnvVarGuard::set("AERO_L2_INSECURE_ALLOW_NO_AUTH", "1");

    let _tcp_send_buffer = EnvVarGuard::set("AERO_L2_TCP_SEND_BUFFER", "0");
    let _ws_send_buffer = EnvVarGuard::set("AERO_L2_WS_SEND_BUFFER", "0");

    let cfg = ProxyConfig::from_env().expect("expected config to accept zero send buffer sizes");
    assert_eq!(
        cfg.tcp_send_buffer, 32,
        "tcp_send_buffer should fall back to default when zero"
    );
    assert_eq!(
        cfg.ws_send_buffer, 64,
        "ws_send_buffer should fall back to default when zero"
    );
}

#[test]
fn proxy_config_ignores_zero_l2_payload_limits() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _insecure = EnvVarGuard::set("AERO_L2_INSECURE_ALLOW_NO_AUTH", "1");

    // `0` is invalid for payload caps (OpenAPI min=1); treat it as unset so deployments that pass
    // through empty/placeholder env vars don't accidentally disable framing.
    let _max_frame = EnvVarGuard::set("AERO_L2_MAX_FRAME_PAYLOAD", "0");
    let _max_control = EnvVarGuard::set("AERO_L2_MAX_CONTROL_PAYLOAD", "0");

    let cfg = ProxyConfig::from_env().expect("expected config to ignore zero payload limits");
    assert_eq!(
        cfg.l2_max_frame_payload,
        aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD
    );
    assert_eq!(
        cfg.l2_max_control_payload,
        aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD
    );
}

#[test]
fn proxy_config_uses_legacy_frame_size_when_payload_unset() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _insecure = EnvVarGuard::set("AERO_L2_INSECURE_ALLOW_NO_AUTH", "1");

    // If the canonical env var is present but empty/zero (common when passing through `${VAR:-}`),
    // fall back to the legacy alias.
    let _max_frame_payload = EnvVarGuard::set("AERO_L2_MAX_FRAME_PAYLOAD", "0");
    let _max_frame_size = EnvVarGuard::set("AERO_L2_MAX_FRAME_SIZE", "9000");

    let cfg = ProxyConfig::from_env().expect("expected config to fall back to legacy alias");
    assert_eq!(cfg.l2_max_frame_payload, 9000);
}

#[test]
fn proxy_config_accepts_custom_l2_payload_limits() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _insecure = EnvVarGuard::set("AERO_L2_INSECURE_ALLOW_NO_AUTH", "1");

    let _max_frame = EnvVarGuard::set("AERO_L2_MAX_FRAME_PAYLOAD", "1234");
    let _max_control = EnvVarGuard::set("AERO_L2_MAX_CONTROL_PAYLOAD", "99");

    let cfg = ProxyConfig::from_env().expect("expected config to accept custom payload limits");
    assert_eq!(cfg.l2_max_frame_payload, 1234);
    assert_eq!(cfg.l2_max_control_payload, 99);
}

use std::sync::Mutex;

use aero_l2_proxy::{AuthMode, SecurityConfig};

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
        EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS"),
        EnvVarGuard::unset("ALLOWED_ORIGINS"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS"),
        EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST"),
        EnvVarGuard::unset("AERO_L2_OPEN"),
    ]
}

#[test]
fn cookie_session_secret_prefers_explicit_aero_l2_session_secret() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    let _explicit = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "explicit");
    let _fallback = EnvVarGuard::set("SESSION_SECRET", "fallback");
    let _gateway = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "gateway");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
    assert_eq!(cfg.session_secret.as_deref(), Some(b"explicit".as_slice()));
}

#[test]
fn cookie_session_secret_falls_back_to_session_secret_when_aero_l2_session_secret_blank() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    // Blank values should be treated as "unset" so deployments can pass through empty env vars.
    let _explicit = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "   ");
    let _fallback = EnvVarGuard::set("SESSION_SECRET", "fallback");
    let _gateway = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "gateway");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
    assert_eq!(cfg.session_secret.as_deref(), Some(b"fallback".as_slice()));
}

#[test]
fn cookie_session_secret_falls_back_to_gateway_alias() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guards = reset_common_env();

    let _auth = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    let _explicit = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "");
    let _session = EnvVarGuard::unset("SESSION_SECRET");
    let _gateway = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "gateway");

    let cfg = SecurityConfig::from_env().unwrap();
    assert_eq!(cfg.auth_mode, AuthMode::Cookie);
    assert_eq!(cfg.session_secret.as_deref(), Some(b"gateway".as_slice()));
}

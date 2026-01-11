use std::{env, path::PathBuf};

use crate::capture::CaptureConfig;

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub admin_api_key: Option<String>,
    pub capture: Option<CaptureConfig>,
}

impl GatewayConfig {
    pub fn from_env() -> Self {
        let admin_api_key = env::var("ADMIN_API_KEY").ok().filter(|v| !v.is_empty());

        let capture_dir = env::var("CAPTURE_DIR").ok().filter(|v| !v.is_empty());
        let capture = capture_dir.map(|dir| CaptureConfig {
            dir: PathBuf::from(dir),
            max_bytes: parse_env_u64("CAPTURE_MAX_BYTES").unwrap_or(1024 * 1024 * 1024),
            max_files: parse_env_usize("CAPTURE_MAX_FILES").unwrap_or(512),
        });

        Self {
            admin_api_key,
            capture,
        }
    }
}

fn parse_env_u64(name: &str) -> Option<u64> {
    env::var(name).ok().and_then(|v| v.parse().ok())
}

fn parse_env_usize(name: &str) -> Option<usize> {
    env::var(name).ok().and_then(|v| v.parse().ok())
}

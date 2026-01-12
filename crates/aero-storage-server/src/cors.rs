use axum::http::{
    header,
    HeaderMap,
    HeaderName,
    HeaderValue,
};
use std::{collections::HashSet, sync::Arc, time::Duration};

use crate::headers::append_vary;

const DEFAULT_PREFLIGHT_MAX_AGE: Duration = Duration::from_secs(60 * 60 * 24); // 24 hours

#[derive(Debug, Clone)]
enum CorsAllowlist {
    Any,
    Origins(Arc<HashSet<String>>),
}

impl Default for CorsAllowlist {
    fn default() -> Self {
        Self::Any
    }
}

/// CORS configuration for `aero-storage-server`.
///
/// This is a lightweight CORS implementation tailored to Aero's needs. Notably:
/// - Supports an allowlist of origins **or** `*`.
/// - When configured as `*`, we never send `Access-Control-Allow-Credentials`.
/// - When configured with an allowlist, we echo back the request `Origin` only if it is allowed.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    allowlist: CorsAllowlist,
    allow_credentials: bool,
    preflight_max_age: Duration,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowlist: CorsAllowlist::Any,
            allow_credentials: false,
            preflight_max_age: DEFAULT_PREFLIGHT_MAX_AGE,
        }
    }
}

impl CorsConfig {
    pub fn with_allow_credentials(mut self, allow_credentials: bool) -> Self {
        self.allow_credentials = allow_credentials;
        self
    }

    pub fn with_preflight_max_age(mut self, preflight_max_age: Duration) -> Self {
        self.preflight_max_age = preflight_max_age;
        self
    }

    pub fn with_allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = HashSet::new();
        for origin in origins {
            let origin = normalize_origin(origin.as_ref());
            if origin == "*" {
                self.allowlist = CorsAllowlist::Any;
                return self;
            }
            if !origin.is_empty() {
                set.insert(origin);
            }
        }
        self.allowlist = CorsAllowlist::Origins(Arc::new(set));
        self
    }

    pub fn with_allow_origin(mut self, origin: HeaderValue) -> Self {
        if origin == HeaderValue::from_static("*") {
            self.allowlist = CorsAllowlist::Any;
            return self;
        }
        let s = origin.to_str().unwrap_or_default();
        self = self.with_allowed_origins([s]);
        self
    }

    fn resolve_allow_origin(&self, req_headers: &HeaderMap) -> Option<HeaderValue> {
        match &self.allowlist {
            CorsAllowlist::Any => Some(HeaderValue::from_static("*")),
            CorsAllowlist::Origins(allowed) => {
                let origin = req_headers.get(header::ORIGIN)?;
                let origin_str = origin.to_str().ok()?;
                let origin_norm = normalize_origin(origin_str);
                if allowed.contains(&origin_norm) {
                    Some(origin.clone())
                } else {
                    None
                }
            }
        }
    }

    fn should_send_allow_credentials(&self, allow_origin: &HeaderValue) -> bool {
        self.allow_credentials && *allow_origin != HeaderValue::from_static("*")
    }

    pub fn insert_cors_headers(
        &self,
        resp_headers: &mut HeaderMap,
        req_headers: &HeaderMap,
        expose_headers: Option<HeaderValue>,
    ) {
        if let Some(allow_origin) = self.resolve_allow_origin(req_headers) {
            resp_headers.insert(
                HeaderName::from_static("access-control-allow-origin"),
                allow_origin.clone(),
            );
            if self.should_send_allow_credentials(&allow_origin) {
                resp_headers.insert(
                    HeaderName::from_static("access-control-allow-credentials"),
                    HeaderValue::from_static("true"),
                );
            }
            if let Some(expose) = expose_headers {
                resp_headers.insert(
                    HeaderName::from_static("access-control-expose-headers"),
                    expose,
                );
            }
        }

        // Only vary on Origin when the response is origin-dependent. In public `*` mode, varying
        // on Origin fragments CDN caches without providing any correctness benefit.
        if matches!(self.allowlist, CorsAllowlist::Origins(_)) {
            append_vary(resp_headers, &["Origin"]);
        }
    }

    pub fn insert_cors_preflight_headers(
        &self,
        resp_headers: &mut HeaderMap,
        req_headers: &HeaderMap,
        allow_methods: HeaderValue,
        allow_headers: HeaderValue,
    ) {
        if let Some(allow_origin) = self.resolve_allow_origin(req_headers) {
            resp_headers.insert(
                HeaderName::from_static("access-control-allow-origin"),
                allow_origin.clone(),
            );
            if self.should_send_allow_credentials(&allow_origin) {
                resp_headers.insert(
                    HeaderName::from_static("access-control-allow-credentials"),
                    HeaderValue::from_static("true"),
                );
            }
            resp_headers.insert(
                HeaderName::from_static("access-control-allow-methods"),
                allow_methods,
            );
            resp_headers.insert(
                HeaderName::from_static("access-control-allow-headers"),
                allow_headers,
            );
            let secs = self.preflight_max_age.as_secs();
            resp_headers.insert(
                HeaderName::from_static("access-control-max-age"),
                HeaderValue::from_str(&secs.to_string()).unwrap(),
            );
        }

        // Preflight responses are cacheable and must vary on the incoming preflight request
        // headers. Only vary on `Origin` when the `Access-Control-Allow-Origin` value is
        // origin-dependent (allowlist mode).
        let mut tokens = vec!["Access-Control-Request-Method", "Access-Control-Request-Headers"];
        if matches!(self.allowlist, CorsAllowlist::Origins(_)) {
            tokens.insert(0, "Origin");
        }
        append_vary(resp_headers, &tokens);
    }
}

fn normalize_origin(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_ascii_lowercase()
}

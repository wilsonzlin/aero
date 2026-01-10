use std::{collections::HashSet, env, sync::Mutex, time::Duration};

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder,
};

/// Prometheus metrics used by the storage server.
///
/// ## Cardinality notes
///
/// `image_bytes_served_total{image_id=...}` is a potentially high-cardinality metric if `image_id`
/// is unbounded (e.g. user-provided or UUID per request).
///
/// By default, this implementation *caps* the number of distinct `image_id` label values that will
/// be recorded (default: 100, configurable via `AERO_IMAGE_METRICS_MAX_IDS`). Once the cap is
/// reached, any additional image IDs are aggregated into `image_id="__other__"`.
pub struct Metrics {
    registry: Registry,

    http_requests_total: IntCounterVec,
    http_request_duration_seconds: HistogramVec,
    image_bytes_served_total: IntCounterVec,
    range_requests_total: IntCounterVec,
    store_errors_total: IntCounterVec,

    max_image_ids: usize,
    known_image_ids: Mutex<HashSet<String>>,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let build_info = IntGaugeVec::new(
            Opts::new(
                "aero_storage_server_build_info",
                "Build information for aero-storage-server.",
            ),
            &["version"],
        )
        .expect("aero_storage_server_build_info metric must be valid");
        registry
            .register(Box::new(build_info.clone()))
            .expect("aero_storage_server_build_info must register");
        build_info
            .with_label_values(&[env!("CARGO_PKG_VERSION")])
            .set(1);

        let http_requests_total = IntCounterVec::new(
            Opts::new("http_requests_total", "Total number of HTTP requests."),
            &["route", "method", "status"],
        )
        .expect("http_requests_total metric must be valid");
        registry
            .register(Box::new(http_requests_total.clone()))
            .expect("http_requests_total must register");

        let http_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds.",
            )
            .buckets(vec![
                0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["route", "method"],
        )
        .expect("http_request_duration_seconds metric must be valid");
        registry
            .register(Box::new(http_request_duration_seconds.clone()))
            .expect("http_request_duration_seconds must register");

        let image_bytes_served_total = IntCounterVec::new(
            Opts::new(
                "image_bytes_served_total",
                "Total bytes served for disk images (bounded image_id label cardinality).",
            ),
            &["image_id"],
        )
        .expect("image_bytes_served_total metric must be valid");
        registry
            .register(Box::new(image_bytes_served_total.clone()))
            .expect("image_bytes_served_total must register");

        let range_requests_total = IntCounterVec::new(
            Opts::new(
                "range_requests_total",
                "Total number of HTTP Range requests.",
            ),
            &["result"],
        )
        .expect("range_requests_total metric must be valid");
        registry
            .register(Box::new(range_requests_total.clone()))
            .expect("range_requests_total must register");

        let store_errors_total = IntCounterVec::new(
            Opts::new(
                "store_errors_total",
                "Total number of storage/backend errors.",
            ),
            &["kind"],
        )
        .expect("store_errors_total metric must be valid");
        registry
            .register(Box::new(store_errors_total.clone()))
            .expect("store_errors_total must register");

        let max_image_ids = env::var("AERO_IMAGE_METRICS_MAX_IDS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(100);

        let this = Self {
            registry,
            http_requests_total,
            http_request_duration_seconds,
            image_bytes_served_total,
            range_requests_total,
            store_errors_total,
            max_image_ids,
            known_image_ids: Mutex::new(HashSet::new()),
        };

        // Pre-initialize the fixed/low-cardinality label sets so `/metrics` has stable output even
        // before the first request arrives.
        this.http_requests_total
            .with_label_values(&["/metrics", "GET", "200"]);
        this.http_requests_total
            .with_label_values(&["/v1/images/:image_id", "GET", "200"]);
        this.http_request_duration_seconds
            .with_label_values(&["/metrics", "GET"]);
        this.http_request_duration_seconds
            .with_label_values(&["/v1/images/:image_id", "GET"]);
        this.image_bytes_served_total
            .with_label_values(&["__other__"]);
        this.range_requests_total.with_label_values(&["valid"]);
        this.range_requests_total.with_label_values(&["invalid"]);
        for kind in ["meta", "open_range", "manifest"] {
            this.store_errors_total.with_label_values(&[kind]);
        }

        this
    }

    pub fn observe_http_request(&self, route: &str, method: &str, status: u16, duration: Duration) {
        let status = status.to_string();
        self.http_requests_total
            .with_label_values(&[route, method, &status])
            .inc();
        self.http_request_duration_seconds
            .with_label_values(&[route, method])
            .observe(duration.as_secs_f64());
    }

    pub fn observe_image_bytes_served(&self, image_id: &str, bytes: u64) {
        let label = self.image_id_label(image_id);
        self.image_bytes_served_total
            .with_label_values(&[label])
            .inc_by(bytes);
    }

    pub fn inc_range_request_valid(&self) {
        self.range_requests_total
            .with_label_values(&["valid"])
            .inc();
    }

    pub fn inc_range_request_invalid(&self) {
        self.range_requests_total
            .with_label_values(&["invalid"])
            .inc();
    }

    pub fn inc_store_error(&self, kind: &str) {
        self.store_errors_total.with_label_values(&[kind]).inc();
    }

    pub fn encode(&self) -> Vec<u8> {
        let metric_families = self.registry.gather();
        let encoder = TextEncoder::new();
        let mut buf = Vec::new();
        encoder
            .encode(&metric_families, &mut buf)
            .expect("prometheus encoding must succeed");
        buf
    }

    pub fn metrics_content_type() -> &'static str {
        // Prometheus text exposition format.
        //
        // prometheus::TextEncoder::format_type() returns a `&str` tied to the encoder instance, so
        // we keep the content type as a static constant for convenience when building responses.
        "text/plain; version=0.0.4"
    }

    fn image_id_label<'a>(&'a self, image_id: &'a str) -> &'a str {
        let mut known = self.known_image_ids.lock().expect("mutex poisoned");

        if known.contains(image_id) {
            return image_id;
        }

        if known.len() < self.max_image_ids {
            known.insert(image_id.to_owned());
            return image_id;
        }

        "__other__"
    }
}

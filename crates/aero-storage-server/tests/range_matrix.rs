use aero_storage_server::{
    http::{
        images::{router_with_state, ImagesState},
        range::RangeOptions,
    },
    metrics::Metrics,
    store::LocalFsImageStore,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::{str, sync::Arc};
use tempfile::tempdir;
use tower::ServiceExt;

const IMAGE_ID: &str = "fixture.img";

fn fixture_bytes() -> Vec<u8> {
    (0u8..=255).collect()
}

async fn setup_app() -> (axum::Router, tempfile::TempDir, Vec<u8>) {
    let dir = tempdir().expect("tempdir");
    let fixture = fixture_bytes();
    tokio::fs::write(dir.path().join(IMAGE_ID), &fixture)
        .await
        .expect("write fixture");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_range_options(RangeOptions {
        max_ranges: 16,
        max_total_bytes: 1024 * 1024,
    });

    (router_with_state(state), dir, fixture)
}

fn assert_common_headers(headers: &axum::http::HeaderMap) {
    assert_eq!(
        headers[header::ACCEPT_RANGES].to_str().unwrap(),
        "bytes",
        "Accept-Ranges"
    );
    assert!(
        headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-transform"),
        "Cache-Control must include no-transform"
    );
    assert!(
        !headers.contains_key(header::CONTENT_ENCODING),
        "Content-Encoding must not be set for disk image responses"
    );
}

#[tokio::test]
async fn range_matrix_no_range_single_suffix_unsatisfiable() {
    let (app, _dir, fixture) = setup_app().await;

    struct Case<'a> {
        name: &'a str,
        method: Method,
        range: Option<&'a str>,
        accept_encoding: Option<&'a str>,
        expected_status: StatusCode,
        expected_content_length: Option<u64>,
        expected_content_range: Option<String>,
        expected_body: Vec<u8>,
    }

    let total_len = fixture.len() as u64;
    let last = total_len - 1;

    let cases = vec![
        Case {
            name: "GET without Range",
            method: Method::GET,
            range: None,
            accept_encoding: None,
            expected_status: StatusCode::OK,
            expected_content_length: Some(total_len),
            expected_content_range: None,
            expected_body: fixture.clone(),
        },
        Case {
            name: "HEAD without Range",
            method: Method::HEAD,
            range: None,
            accept_encoding: None,
            expected_status: StatusCode::OK,
            expected_content_length: Some(total_len),
            expected_content_range: None,
            expected_body: vec![],
        },
        Case {
            name: "bytes=0-0",
            method: Method::GET,
            range: Some("bytes=0-0"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(1),
            expected_content_range: Some(format!("bytes 0-0/{total_len}")),
            expected_body: vec![fixture[0]],
        },
        Case {
            name: "bytes=1-3",
            method: Method::GET,
            range: Some("bytes=1-3"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(3),
            expected_content_range: Some(format!("bytes 1-3/{total_len}")),
            expected_body: fixture[1..=3].to_vec(),
        },
        Case {
            name: "bytes=0- (full body but 206)",
            method: Method::GET,
            range: Some("bytes=0-"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(total_len),
            expected_content_range: Some(format!("bytes 0-{last}/{total_len}")),
            expected_body: fixture.clone(),
        },
        Case {
            name: "bytes=<len-1>- (last byte)",
            method: Method::GET,
            range: Some("bytes=255-"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(1),
            expected_content_range: Some(format!("bytes {last}-{last}/{total_len}")),
            expected_body: vec![fixture[last as usize]],
        },
        Case {
            name: "bytes=0-<len> (end clamped)",
            method: Method::GET,
            range: Some("bytes=0-256"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(total_len),
            expected_content_range: Some(format!("bytes 0-{last}/{total_len}")),
            expected_body: fixture.clone(),
        },
        Case {
            name: "bytes=-1 (suffix last byte)",
            method: Method::GET,
            range: Some("bytes=-1"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(1),
            expected_content_range: Some(format!("bytes {last}-{last}/{total_len}")),
            expected_body: vec![fixture[last as usize]],
        },
        Case {
            name: "bytes=-<len> (suffix full body)",
            method: Method::GET,
            range: Some("bytes=-256"),
            accept_encoding: None,
            expected_status: StatusCode::PARTIAL_CONTENT,
            expected_content_length: Some(total_len),
            expected_content_range: Some(format!("bytes 0-{last}/{total_len}")),
            expected_body: fixture.clone(),
        },
        Case {
            name: "unsatisfiable bytes=<len>-",
            method: Method::GET,
            range: Some("bytes=256-"),
            accept_encoding: None,
            expected_status: StatusCode::RANGE_NOT_SATISFIABLE,
            expected_content_length: None,
            expected_content_range: Some(format!("bytes */{total_len}")),
            expected_body: vec![],
        },
        Case {
            name: "unsatisfiable bytes=<len+100>-<len+200>",
            method: Method::GET,
            range: Some("bytes=356-456"),
            accept_encoding: None,
            expected_status: StatusCode::RANGE_NOT_SATISFIABLE,
            expected_content_length: None,
            expected_content_range: Some(format!("bytes */{total_len}")),
            expected_body: vec![],
        },
        Case {
            name: "Accept-Encoding must not trigger Content-Encoding",
            method: Method::GET,
            range: None,
            accept_encoding: Some("gzip, br"),
            expected_status: StatusCode::OK,
            expected_content_length: Some(total_len),
            expected_content_range: None,
            expected_body: fixture.clone(),
        },
    ];

    for case in cases {
        let mut req = Request::builder()
            .method(case.method)
            .uri(format!("/v1/images/{IMAGE_ID}"));

        if let Some(v) = case.range {
            req = req.header(header::RANGE, v);
        }
        if let Some(v) = case.accept_encoding {
            req = req.header(header::ACCEPT_ENCODING, v);
        }

        let res = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), case.expected_status, "{}", case.name);

        if case.expected_status != StatusCode::NO_CONTENT {
            assert_common_headers(res.headers());
        }

        if let Some(expected_len) = case.expected_content_length {
            assert_eq!(
                res.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
                expected_len.to_string(),
                "{}: Content-Length",
                case.name
            );
        }

        match (
            case.expected_content_range.as_deref(),
            res.headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok()),
        ) {
            (None, None) => {}
            (Some(expected), Some(actual)) => {
                assert_eq!(actual, expected, "{}: Content-Range", case.name)
            }
            (expected, actual) => panic!(
                "{}: Content-Range mismatch expected={expected:?} actual={actual:?}",
                case.name
            ),
        }

        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], &case.expected_body[..], "{}: body", case.name);
    }
}

fn parse_boundary(content_type: &str) -> &str {
    content_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("boundary="))
        .map(|b| b.trim_matches('"'))
        .expect("boundary present")
}

fn parse_multipart(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let marker = format!("--{boundary}").into_bytes();

    assert!(
        body.starts_with(&marker),
        "multipart body must start with boundary"
    );

    let mut pos = 0usize;
    let mut parts = Vec::new();

    loop {
        assert!(body[pos..].starts_with(&marker));
        pos += marker.len();

        if body[pos..].starts_with(b"--") {
            pos += 2;
            if body.get(pos..pos + 2) == Some(b"\r\n") {
                pos += 2;
            }
            break;
        }

        assert_eq!(&body[pos..pos + 2], b"\r\n");
        pos += 2;

        let headers_end = body[pos..]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header terminator");
        let headers_bytes = &body[pos..pos + headers_end];
        pos += headers_end + 4;

        let headers_text = str::from_utf8(headers_bytes).expect("ascii headers");
        let mut content_range = None;
        for line in headers_text.split("\r\n") {
            if let Some(v) = line.strip_prefix("Content-Range:") {
                content_range = Some(v.trim().to_string());
            }
        }
        let content_range = content_range.expect("Content-Range header");

        let (start, end) = content_range
            .strip_prefix("bytes ")
            .and_then(|v| v.split_once('/'))
            .and_then(|(range, _total)| range.split_once('-'))
            .map(|(s, e)| (s.parse::<usize>().unwrap(), e.parse::<usize>().unwrap()))
            .expect("Content-Range parse");
        let len = end - start + 1;

        let payload = body[pos..pos + len].to_vec();
        pos += len;

        assert_eq!(&body[pos..pos + 2], b"\r\n");
        pos += 2;

        parts.push((content_range, payload));
    }

    assert_eq!(pos, body.len(), "multipart parser must consume entire body");
    parts
}

#[tokio::test]
async fn range_matrix_multi_range_multipart() {
    let (app, _dir, fixture) = setup_app().await;
    let total_len = fixture.len() as u64;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{IMAGE_ID}"))
                .header(header::RANGE, "bytes=0-0,2-2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_common_headers(res.headers());

    let content_type = res.headers()[header::CONTENT_TYPE].to_str().unwrap();
    assert!(
        content_type.starts_with("multipart/byteranges"),
        "expected multipart/byteranges, got {content_type}"
    );
    let boundary = parse_boundary(content_type).to_string();

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let parts = parse_multipart(&body, &boundary);

    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].0, format!("bytes 0-0/{total_len}"));
    assert_eq!(parts[0].1, vec![fixture[0]]);
    assert_eq!(parts[1].0, format!("bytes 2-2/{total_len}"));
    assert_eq!(parts[1].1, vec![fixture[2]]);
}

#[tokio::test]
async fn range_matrix_multi_range_partial_satisfaction_collapses_to_single_range() {
    let (app, _dir, fixture) = setup_app().await;
    let total_len = fixture.len() as u64;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{IMAGE_ID}"))
                .header(header::RANGE, "bytes=0-0,256-266")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Policy: drop unsatisfiable ranges and collapse to a single-range 206 when only one remains.
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_common_headers(res.headers());
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes 0-0/{total_len}")
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], &[fixture[0]]);
}

#[tokio::test]
async fn cors_preflight_allows_range_request_header() {
    let (app, _dir, _fixture) = setup_app().await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri(format!("/v1/images/{IMAGE_ID}"))
                .header("Origin", "https://example.com")
                .header("Access-Control-Request-Method", "GET")
                .header("Access-Control-Request-Headers", "Range")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert!(res.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .to_ascii_lowercase()
        .contains("range"));
}

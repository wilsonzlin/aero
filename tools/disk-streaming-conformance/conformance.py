#!/usr/bin/env python3
"""
Disk streaming endpoint conformance checks (Range + CORS + auth).

This tool is designed to be pointed at any deployment of the "disk image streaming"
endpoint (local server, staging CDN, prod) and validate it matches what the Aero
emulator expects from a browser `fetch()` client.

No third-party dependencies; Python stdlib only.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import textwrap
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Mapping, Sequence


@dataclass(frozen=True)
class HttpResponse:
    url: str
    status: int
    reason: str
    headers: Mapping[str, str]
    body: bytes


def _collapse_headers(message) -> dict[str, str]:
    # `message` is an `http.client.HTTPMessage` (case-insensitive mapping).
    # Some headers may legally be repeated. For conformance checks we collapse
    # them with ", " like browsers do when exposing to JS.
    collapsed: dict[str, str] = {}
    for key in message.keys():
        values = message.get_all(key) or []
        if not values:
            continue
        collapsed[key.lower()] = ", ".join(v.strip() for v in values if v is not None)
    return collapsed


def _request(
    *,
    url: str,
    method: str,
    headers: Mapping[str, str],
    timeout_s: float,
    follow_redirects: bool = True,
    max_body_bytes: int | None = None,
) -> HttpResponse:
    req = urllib.request.Request(url=url, method=method, headers=dict(headers))
    opener = urllib.request.build_opener()
    if not follow_redirects:
        # Browsers reject redirected CORS preflights ("Redirect is not allowed for a
        # preflight request"). Use a no-redirect opener so 30x comes back as a
        # response we can fail on, rather than being transparently followed.
        class _NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, hdrs, newurl):  # type: ignore[override]
                return None

        opener = urllib.request.build_opener(_NoRedirect())
    try:
        with opener.open(req, timeout=timeout_s) as resp:
            if method == "HEAD":
                body = b""
            elif max_body_bytes is None:
                body = resp.read()
            else:
                body = resp.read(max_body_bytes)
            return HttpResponse(
                url=resp.geturl(),
                status=int(resp.status),
                reason=getattr(resp, "reason", ""),
                headers=_collapse_headers(resp.headers),
                body=body,
            )
    except urllib.error.HTTPError as e:
        if method == "HEAD":
            body = b""
        elif max_body_bytes is None:
            body = e.read()
        else:
            body = e.read(max_body_bytes)
        return HttpResponse(
            url=e.geturl(),
            status=int(e.code),
            reason=str(e.reason),
            headers=_collapse_headers(e.headers),
            body=body,
        )
    except urllib.error.URLError as e:
        raise TestFailure(f"{method} {url}: {e}") from None


def _csv_tokens(value: str) -> set[str]:
    return {token.strip().lower() for token in value.split(",") if token.strip()}


def _media_type(value: str) -> str:
    # `Content-Type` may include parameters; for conformance we only care about the base type.
    return value.split(";", 1)[0].strip().lower()


def _authorization_value(token: str) -> str:
    token = token.strip()
    # Allow either a raw token ("abc...") or a full Authorization header value
    # ("Bearer abc..."). If it contains whitespace, treat it as already-specified.
    if re.search(r"\s", token):
        return token
    return f"Bearer {token}"


def _fmt_bytes(n: int) -> str:
    # Human-readable-ish; keep it simple for CI logs.
    units = ["B", "KiB", "MiB", "GiB", "TiB"]
    value = float(n)
    unit = units[0]
    for next_unit in units[1:]:
        if value < 1024.0:
            break
        value /= 1024.0
        unit = next_unit
    if unit == "B":
        return f"{n} B"
    return f"{value:.2f} {unit}"


class TestFailure(Exception):
    pass


@dataclass
class TestResult:
    name: str
    status: str  # PASS | FAIL | SKIP | WARN
    details: str = ""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise TestFailure(message)


def _header(resp: HttpResponse, name: str) -> str | None:
    return resp.headers.get(name.lower())


def _require_allow_origin(resp: HttpResponse, origin: str) -> None:
    allow_origin = _header(resp, "Access-Control-Allow-Origin")
    _require(allow_origin is not None, "missing Access-Control-Allow-Origin")
    allow_origin = allow_origin.strip()
    _require(
        allow_origin == "*" or allow_origin == origin,
        f"expected Access-Control-Allow-Origin '*' or {origin!r}, got {allow_origin!r}",
    )


def _require_expose_headers(resp: HttpResponse, required: set[str]) -> None:
    expose = _header(resp, "Access-Control-Expose-Headers")
    _require(expose is not None, "missing Access-Control-Expose-Headers")
    tokens = _csv_tokens(expose)
    _require(
        "*" in tokens or required.issubset(tokens),
        f"expected Access-Control-Expose-Headers to include {sorted(required)}, got {expose!r}",
    )


def _require_cors(resp: HttpResponse, origin: str | None, *, expose: set[str] | None = None) -> None:
    if origin is None:
        return
    _require_allow_origin(resp, origin)
    if expose is not None:
        _require_expose_headers(resp, expose)


def _print_result(result: TestResult) -> None:
    line = f"{result.status:<4} {result.name}"
    if result.details:
        line += f" - {result.details}"
    print(line)


def _test_private_requires_auth(
    *,
    base_url: str,
    origin: str | None,
    timeout_s: float,
) -> TestResult:
    name = "private: unauthenticated request is denied (401/403)"
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": "bytes=0-0",
        }
        if origin is not None:
            headers["Origin"] = origin
        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=1024,
        )
        _require_cors(resp, origin)
        _require(resp.status in (401, 403), f"expected 401/403, got {resp.status}")
        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


@dataclass(frozen=True)
class HeadInfo:
    resp: HttpResponse
    size: int
    etag: str | None
    last_modified: str | None


def _test_head(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
) -> tuple[TestResult, HeadInfo | None]:
    name = "HEAD: Accept-Ranges=bytes and Content-Length is present"
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(url=base_url, method="HEAD", headers=headers, timeout_s=timeout_s)
        _require(200 <= resp.status < 300, f"expected 2xx, got {resp.status}")

        accept_ranges = _header(resp, "Accept-Ranges")
        _require(accept_ranges is not None, "missing Accept-Ranges header")
        _require(
            "bytes" in _csv_tokens(accept_ranges),
            f"expected Accept-Ranges to include 'bytes', got {accept_ranges!r}",
        )

        content_length = _header(resp, "Content-Length")
        _require(content_length is not None, "missing Content-Length header")
        try:
            size = int(content_length)
        except ValueError:
            raise TestFailure(f"invalid Content-Length {content_length!r}") from None
        _require(size > 0, f"Content-Length must be > 0, got {size}")
        _require_cors(
            resp,
            origin,
            expose={"accept-ranges", "content-range", "content-length", "etag", "last-modified"},
        )
        etag = _header(resp, "ETag")
        last_modified = _header(resp, "Last-Modified")
        return (
            TestResult(
                name=name,
                status="PASS",
                details=f"size={size} ({_fmt_bytes(size)})",
            ),
            HeadInfo(resp=resp, size=size, etag=etag, last_modified=last_modified),
        )
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e)), None


def _parse_content_range(value: str) -> tuple[int, int, int]:
    # Example: "bytes 0-0/12345"
    m = re.fullmatch(r"\s*bytes\s+(\d+)-(\d+)/(\d+)\s*", value, flags=re.IGNORECASE)
    if not m:
        raise TestFailure(f"invalid Content-Range {value!r}")
    start, end, total = (int(m.group(1)), int(m.group(2)), int(m.group(3)))
    return start, end, total


def _test_get_valid_range(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    size: int | None,
    strict: bool,
) -> TestResult:
    name = "GET: valid Range (first byte) returns 206 with correct Content-Range and body length"
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")

    try:
        req_start = 0
        req_end = 0
        return _test_get_range(
            name=name,
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            req_start=req_start,
            req_end=req_end,
            strict=strict,
        )
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_get_mid_range(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    size: int | None,
    strict: bool,
) -> TestResult:
    name = "GET: valid Range (mid-file) returns 206 with correct Content-Range and body length"
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")
    if size < 2:
        return TestResult(name=name, status="SKIP", details=f"skipped (size too small: {size})")

    try:
        req_start = size // 2
        if req_start <= 0:
            return TestResult(name=name, status="SKIP", details=f"skipped (size too small: {size})")
        req_end = req_start
        return _test_get_range(
            name=name,
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            req_start=req_start,
            req_end=req_end,
            strict=strict,
        )
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_get_range(
    *,
    name: str,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    size: int,
    req_start: int,
    req_end: int,
    strict: bool,
    extra_headers: Mapping[str, str] | None = None,
) -> TestResult:
    _require(0 <= req_start <= req_end < size, f"invalid test range {req_start}-{req_end} for size {size}")
    headers: dict[str, str] = {
        "Accept-Encoding": "identity",
        "Range": f"bytes={req_start}-{req_end}",
    }
    if origin is not None:
        headers["Origin"] = origin
    if authorization is not None:
        headers["Authorization"] = authorization
    if extra_headers is not None:
        for k, v in extra_headers.items():
            headers[str(k)] = str(v)

    # Safety: if the server ignores Range and returns a full 200 response, don't download the whole
    # disk image. We only need `expected_len` bytes to validate conformance.
    expected_len = req_end - req_start + 1
    resp = _request(
        url=base_url,
        method="GET",
        headers=headers,
        timeout_s=timeout_s,
        max_body_bytes=expected_len + 1,
    )
    _require(resp.status == 206, f"expected 206, got {resp.status}")

    accept_ranges = _header(resp, "Accept-Ranges")
    _require(accept_ranges is not None, "missing Accept-Ranges header")
    _require(
        "bytes" in _csv_tokens(accept_ranges),
        f"expected Accept-Ranges to include 'bytes', got {accept_ranges!r}",
    )

    cache_control = _header(resp, "Cache-Control")
    _require(cache_control is not None, "missing Cache-Control header")
    _require(
        "no-transform" in _csv_tokens(cache_control),
        f"expected Cache-Control to include 'no-transform', got {cache_control!r}",
    )

    content_encoding = _header(resp, "Content-Encoding")
    if content_encoding is not None:
        encodings = _csv_tokens(content_encoding)
        _require(
            encodings == {"identity"},
            f"expected Content-Encoding to be absent or 'identity', got {content_encoding!r}",
        )

    content_range = _header(resp, "Content-Range")
    _require(content_range is not None, "missing Content-Range header")
    _require_cors(
        resp,
        origin,
        expose={"accept-ranges", "content-range", "content-length", "etag", "last-modified"},
    )
    start, end, total = _parse_content_range(content_range)
    _require(start == req_start and end == req_end, f"expected bytes {req_start}-{req_end}, got {start}-{end}")
    _require(total == size, f"expected total size {size}, got {total}")

    _require(len(resp.body) == expected_len, f"expected body length {expected_len}, got {len(resp.body)}")

    content_length = _header(resp, "Content-Length")
    if content_length is not None:
        try:
            resp_len = int(content_length)
        except ValueError:
            raise TestFailure(f"invalid Content-Length {content_length!r}") from None
        _require(resp_len == expected_len, f"expected Content-Length {expected_len}, got {resp_len}")

    transfer_encoding = _header(resp, "Transfer-Encoding")
    if transfer_encoding is not None and "chunked" in _csv_tokens(transfer_encoding):
        message = (
            "206 responses should not use Transfer-Encoding: chunked (some CDNs mishandle it); "
            "prefer a fixed Content-Length"
        )
        if strict:
            raise TestFailure(message)
        return TestResult(
            name=name,
            status="WARN",
            details=f"{message}; Transfer-Encoding={transfer_encoding!r}; Content-Range={content_range!r}",
        )

    return TestResult(name=name, status="PASS", details=f"Content-Range={content_range!r}")


def _test_get_unsatisfiable_range(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    size: int | None,
) -> TestResult:
    name = "GET: unsatisfiable Range returns 416 and Content-Range bytes */<size>"
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")

    try:
        # First byte *after* the end of the resource is always unsatisfiable.
        start = size
        end = size + 10
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": f"bytes={start}-{end}",
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        # Safety: if Range is ignored and a full 200 is returned, don't download the whole image.
        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=1024,
        )
        _require(resp.status == 416, f"expected 416, got {resp.status}")

        accept_ranges = _header(resp, "Accept-Ranges")
        _require(accept_ranges is not None, "missing Accept-Ranges header")
        _require(
            "bytes" in _csv_tokens(accept_ranges),
            f"expected Accept-Ranges to include 'bytes', got {accept_ranges!r}",
        )

        content_range = _header(resp, "Content-Range")
        _require(content_range is not None, "missing Content-Range header")
        _require_cors(
            resp,
            origin,
            expose={"accept-ranges", "content-range", "content-length", "etag", "last-modified"},
        )

        # Example: "bytes */12345"
        m = re.fullmatch(r"\s*bytes\s+\*/(\d+)\s*", content_range, flags=re.IGNORECASE)
        _require(m is not None, f"expected Content-Range 'bytes */{size}', got {content_range!r}")
        total = int(m.group(1))
        _require(total == size, f"expected total size {size}, got {total}")

        return TestResult(name=name, status="PASS", details=f"Content-Range={content_range!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_options_preflight(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    etag: str | None,
) -> TestResult:
    required_headers = {"range", "if-range"}
    req_headers: list[str] = ["range", "if-range"]
    if etag is not None:
        required_headers.add("if-none-match")
        req_headers.append("if-none-match")

    req_header_value = ",".join(req_headers)

    name = "OPTIONS: CORS preflight allows Range + If-Range headers"
    if etag is not None:
        name += " + If-None-Match"
    if authorization is not None:
        required_headers.add("authorization")
        req_header_value += ",authorization"
        name += " + Authorization"
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    try:
        resp = _request(
            url=base_url,
            method="OPTIONS",
            headers={
                "Accept-Encoding": "identity",
                "Origin": origin,
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": req_header_value,
            },
            timeout_s=timeout_s,
            follow_redirects=False,
            max_body_bytes=1024,
        )
        _require(200 <= resp.status < 300, f"expected 2xx, got {resp.status}")

        allow_origin = _header(resp, "Access-Control-Allow-Origin")
        _require(allow_origin is not None, "missing Access-Control-Allow-Origin")
        allow_origin = allow_origin.strip()
        _require(
            allow_origin == "*" or allow_origin == origin,
            f"expected Allow-Origin '*' or {origin!r}, got {allow_origin!r}",
        )

        allow_methods = _header(resp, "Access-Control-Allow-Methods")
        _require(allow_methods is not None, "missing Access-Control-Allow-Methods")
        allow_method_tokens = _csv_tokens(allow_methods)
        required_methods = {"get", "head"}
        _require(
            "*" in allow_method_tokens or required_methods.issubset(allow_method_tokens),
            f"expected Allow-Methods to include {sorted(required_methods)} (or '*'); got {allow_methods!r}",
        )

        allow_headers = _header(resp, "Access-Control-Allow-Headers")
        _require(allow_headers is not None, "missing Access-Control-Allow-Headers")
        allowed = _csv_tokens(allow_headers)
        missing_required = set()
        if "*" not in allowed:
            missing_required = required_headers.difference(allowed)
        _require(
            not missing_required,
            f"expected Allow-Headers to include {sorted(required_headers)} (or '*'); missing {sorted(missing_required)}; got {allow_headers!r}",
        )

        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_options_preflight_if_modified_since(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    last_modified: str | None,
) -> TestResult:
    name = "OPTIONS: CORS preflight allows If-Modified-Since header"
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if last_modified is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Last-Modified from HEAD)")

    try:
        req_header_value = "if-modified-since"
        required_headers = {"if-modified-since"}
        if authorization is not None:
            req_header_value += ",authorization"
            required_headers.add("authorization")

        resp = _request(
            url=base_url,
            method="OPTIONS",
            headers={
                "Accept-Encoding": "identity",
                "Origin": origin,
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": req_header_value,
            },
            timeout_s=timeout_s,
            follow_redirects=False,
            max_body_bytes=1024,
        )
        if not (200 <= resp.status < 300):
            return TestResult(name=name, status="WARN", details=f"expected 2xx, got {resp.status}")

        allow_origin = _header(resp, "Access-Control-Allow-Origin")
        if allow_origin is None:
            return TestResult(name=name, status="WARN", details="missing Access-Control-Allow-Origin")
        allow_origin = allow_origin.strip()
        if not (allow_origin == "*" or allow_origin == origin):
            return TestResult(
                name=name,
                status="WARN",
                details=f"expected Allow-Origin '*' or {origin!r}, got {allow_origin!r}",
            )

        allow_methods = _header(resp, "Access-Control-Allow-Methods")
        if allow_methods is None:
            return TestResult(name=name, status="WARN", details="missing Access-Control-Allow-Methods")
        allow_method_tokens = _csv_tokens(allow_methods)
        required_methods = {"get", "head"}
        if not ("*" in allow_method_tokens or required_methods.issubset(allow_method_tokens)):
            return TestResult(
                name=name,
                status="WARN",
                details=f"expected Allow-Methods to include {sorted(required_methods)} (or '*'); got {allow_methods!r}",
            )

        allow_headers = _header(resp, "Access-Control-Allow-Headers")
        if allow_headers is None:
            return TestResult(name=name, status="WARN", details="missing Access-Control-Allow-Headers")
        allowed = _csv_tokens(allow_headers)
        if "*" not in allowed:
            missing = required_headers.difference(allowed)
            if missing:
                return TestResult(
                    name=name,
                    status="WARN",
                    details=f"missing Allow-Headers {sorted(missing)}; got {allow_headers!r}",
                )

        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="WARN", details=str(e))

def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    env_base_url = os.environ.get("BASE_URL")
    env_token = os.environ.get("TOKEN")
    env_origin = os.environ.get("ORIGIN")

    parser = argparse.ArgumentParser(
        prog="disk-streaming-conformance",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=textwrap.dedent(
            """\
            Disk image streaming endpoint conformance checks.

            Required: BASE_URL / --base-url
            Optional: TOKEN / --token, ORIGIN / --origin
            """
        ),
    )
    parser.add_argument("--base-url", default=env_base_url, help="Base URL to the disk image (env: BASE_URL)")
    parser.add_argument("--token", default=env_token, help="Auth token or full Authorization header value (env: TOKEN)")
    parser.add_argument(
        "--origin",
        default=env_origin or "https://example.com",
        help="Origin to simulate for CORS (env: ORIGIN; default: https://example.com)",
    )
    parser.add_argument("--timeout", type=float, default=30.0, help="Request timeout in seconds (default: 30)")
    parser.add_argument(
        "--strict",
        action="store_true",
        help=(
            "Fail on 'WARN' conditions (e.g. Transfer-Encoding: chunked on 206, "
            "missing Cross-Origin-Resource-Policy, private caching without no-store, "
            "If-Range mismatch behavior, CORS misconfigurations like Allow-Credentials with '*')"
        ),
    )
    parser.add_argument(
        "--expect-corp",
        default=None,
        help=(
            "Require Cross-Origin-Resource-Policy to equal this value (e.g. same-site, cross-origin). "
            "If omitted, missing CORP is WARN-only."
        ),
    )
    args = parser.parse_args(argv)

    if not args.base_url:
        parser.error("Missing --base-url (or env BASE_URL)")
    args.base_url = args.base_url.strip()
    args.origin = args.origin.strip()
    if args.origin == "":
        args.origin = None
    if args.token is not None:
        args.token = args.token.strip()
        if args.token == "":
            args.token = None
    if args.expect_corp is not None:
        args.expect_corp = args.expect_corp.strip()
        if args.expect_corp == "":
            args.expect_corp = None
    return args


def _is_weak_etag(etag: str) -> bool:
    return etag.strip().lower().startswith("w/")


def _is_single_etag(etag: str) -> bool:
    # If-Range only accepts a single validator. We conservatively skip if it looks like a list.
    # (ETag values can technically contain commas inside quotes, but it's extremely uncommon.)
    return "," not in etag


def _test_if_range_matches_etag(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    size: int | None,
    etag: str | None,
    strict: bool,
) -> TestResult:
    name = "GET: Range + If-Range (matching ETag) returns 206"
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")
    if etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag from HEAD)")
    if _is_weak_etag(etag):
        return TestResult(
            name=name,
            status="SKIP",
            details="skipped (ETag is weak; If-Range requires a strong ETag)",
        )
    if not _is_single_etag(etag):
        return TestResult(name=name, status="SKIP", details=f"skipped (ETag looks like a list: {etag!r})")

    try:
        return _test_get_range(
            name=name,
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            req_start=0,
            req_end=0,
            strict=strict,
            extra_headers={"If-Range": etag},
        )
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_etag_strength(etag: str | None) -> TestResult:
    name = "HEAD: ETag is strong (recommended for If-Range)"
    if etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag from HEAD)")
    etag = etag.strip()
    if _is_weak_etag(etag):
        return TestResult(
            name=name,
            status="WARN",
            details=f"ETag is weak ({etag!r}); If-Range requires a strong ETag",
        )
    if not etag.startswith('"'):
        return TestResult(name=name, status="WARN", details=f"ETag does not look quoted: {etag!r}")
    return TestResult(name=name, status="PASS")

def _test_if_range_mismatch(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    size: int | None,
    strict: bool,
) -> TestResult:
    name = 'GET: Range + If-Range ("mismatch") does not return mixed-version 206'
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": "bytes=0-0",
            "If-Range": '"mismatch"',
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        # If the server prefers to ignore `Range` and return `200`, that could be many GiB. Don't
        # download it.
        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=1024,
        )
        _require_cors(resp, origin)

        if resp.status == 200:
            content_range = _header(resp, "Content-Range")
            _require(content_range is None, f"expected no Content-Range on 200, got {content_range!r}")
            content_length = _header(resp, "Content-Length")
            if content_length is not None:
                try:
                    resp_len = int(content_length)
                except ValueError:
                    raise TestFailure(f"invalid Content-Length {content_length!r}") from None
                _require(resp_len == size, f"expected full Content-Length {size}, got {resp_len}")
            return TestResult(name=name, status="PASS", details="status=200 (Range ignored)")

        if resp.status == 412:
            message = (
                "server returned 412 Precondition Failed for If-Range mismatch; "
                "spec prefers 200 full body to avoid mixed-version ranges"
            )
            if strict:
                return TestResult(name=name, status="FAIL", details=message)
            return TestResult(name=name, status="WARN", details=message)

        raise TestFailure(f"expected 200 (preferred) or 412, got {resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_conditional_if_none_match(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    etag: str | None,
) -> TestResult:
    name = "GET: If-None-Match returns 304 Not Modified"
    if etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag from HEAD)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "If-None-Match": etag,
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            # Safety: a broken server might ignore If-None-Match and return a giant 200.
            max_body_bytes=1024,
        )
        _require_cors(resp, origin)
        _require(resp.status == 304, f"expected 304, got {resp.status}")
        _require(len(resp.body) == 0, f"expected empty body on 304, got {len(resp.body)} bytes")
        return TestResult(name=name, status="PASS", details="status=304")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_head_conditional_if_none_match(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    etag: str | None,
) -> TestResult:
    name = "HEAD: If-None-Match returns 304 Not Modified"
    if etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag from HEAD)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "If-None-Match": etag,
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(
            url=base_url,
            method="HEAD",
            headers=headers,
            timeout_s=timeout_s,
        )
        _require_cors(resp, origin)
        _require(resp.status == 304, f"expected 304, got {resp.status}")
        _require(len(resp.body) == 0, f"expected empty body on 304, got {len(resp.body)} bytes")
        return TestResult(name=name, status="PASS", details="status=304")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_conditional_if_modified_since(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    last_modified: str | None,
    strict: bool,
) -> TestResult:
    name = "GET: If-Modified-Since returns 304 Not Modified"
    if last_modified is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Last-Modified from HEAD)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "If-Modified-Since": last_modified,
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=1024,
        )
        _require_cors(resp, origin)

        if resp.status == 304:
            _require(len(resp.body) == 0, f"expected empty body on 304, got {len(resp.body)} bytes")
            return TestResult(name=name, status="PASS", details="status=304")

        message = f"expected 304, got {resp.status}"
        if strict:
            return TestResult(name=name, status="FAIL", details=message)
        return TestResult(name=name, status="WARN", details=message)
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_head_conditional_if_modified_since(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    last_modified: str | None,
    strict: bool,
) -> TestResult:
    name = "HEAD: If-Modified-Since returns 304 Not Modified"
    if last_modified is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Last-Modified from HEAD)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "If-Modified-Since": last_modified,
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(
            url=base_url,
            method="HEAD",
            headers=headers,
            timeout_s=timeout_s,
        )
        _require_cors(resp, origin)

        if resp.status == 304:
            return TestResult(name=name, status="PASS", details="status=304")

        message = f"expected 304, got {resp.status}"
        if strict:
            return TestResult(name=name, status="FAIL", details=message)
        return TestResult(name=name, status="WARN", details=message)
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_content_headers(
    *,
    name: str,
    resp: HttpResponse | None,
) -> TestResult:
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")

    issues: list[str] = []

    content_type = _header(resp, "Content-Type")
    if content_type is None:
        issues.append("missing Content-Type")
    else:
        media_type = _media_type(content_type)
        if media_type != "application/octet-stream":
            issues.append(f"unexpected Content-Type {content_type!r} (expected application/octet-stream)")

    xcto = _header(resp, "X-Content-Type-Options")
    if xcto is None:
        issues.append("missing X-Content-Type-Options (recommended: nosniff)")
    else:
        if xcto.strip().lower() != "nosniff":
            issues.append(f"unexpected X-Content-Type-Options {xcto!r} (expected 'nosniff')")

    cache_control = _header(resp, "Cache-Control")
    if cache_control is None:
        issues.append("missing Cache-Control (recommended: include no-transform)")
    else:
        if "no-transform" not in _csv_tokens(cache_control):
            issues.append(f"Cache-Control missing no-transform: {cache_control!r}")

    if issues:
        return TestResult(name=name, status="WARN", details="; ".join(issues))
    return TestResult(name=name, status="PASS")


def _test_get_content_headers(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
) -> TestResult:
    name = "GET: Content-Type is application/octet-stream and X-Content-Type-Options=nosniff"
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": "bytes=0-0",
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=2,
        )
        _require(resp.status == 206, f"expected 206, got {resp.status}")
        _require_cors(resp, origin)
        return _test_content_headers(name=name, resp=resp)
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_cors_vary_origin(
    *,
    resp: HttpResponse | None,
    origin: str | None,
) -> TestResult:
    name = "CORS: Vary includes Origin when Allow-Origin echoes a specific origin"
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")

    try:
        allow_origin = _header(resp, "Access-Control-Allow-Origin")
        _require(allow_origin is not None, "missing Access-Control-Allow-Origin")
        allow_origin = allow_origin.strip()
        if allow_origin == "*":
            return TestResult(name=name, status="SKIP", details="skipped (Allow-Origin is '*')")
        if allow_origin != origin:
            raise TestFailure(f"expected Allow-Origin {origin!r}, got {allow_origin!r}")

        vary = _header(resp, "Vary")
        if vary is None:
            return TestResult(name=name, status="WARN", details="missing Vary header (expected 'Origin')")
        tokens = _csv_tokens(vary)
        if "origin" not in tokens and "*" not in tokens:
            return TestResult(
                name=name,
                status="WARN",
                details=f"expected Vary to include 'Origin', got {vary!r}",
            )
        return TestResult(name=name, status="PASS", details=f"Vary={vary!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_cors_allow_credentials_sane(
    *,
    resp: HttpResponse | None,
    origin: str | None,
) -> TestResult:
    name = "CORS: Allow-Credentials does not contradict Allow-Origin"
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")

    try:
        allow_origin = _header(resp, "Access-Control-Allow-Origin")
        _require(allow_origin is not None, "missing Access-Control-Allow-Origin")
        allow_origin = allow_origin.strip()

        allow_credentials = _header(resp, "Access-Control-Allow-Credentials")
        if allow_credentials is None:
            return TestResult(name=name, status="PASS", details="(no Allow-Credentials)")
        ac = allow_credentials.strip().lower()
        if ac != "true":
            return TestResult(name=name, status="WARN", details=f"unexpected Allow-Credentials={allow_credentials!r}")
        if allow_origin == "*":
            return TestResult(
                name=name,
                status="WARN",
                details="Allow-Credentials=true with Allow-Origin='*' will not work for credentialed fetches",
            )
        return TestResult(name=name, status="PASS", details=f"Allow-Credentials={allow_credentials!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_corp_header(
    *,
    name: str,
    resp: HttpResponse | None,
    expect_corp: str | None,
) -> TestResult:
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")

    corp = _header(resp, "Cross-Origin-Resource-Policy")

    if expect_corp is not None:
        if corp is None:
            return TestResult(name=name, status="FAIL", details="missing Cross-Origin-Resource-Policy header")
        actual = corp.strip().lower()
        expected = expect_corp.strip().lower()
        if actual != expected:
            return TestResult(name=name, status="FAIL", details=f"expected {expected!r}, got {actual!r}")
        return TestResult(name=name, status="PASS", details=f"value={corp!r}")

    if corp is None:
        return TestResult(
            name=name,
            status="WARN",
            details=(
                "missing Cross-Origin-Resource-Policy header "
                "(recommended for COEP: require-corp defence-in-depth)"
            ),
        )

    actual = corp.strip().lower()
    allowed = {"same-origin", "same-site", "cross-origin"}
    if actual not in allowed:
        return TestResult(
            name=name,
            status="WARN",
            details=f"unexpected Cross-Origin-Resource-Policy value {corp!r} (expected one of {sorted(allowed)})",
        )

    return TestResult(name=name, status="PASS", details=f"value={corp!r}")


def _test_corp_on_get(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    expect_corp: str | None,
) -> TestResult:
    name = "GET: Cross-Origin-Resource-Policy is set"
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": "bytes=0-0",
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization
        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=2,
        )
        # If Range is supported this should be 206, but CORP is meaningful on any successful GET.
        _require(200 <= resp.status < 400, f"expected <400, got {resp.status}")
        return _test_corp_header(name=name, resp=resp, expect_corp=expect_corp)
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_private_cache_control(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    strict: bool,
) -> TestResult:
    name = "private: 206 responses are not publicly cacheable (Cache-Control)"
    if authorization is None:
        return TestResult(name=name, status="SKIP", details="skipped (no --token provided)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": "bytes=0-0",
            "Authorization": authorization,
        }
        if origin is not None:
            headers["Origin"] = origin

        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=2,
        )
        _require_cors(resp, origin)
        _require(resp.status == 206, f"expected 206, got {resp.status}")

        cache_control = _header(resp, "Cache-Control")
        _require(cache_control is not None, "missing Cache-Control header")
        tokens = _csv_tokens(cache_control)
        if "public" in tokens:
            raise TestFailure(f"private response must not be Cache-Control: public; got {cache_control!r}")
        if "no-store" in tokens:
            return TestResult(name=name, status="PASS", details=f"Cache-Control={cache_control!r}")

        message = (
            "private response Cache-Control does not include 'no-store'. "
            "This is risky for browser/intermediary caching unless you intentionally enforce auth at the edge. "
            "Run with --strict to fail."
        )
        if strict:
            return TestResult(name=name, status="FAIL", details=f"{message} Cache-Control={cache_control!r}")
        return TestResult(name=name, status="WARN", details=f"{message} Cache-Control={cache_control!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def main(argv: Sequence[str]) -> int:
    args = _parse_args(argv)
    base_url: str = args.base_url
    origin: str | None = args.origin
    timeout_s: float = args.timeout
    strict: bool = bool(args.strict)
    expect_corp: str | None = args.expect_corp

    token: str | None = args.token
    authorization: str | None = _authorization_value(token) if token else None

    print("Disk streaming conformance")
    print(f"  BASE_URL: {base_url}")
    print(f"  ORIGIN:   {origin or '(none)'}")
    print(f"  STRICT:   {strict}")
    print(f"  CORP:     {expect_corp or '(not required)'}")
    if authorization is None:
        print("  AUTH:     (none)")
    else:
        # Don't print the actual token; CI logs are forever.
        print("  AUTH:     provided (token hidden)")
    print()

    results: list[TestResult] = []

    if authorization is not None:
        results.append(_test_private_requires_auth(base_url=base_url, origin=origin, timeout_s=timeout_s))

    head_result, head_info = _test_head(
        base_url=base_url,
        origin=origin,
        authorization=authorization,
        timeout_s=timeout_s,
    )
    results.append(head_result)
    size = head_info.size if head_info is not None else None
    etag = head_info.etag if head_info is not None else None
    last_modified = head_info.last_modified if head_info is not None else None

    results.append(_test_etag_strength(etag))
    results.append(
        _test_content_headers(
            name="HEAD: Content-Type is application/octet-stream and X-Content-Type-Options=nosniff",
            resp=head_info.resp if head_info is not None else None,
        )
    )
    results.append(
        _test_cors_allow_credentials_sane(
            resp=head_info.resp if head_info is not None else None,
            origin=origin,
        )
    )
    results.append(
        _test_cors_vary_origin(
            resp=head_info.resp if head_info is not None else None,
            origin=origin,
        )
    )

    results.append(
        _test_head_conditional_if_none_match(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            etag=etag,
        )
    )
    results.append(
        _test_head_conditional_if_modified_since(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            last_modified=last_modified,
            strict=strict,
        )
    )

    results.append(
        _test_corp_header(
            name="HEAD: Cross-Origin-Resource-Policy is set",
            resp=head_info.resp if head_info is not None else None,
            expect_corp=expect_corp,
        )
    )
    results.append(
        _test_corp_on_get(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            expect_corp=expect_corp,
        )
    )
    results.append(
        _test_get_content_headers(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
        )
    )

    results.append(
        _test_get_valid_range(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            strict=strict,
        )
    )
    results.append(
        _test_private_cache_control(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            strict=strict,
        )
    )
    results.append(
        _test_get_mid_range(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            strict=strict,
        )
    )
    results.append(
        _test_get_unsatisfiable_range(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
        )
    )
    results.append(
        _test_if_range_matches_etag(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            etag=etag,
            strict=strict,
        )
    )
    results.append(
        _test_if_range_mismatch(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
            strict=strict,
        )
    )
    results.append(
        _test_conditional_if_none_match(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            etag=etag,
        )
    )
    results.append(
        _test_conditional_if_modified_since(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            last_modified=last_modified,
            strict=strict,
        )
    )
    results.append(
        _test_options_preflight(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            etag=etag,
        )
    )
    results.append(
        _test_options_preflight_if_modified_since(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            last_modified=last_modified,
        )
    )

    for result in results:
        _print_result(result)

    failed = [r for r in results if r.status == "FAIL"]
    warned = [r for r in results if r.status == "WARN"]
    skipped = [r for r in results if r.status == "SKIP"]
    passed = [r for r in results if r.status == "PASS"]

    print()
    print(
        f"Summary: {len(passed)} passed, {len(failed)} failed, {len(warned)} warned, {len(skipped)} skipped"
    )

    return 1 if failed or (strict and warned) else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

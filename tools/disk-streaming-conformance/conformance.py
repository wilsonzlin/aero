#!/usr/bin/env python3
"""
Disk streaming endpoint conformance checks (Range + chunked + CORS + auth).

This tool is designed to be pointed at any deployment of the disk image delivery
endpoints (local server, staging CDN, prod) and validate it matches what the Aero
emulator expects from a browser `fetch()` client.

Supported modes:

- Range mode (`--mode range`, default): one large object served via HTTP Range.
- Chunked mode (`--mode chunked`): `manifest.json` + chunk objects under `chunks/`
  (see `docs/18-chunked-disk-image-format.md`).

No third-party dependencies; Python stdlib only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import sys
import textwrap
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Mapping, Sequence

# Browsers automatically send a non-identity Accept-Encoding (and scripts cannot override it).
# Conformance checks must therefore emulate a browser-like Accept-Encoding so we detect any
# CDN/object-store transforms that would break byte-addressed disk streaming.
_BROWSER_ACCEPT_ENCODING = "gzip, deflate, br, zstd"


@dataclass(frozen=True)
class HttpResponse:
    url: str
    status: int
    reason: str
    headers: Mapping[str, str]
    body: bytes
    body_truncated: bool = False


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
    if max_body_bytes is not None:
        _require(max_body_bytes > 0, f"max_body_bytes must be > 0, got {max_body_bytes}")

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

    def _expected_body_len_from_headers(headers: Mapping[str, str]) -> int | None:
        # For Range requests, Content-Range implies the expected body length.
        content_range = headers.get("content-range")
        if content_range is not None:
            # Example: "bytes 0-0/12345"
            m = re.fullmatch(r"\s*bytes\s+(\d+)-(\d+)/(\d+)\s*", content_range, flags=re.IGNORECASE)
            if m:
                start, end = (int(m.group(1)), int(m.group(2)))
                if end >= start:
                    return end - start + 1

        # Otherwise, fall back to Content-Length (works for 200/4xx etc).
        content_length = headers.get("content-length")
        if content_length is None:
            return None
        try:
            return int(content_length)
        except ValueError:
            return None

    def _read_body(resp, *, method: str, headers: Mapping[str, str]) -> tuple[bytes, bool]:
        if method == "HEAD":
            return b"", False
        if max_body_bytes is None:
            return resp.read(), False

        body = resp.read(max_body_bytes)
        expected_len = _expected_body_len_from_headers(headers)
        if expected_len is not None:
            return body, expected_len > max_body_bytes

        # If we don't know the expected length (no Content-Length/Content-Range),
        # consider the body truncated if we hit the cap exactly.
        return body, len(body) >= max_body_bytes

    try:
        with opener.open(req, timeout=timeout_s) as resp:
            headers_collapsed = _collapse_headers(resp.headers)
            body, body_truncated = _read_body(resp, method=method, headers=headers_collapsed)
            return HttpResponse(
                url=resp.geturl(),
                status=int(resp.status),
                reason=getattr(resp, "reason", ""),
                headers=headers_collapsed,
                body=body,
                body_truncated=body_truncated,
            )
    except urllib.error.HTTPError as e:
        try:
            headers_collapsed = _collapse_headers(e.headers)
            body, body_truncated = _read_body(e, method=method, headers=headers_collapsed)
            return HttpResponse(
                url=e.geturl(),
                status=int(e.code),
                reason=str(e.reason),
                headers=headers_collapsed,
                body=body,
                body_truncated=body_truncated,
            )
        finally:
            # Ensure we promptly close the underlying connection even when we
            # intentionally read only a prefix of the body.
            try:
                e.close()
            except Exception:
                pass
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


@dataclass(frozen=True)
class ChunkedDiskManifest:
    """
    Parsed + validated chunked disk image manifest (aero.chunked-disk-image.v1).

    This mirrors the schema used by `web/src/storage/remote_chunked_disk.ts` and
    `services/image-gateway/openapi.yaml`.
    """

    version: str
    mime_type: str
    total_size: int
    chunk_size: int
    chunk_count: int
    chunk_index_width: int
    chunk_sizes: list[int]
    chunk_sha256: list[str | None]


def _sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _derive_manifest_url_from_base_url(base_url: str) -> str:
    """
    Given a chunked base URL (prefix), derive `<base>/manifest.json`.

    Notes:
    - `base_url` is treated as a directory prefix, not a file.
    - Query string is preserved (useful for some signed URL schemes).
    """

    parts = urllib.parse.urlsplit(base_url)
    base_path = parts.path
    if not base_path.endswith("/"):
        base_path += "/"
    manifest_path = base_path + "manifest.json"
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, manifest_path, parts.query, parts.fragment))


def _derive_chunk_url(*, manifest_url: str, chunk_index: int, chunk_index_width: int) -> str:
    name = str(chunk_index).zfill(chunk_index_width)
    parts = urllib.parse.urlsplit(manifest_url)
    path = parts.path
    prefix = path.rsplit("/", 1)[0] + "/" if "/" in path else ""
    chunk_path = f"{prefix}chunks/{name}.bin"
    # Preserve querystring auth material (e.g. signed URLs), matching the browser client.
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, chunk_path, parts.query, parts.fragment))


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
    max_body_bytes: int,
) -> TestResult:
    name = "private: unauthenticated request is denied (401/403)"
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
            "Range": "bytes=0-0",
        }
        if origin is not None:
            headers["Origin"] = origin
        resp = _request(
            url=base_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
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
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
        etag = _header(resp, "ETag")
        last_modified = _header(resp, "Last-Modified")
        # Require exposing the headers the browser client needs to read for probing.
        # Note: `Last-Modified` is CORS-safelisted and does not need explicit exposure.
        # `ETag` is not safelisted, but is treated as optional; it is checked separately when present.
        _require_cors(resp, origin, expose={"accept-ranges", "content-length"})
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
    max_body_bytes: int,
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
            max_body_bytes=max_body_bytes,
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
    max_body_bytes: int,
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
            max_body_bytes=max_body_bytes,
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
    max_body_bytes: int,
    size: int,
    req_start: int,
    req_end: int,
    strict: bool,
    extra_headers: Mapping[str, str] | None = None,
) -> TestResult:
    _require(0 <= req_start <= req_end < size, f"invalid test range {req_start}-{req_end} for size {size}")
    headers: dict[str, str] = {
        "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
        "Range": f"bytes={req_start}-{req_end}",
    }
    if origin is not None:
        headers["Origin"] = origin
    if authorization is not None:
        headers["Authorization"] = authorization
    if extra_headers is not None:
        for k, v in extra_headers.items():
            headers[str(k)] = str(v)

    expected_len = req_end - req_start + 1
    resp = _request(
        url=base_url,
        method="GET",
        headers=headers,
        timeout_s=timeout_s,
        max_body_bytes=max_body_bytes,
    )
    if resp.status != 206:
        if resp.status == 200:
            truncated = " (body truncated by safety cap)" if resp.body_truncated else ""
            content_length = _header(resp, "Content-Length")
            content_length_msg = f"; Content-Length={content_length}" if content_length is not None else ""
            raise TestFailure(
                "expected 206 Partial Content, got 200 OK (server may be ignoring Range); "
                f"refused to download full response body: read {len(resp.body)} bytes{truncated} "
                f"(cap {_fmt_bytes(max_body_bytes)}){content_length_msg}"
            )
        raise TestFailure(f"expected 206, got {resp.status}")

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
        expose={"accept-ranges", "content-range", "content-length"},
    )
    start, end, total = _parse_content_range(content_range)
    _require(start == req_start and end == req_end, f"expected bytes {req_start}-{req_end}, got {start}-{end}")
    _require(total == size, f"expected total size {size}, got {total}")

    if resp.body_truncated:
        raise TestFailure(
            "response body was truncated by safety cap; "
            f"expected {expected_len} bytes but only read {len(resp.body)} bytes "
            f"(cap {_fmt_bytes(max_body_bytes)}). "
            "Increase --max-body-bytes to debug, or fix server to respect Range."
        )
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
    max_body_bytes: int,
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
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
            "Range": f"bytes={start}-{end}",
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
            max_body_bytes=max_body_bytes,
        )
        if resp.status != 416:
            if resp.status == 200:
                truncated = " (body truncated by safety cap)" if resp.body_truncated else ""
                content_length = _header(resp, "Content-Length")
                content_length_msg = f"; Content-Length={content_length}" if content_length is not None else ""
                raise TestFailure(
                    "expected 416 Range Not Satisfiable, got 200 OK (server may be ignoring Range); "
                    f"refused to download full response body: read {len(resp.body)} bytes{truncated} "
                    f"(cap {_fmt_bytes(max_body_bytes)}){content_length_msg}"
                )
            raise TestFailure(f"expected 416, got {resp.status}")

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
            expose={"accept-ranges", "content-range", "content-length"},
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
    max_body_bytes: int,
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
                "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
                "Origin": origin,
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": req_header_value,
            },
            timeout_s=timeout_s,
            follow_redirects=False,
            max_body_bytes=max_body_bytes,
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

        warnings: list[str] = []

        max_age = _header(resp, "Access-Control-Max-Age")
        if max_age is None:
            warnings.append("missing Access-Control-Max-Age (preflight caching recommended)")
        else:
            try:
                max_age_i = int(max_age.strip())
                if max_age_i <= 0:
                    warnings.append(f"Access-Control-Max-Age should be > 0, got {max_age!r}")
                elif max_age_i < 600:
                    warnings.append(
                        f"Access-Control-Max-Age is low ({max_age_i}s); consider >=600 to reduce preflight overhead"
                    )
            except ValueError:
                warnings.append(f"invalid Access-Control-Max-Age {max_age!r}")

        vary = _header(resp, "Vary")
        if vary is None:
            warnings.append(
                "missing Vary (recommended: Access-Control-Request-Method, Access-Control-Request-Headers, and Origin when varying by Origin)"
            )
        else:
            vary_tokens = _csv_tokens(vary)
            recommended = {"access-control-request-method", "access-control-request-headers"}
            # Only require `Vary: Origin` when the preflight response varies by Origin (i.e. not
            # wildcard allow-origin).
            if allow_origin != "*":
                recommended.add("origin")
            missing_vary = recommended.difference(vary_tokens)
            if missing_vary and "*" not in vary_tokens:
                warnings.append(f"Vary missing {sorted(missing_vary)} (got {vary!r})")

        if warnings:
            return TestResult(name=name, status="WARN", details="; ".join(warnings))

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
    max_body_bytes: int,
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
                "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
                "Origin": origin,
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": req_header_value,
            },
            timeout_s=timeout_s,
            follow_redirects=False,
            max_body_bytes=max_body_bytes,
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

        warnings: list[str] = []

        max_age = _header(resp, "Access-Control-Max-Age")
        if max_age is None:
            warnings.append("missing Access-Control-Max-Age (preflight caching recommended)")
        vary = _header(resp, "Vary")
        if vary is None:
            warnings.append(
                "missing Vary (recommended: Access-Control-Request-Method, Access-Control-Request-Headers, and Origin when varying by Origin)"
            )

        if warnings:
            return TestResult(name=name, status="WARN", details="; ".join(warnings))

        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="WARN", details=str(e))


def _test_options_preflight_authorization(
    *,
    name: str,
    url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    max_body_bytes: int,
) -> TestResult:
    """
    Chunked mode helper: when the caller provides `--token`, requests will include
    `Authorization`, which triggers CORS preflight for cross-origin fetches.
    """
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if authorization is None:
        return TestResult(name=name, status="SKIP", details="skipped (no --token provided)")

    try:
        resp = _request(
            url=url,
            method="OPTIONS",
            headers={
                "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
                "Origin": origin,
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": "authorization",
            },
            timeout_s=timeout_s,
            follow_redirects=False,
            max_body_bytes=max_body_bytes,
        )
        _require(200 <= resp.status < 300, f"expected 2xx, got {resp.status}")

        _require_allow_origin(resp, origin)
        allow_origin = (_header(resp, "Access-Control-Allow-Origin") or "").strip()

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
        _require(
            "*" in allowed or "authorization" in allowed,
            f"expected Allow-Headers to include ['authorization'] (or '*'); got {allow_headers!r}",
        )

        warnings: list[str] = []

        allow_credentials = _header(resp, "Access-Control-Allow-Credentials")
        if allow_credentials is not None:
            ac = allow_credentials.strip().lower()
            if ac != "true":
                warnings.append(
                    f"unexpected Access-Control-Allow-Credentials={allow_credentials!r} (omit or use 'true')"
                )
            elif allow_origin == "*":
                warnings.append(
                    "Access-Control-Allow-Credentials=true with Allow-Origin='*' will not work for credentialed fetches"
                )

        max_age = _header(resp, "Access-Control-Max-Age")
        if max_age is None:
            warnings.append("missing Access-Control-Max-Age (preflight caching recommended)")
        else:
            try:
                max_age_i = int(max_age.strip())
                if max_age_i <= 0:
                    warnings.append(f"Access-Control-Max-Age should be > 0, got {max_age!r}")
                elif max_age_i < 600:
                    warnings.append(
                        f"Access-Control-Max-Age is low ({max_age_i}s); consider >=600 to reduce preflight overhead"
                    )
            except ValueError:
                warnings.append(f"invalid Access-Control-Max-Age {max_age!r}")

        vary = _header(resp, "Vary")
        if vary is None:
            warnings.append(
                "missing Vary (recommended: Access-Control-Request-Method, Access-Control-Request-Headers, and Origin when varying by Origin)"
            )
        else:
            vary_tokens = _csv_tokens(vary)
            recommended = {"access-control-request-method", "access-control-request-headers"}
            if allow_origin != "*":
                recommended.add("origin")
            missing_vary = recommended.difference(vary_tokens)
            if missing_vary and "*" not in vary_tokens:
                warnings.append(f"Vary missing {sorted(missing_vary)} (got {vary!r})")

        if warnings:
            return TestResult(name=name, status="WARN", details="; ".join(warnings))

        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))

def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    env_mode = os.environ.get("MODE")
    env_base_url = os.environ.get("BASE_URL")
    env_manifest_url = os.environ.get("MANIFEST_URL")
    env_token = os.environ.get("TOKEN")
    env_origin = os.environ.get("ORIGIN")
    env_max_body_bytes = os.environ.get("MAX_BODY_BYTES")
    env_max_bytes_per_chunk = os.environ.get("MAX_BYTES_PER_CHUNK")

    parser = argparse.ArgumentParser(
        prog="disk-streaming-conformance",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=textwrap.dedent(
            """\
            Disk image streaming endpoint conformance checks.

            Modes:
              - range   (default): HTTP Range-based streaming endpoint
              - chunked: chunked disk image delivery (manifest + chunks/, no Range)

            Required (range mode): BASE_URL / --base-url
            Required (chunked mode): MANIFEST_URL / --manifest-url OR BASE_URL / --base-url (prefix containing manifest.json)

            Optional: TOKEN / --token, ORIGIN / --origin
            """
        ),
    )
    env_mode_norm = env_mode.strip().lower() if env_mode else ""
    if env_mode_norm not in ("range", "chunked"):
        env_mode_norm = ""
    parser.add_argument(
        "--mode",
        choices=["range", "chunked"],
        default=env_mode_norm or "range",
        help="Conformance mode (env: MODE; default: range)",
    )
    parser.add_argument(
        "--base-url",
        default=env_base_url,
        help=(
            "Range mode: base URL to the disk image bytes endpoint. "
            "Chunked mode: base URL prefix containing manifest.json + chunks/ (env: BASE_URL)"
        ),
    )
    parser.add_argument(
        "--manifest-url",
        default=env_manifest_url,
        help=(
            "Chunked mode: explicit manifest URL (env: MANIFEST_URL). "
            "If omitted, the tool will fetch <base-url>/manifest.json."
        ),
    )
    parser.add_argument("--token", default=env_token, help="Auth token or full Authorization header value (env: TOKEN)")
    parser.add_argument(
        "--origin",
        default=env_origin or "https://example.com",
        help="Origin to simulate for CORS (env: ORIGIN; default: https://example.com)",
    )
    parser.add_argument("--timeout", type=float, default=30.0, help="Request timeout in seconds (default: 30)")

    # In range mode, reads are capped at 1MiB by default because a bug can trigger a full-disk
    # download (20–40GB). In chunked mode we intentionally fetch whole chunks and full manifests,
    # so default high enough to cover Aero's reference client safety bounds.
    default_max_body_bytes_range = 1024 * 1024
    default_max_body_bytes_chunked = 64 * 1024 * 1024
    default_max_body_bytes_env: int | None = None
    if env_max_body_bytes is not None and env_max_body_bytes.strip() != "":
        try:
            default_max_body_bytes_env = int(env_max_body_bytes)
        except ValueError:
            parser.error(f"Invalid MAX_BODY_BYTES {env_max_body_bytes!r} (expected integer)")
    parser.add_argument(
        "--max-body-bytes",
        type=int,
        default=None,
        help=(
            "Maximum response body bytes to read per request "
            f"(env: MAX_BODY_BYTES; default: {default_max_body_bytes_range} for range mode, "
            f"{default_max_body_bytes_chunked} for chunked mode). "
            "The tool caps reads to avoid accidentally downloading full disk images."
        ),
    )
    default_max_bytes_per_chunk = 64 * 1024 * 1024
    if env_max_bytes_per_chunk is not None and env_max_bytes_per_chunk.strip() != "":
        try:
            default_max_bytes_per_chunk = int(env_max_bytes_per_chunk)
        except ValueError:
            parser.error(f"Invalid MAX_BYTES_PER_CHUNK {env_max_bytes_per_chunk!r} (expected integer)")
    parser.add_argument(
        "--max-bytes-per-chunk",
        type=int,
        default=default_max_bytes_per_chunk,
        help=(
            "Chunked mode safety cap: refuse to download chunk objects larger than this. "
            f"(env: MAX_BYTES_PER_CHUNK; default: {default_max_bytes_per_chunk})."
        ),
    )
    parser.add_argument(
        "--sample-chunks",
        type=int,
        default=0,
        help=(
            "Chunked mode: fetch N additional pseudo-random chunks in addition to first+last "
            "(default: 0)."
        ),
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help=(
            "Fail on 'WARN' conditions (e.g. Transfer-Encoding: chunked on 206, "
            "missing Cross-Origin-Resource-Policy, private caching without no-store, "
            "If-Range mismatch behavior, CORS misconfigurations like Allow-Credentials with '*', "
            "or missing preflight caching headers like Access-Control-Max-Age)"
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

    if args.base_url is not None:
        args.base_url = args.base_url.strip()
        if args.base_url == "":
            args.base_url = None
    if args.manifest_url is not None:
        args.manifest_url = args.manifest_url.strip()
        if args.manifest_url == "":
            args.manifest_url = None

    if args.mode == "range":
        if not args.base_url:
            parser.error("Missing --base-url (or env BASE_URL)")
    elif args.mode == "chunked":
        if not args.manifest_url and not args.base_url:
            parser.error("Missing --manifest-url (or env MANIFEST_URL) or --base-url (or env BASE_URL)")

    if args.max_body_bytes is None:
        if default_max_body_bytes_env is not None:
            args.max_body_bytes = default_max_body_bytes_env
        else:
            args.max_body_bytes = default_max_body_bytes_chunked if args.mode == "chunked" else default_max_body_bytes_range

    if args.max_body_bytes <= 0:
        parser.error("--max-body-bytes must be > 0")
    if args.max_bytes_per_chunk <= 0:
        parser.error("--max-bytes-per-chunk must be > 0")
    if args.sample_chunks < 0:
        parser.error("--sample-chunks must be >= 0")

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


def _strip_weak_etag_prefix(etag: str) -> str:
    etag = etag.strip()
    if etag.lower().startswith("w/"):
        return etag[2:].strip()
    return etag


def _has_comma_outside_quotes(value: str) -> bool:
    in_quotes = False
    escaped = False
    for ch in value:
        if escaped:
            escaped = False
            continue
        # Quoted-string can escape characters. Track escapes only while inside quotes so
        # backslashes in unquoted portions don't affect parsing.
        if in_quotes and ch == "\\":
            escaped = True
            continue
        if ch == '"':
            in_quotes = not in_quotes
            continue
        if ch == "," and not in_quotes:
            return True
    # Be conservative: if quotes are unbalanced, treat it as potentially a list.
    return in_quotes


def _is_single_etag(etag: str) -> bool:
    # If-Range only accepts a single validator. We conservatively skip if it looks like a list.
    #
    # ETag values can contain commas inside quotes, so only treat commas that occur outside quotes
    # as list separators.
    return not _has_comma_outside_quotes(etag)


def _test_if_range_matches_etag(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    max_body_bytes: int,
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
            max_body_bytes=max_body_bytes,
            size=size,
            req_start=0,
            req_end=0,
            strict=strict,
            extra_headers={"If-Range": etag},
        )
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_if_range_matches_last_modified(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    max_body_bytes: int,
    size: int | None,
    last_modified: str | None,
    strict: bool,
) -> TestResult:
    name = "GET: Range + If-Range (Last-Modified date) returns 206"
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")
    if last_modified is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Last-Modified from HEAD)")

    # The HTTP-date form of If-Range is optional but useful when a server advertises Last-Modified
    # without a strong ETag. Treat non-206 behavior as WARN (unless strict mode is enabled).
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
            "Range": "bytes=0-0",
            "If-Range": last_modified,
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
            max_body_bytes=max_body_bytes,
        )
        _require_cors(resp, origin)

        if resp.status != 206:
            if resp.status in (200, 412):
                truncated = " (body truncated by safety cap)" if resp.body_truncated else ""
                content_length = _header(resp, "Content-Length")
                content_length_msg = (
                    f"; Content-Length={content_length}" if content_length is not None else ""
                )
                message = (
                    "server did not accept If-Range in HTTP-date form; "
                    f"expected 206 but got {resp.status} (Range likely ignored)"
                )
                if strict:
                    return TestResult(
                        name=name,
                        status="FAIL",
                        details=f"{message}; read {len(resp.body)} bytes{truncated} (cap {_fmt_bytes(max_body_bytes)}){content_length_msg}",
                    )
                return TestResult(
                    name=name,
                    status="WARN",
                    details=f"{message}; read {len(resp.body)} bytes{truncated} (cap {_fmt_bytes(max_body_bytes)}){content_length_msg}",
                )
            raise TestFailure(f"expected 206, got {resp.status}")

        # Validate the ranged response similarly to `_test_get_range`, but keep the error surface
        # minimal since this is an optional check.
        content_range = _header(resp, "Content-Range")
        _require(content_range is not None, "missing Content-Range header")
        start, end, total = _parse_content_range(content_range)
        _require(start == 0 and end == 0, f"expected bytes 0-0, got {start}-{end}")
        _require(total == size, f"expected total size {size}, got {total}")

        if resp.body_truncated:
            raise TestFailure(
                "response body was truncated by safety cap; "
                f"expected 1 byte but only read {len(resp.body)} bytes "
                f"(cap {_fmt_bytes(max_body_bytes)}). "
                "Fix server to respect Range/If-Range, or increase --max-body-bytes to debug."
            )
        _require(len(resp.body) == 1, f"expected body length 1, got {len(resp.body)}")

        return TestResult(name=name, status="PASS", details=f"Content-Range={content_range!r}")
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


def _test_get_etag_matches_head(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    max_body_bytes: int,
    head_etag: str | None,
) -> tuple[TestResult, HttpResponse | None]:
    name = "GET: ETag matches HEAD ETag"
    if head_etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag from HEAD)"), None

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
            max_body_bytes=min(max_body_bytes, 1024),
        )
        _require_cors(resp, origin)
        _require(resp.status == 206, f"expected 206, got {resp.status}")

        get_etag = _header(resp, "ETag")
        _require(get_etag is not None, "missing ETag on 206 response")

        if _strip_weak_etag_prefix(get_etag) != _strip_weak_etag_prefix(head_etag):
            raise TestFailure(f"ETag mismatch: HEAD={head_etag!r} GET={get_etag!r}")

        return TestResult(name=name, status="PASS"), resp
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e)), None

def _test_if_range_mismatch(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    max_body_bytes: int,
    size: int | None,
    strict: bool,
) -> TestResult:
    name = 'GET: Range + If-Range ("mismatch") does not return mixed-version 206'
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
            max_body_bytes=max_body_bytes,
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
    max_body_bytes: int,
) -> TestResult:
    name = "GET: If-None-Match returns 304 Not Modified"
    if etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag from HEAD)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
            max_body_bytes=max_body_bytes,
        )
        _require_cors(resp, origin)
        _require(resp.status == 304, f"expected 304, got {resp.status}")
        _require(len(resp.body) == 0, f"expected empty body on 304, got {len(resp.body)} bytes")

        resp_etag = _header(resp, "ETag")
        if resp_etag is None:
            return TestResult(name=name, status="WARN", details="missing ETag on 304 response")
        if _strip_weak_etag_prefix(resp_etag) != _strip_weak_etag_prefix(etag):
            return TestResult(
                name=name,
                status="FAIL",
                details=f"ETag mismatch on 304: expected {etag!r}, got {resp_etag!r}",
            )
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
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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

        resp_etag = _header(resp, "ETag")
        if resp_etag is None:
            return TestResult(name=name, status="WARN", details="missing ETag on 304 response")
        if _strip_weak_etag_prefix(resp_etag) != _strip_weak_etag_prefix(etag):
            return TestResult(
                name=name,
                status="FAIL",
                details=f"ETag mismatch on 304: expected {etag!r}, got {resp_etag!r}",
            )
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
    max_body_bytes: int,
    strict: bool,
) -> TestResult:
    name = "GET: If-Modified-Since returns 304 Not Modified"
    if last_modified is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Last-Modified from HEAD)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
            max_body_bytes=max_body_bytes,
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
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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

    content_encoding = _header(resp, "Content-Encoding")
    if content_encoding is not None:
        encodings = _csv_tokens(content_encoding)
        if encodings != {"identity"}:
            issues.append(
                f"unexpected Content-Encoding {content_encoding!r} (expected 'identity' or absent)"
            )

    if issues:
        return TestResult(name=name, status="WARN", details="; ".join(issues))
    return TestResult(name=name, status="PASS")


def _test_get_content_headers(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
) -> tuple[TestResult, HttpResponse | None]:
    name = "GET: Content-Type is application/octet-stream and X-Content-Type-Options=nosniff"
    resp: HttpResponse | None = None
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
        return _test_content_headers(name=name, resp=resp), resp
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e)), resp


def _test_cors_vary_origin(
    *,
    resp: HttpResponse | None,
    origin: str | None,
    name: str = "CORS: Vary includes Origin when Allow-Origin echoes a specific origin",
) -> TestResult:
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
    name: str = "CORS: Allow-Credentials does not contradict Allow-Origin",
) -> TestResult:
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
    max_body_bytes: int,
) -> TestResult:
    name = "GET: Cross-Origin-Resource-Policy is set"
    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
            max_body_bytes=max_body_bytes,
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
    max_body_bytes: int,
    strict: bool,
) -> TestResult:
    name = "private: 206 responses are not publicly cacheable (Cache-Control)"
    if authorization is None:
        return TestResult(name=name, status="SKIP", details="skipped (no --token provided)")

    try:
        headers: dict[str, str] = {
            "Accept-Encoding": _BROWSER_ACCEPT_ENCODING,
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
            max_body_bytes=max_body_bytes,
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


# ---------------------------------------------------------------------------
# Chunked disk image conformance (manifest.json + chunks/, no Range)
# ---------------------------------------------------------------------------


def _json_int(value: object, *, name: str) -> int:
    # JSON numbers should parse as Python ints; reject floats/bools for conformance.
    if isinstance(value, bool) or not isinstance(value, int):
        raise TestFailure(f"{name} must be an integer, got {type(value).__name__}")
    return int(value)


def _parse_chunked_manifest_v1(raw: object) -> ChunkedDiskManifest:
    if not isinstance(raw, dict):
        raise TestFailure("manifest.json must be a JSON object")

    schema = raw.get("schema")
    _require(schema == "aero.chunked-disk-image.v1", f"unsupported manifest schema {schema!r}")

    version = raw.get("version")
    _require(isinstance(version, str) and version.strip(), "manifest.version must be a non-empty string")

    mime_type = raw.get("mimeType")
    _require(isinstance(mime_type, str) and mime_type.strip(), "manifest.mimeType must be a non-empty string")

    total_size = _json_int(raw.get("totalSize"), name="totalSize")
    chunk_size = _json_int(raw.get("chunkSize"), name="chunkSize")
    chunk_count = _json_int(raw.get("chunkCount"), name="chunkCount")
    chunk_index_width = _json_int(raw.get("chunkIndexWidth"), name="chunkIndexWidth")

    _require(total_size > 0, f"totalSize must be > 0, got {total_size}")
    _require(total_size % 512 == 0, f"totalSize must be a multiple of 512, got {total_size}")
    _require(chunk_size > 0, f"chunkSize must be > 0, got {chunk_size}")
    _require(chunk_size % 512 == 0, f"chunkSize must be a multiple of 512, got {chunk_size}")
    _require(chunk_count > 0, f"chunkCount must be > 0, got {chunk_count}")
    _require(chunk_index_width > 0, f"chunkIndexWidth must be > 0, got {chunk_index_width}")

    # Defensive bounds to avoid pathological allocations when validating untrusted manifests.
    # Mirror the browser client bounds (`web/src/storage/remote_chunked_disk.ts`) so conformance
    # matches what Aero will actually accept.
    max_chunk_size = 64 * 1024 * 1024  # 64 MiB
    max_chunk_count = 500_000
    max_chunk_index_width = 32
    _require(
        chunk_size <= max_chunk_size,
        f"chunkSize too large: max={max_chunk_size} got={chunk_size}",
    )
    _require(
        chunk_count <= max_chunk_count,
        f"chunkCount too large: max={max_chunk_count} got={chunk_count}",
    )
    _require(
        chunk_index_width <= max_chunk_index_width,
        f"chunkIndexWidth too large: max={max_chunk_index_width} got={chunk_index_width}",
    )

    expected_chunk_count = (total_size + chunk_size - 1) // chunk_size
    _require(
        chunk_count == expected_chunk_count,
        f"chunkCount mismatch: expected={expected_chunk_count} manifest={chunk_count}",
    )

    min_width = len(str(chunk_count - 1))
    _require(
        chunk_index_width >= min_width,
        f"chunkIndexWidth too small: need>={min_width} got={chunk_index_width}",
    )

    last_chunk_size = total_size - chunk_size * (chunk_count - 1)
    _require(
        0 < last_chunk_size <= chunk_size,
        f"invalid final chunk size: derived={last_chunk_size} chunkSize={chunk_size}",
    )

    chunks = raw.get("chunks")
    chunk_sizes: list[int] = []
    chunk_sha256: list[str | None] = []

    if chunks is None:
        chunk_sizes = [chunk_size] * chunk_count
        chunk_sizes[-1] = last_chunk_size
        chunk_sha256 = [None] * chunk_count
    else:
        _require(isinstance(chunks, list), "chunks must be an array when present")
        _require(len(chunks) == chunk_count, f"chunks.length mismatch: expected={chunk_count} actual={len(chunks)}")
        for i, item in enumerate(chunks):
            _require(isinstance(item, dict), f"chunks[{i}] must be a JSON object")
            expected_size = chunk_size if i < chunk_count - 1 else last_chunk_size
            if "size" not in item or item.get("size") is None:
                # `size` is optional; when omitted, clients derive it from `chunkSize` and `totalSize`.
                size = expected_size
            else:
                size = _json_int(item.get("size"), name=f"chunks[{i}].size")
                _require(size > 0, f"chunks[{i}].size must be > 0, got {size}")
                _require(
                    size == expected_size,
                    f"chunks[{i}].size mismatch: expected={expected_size} actual={size}",
                )
            chunk_sizes.append(size)

            sha = item.get("sha256")
            if sha is None:
                chunk_sha256.append(None)
            else:
                _require(isinstance(sha, str), f"chunks[{i}].sha256 must be a string")
                normalized = sha.strip().lower()
                _require(
                    re.fullmatch(r"[0-9a-f]{64}", normalized) is not None,
                    f"chunks[{i}].sha256 must be a 64-char hex string, got {sha!r}",
                )
                chunk_sha256.append(normalized)

    _require(sum(chunk_sizes) == total_size, f"chunk sizes do not sum to totalSize: sum={sum(chunk_sizes)} totalSize={total_size}")

    return ChunkedDiskManifest(
        version=version.strip(),
        mime_type=mime_type.strip(),
        total_size=total_size,
        chunk_size=chunk_size,
        chunk_count=chunk_count,
        chunk_index_width=chunk_index_width,
        chunk_sizes=chunk_sizes,
        chunk_sha256=chunk_sha256,
    )


def _test_chunked_manifest_fetch(
    *,
    manifest_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
    max_body_bytes: int,
) -> tuple[TestResult, HttpResponse | None, object | None]:
    name = "manifest: GET returns 200 and parses JSON"
    resp: HttpResponse | None = None
    try:
        # Read at most max_body_bytes+1 so we can distinguish "exactly hit cap" from
        # "response is larger than cap" even when Content-Length is missing.
        request_cap = max_body_bytes + 1
        headers: dict[str, str] = {"Accept-Encoding": _BROWSER_ACCEPT_ENCODING}
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(
            url=manifest_url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=request_cap,
        )
        _require(200 <= resp.status < 300, f"expected 2xx, got {resp.status}")
        if len(resp.body) > max_body_bytes:
            truncated = " (body truncated by safety cap)" if resp.body_truncated else ""
            raise TestFailure(
                "manifest response exceeds safety cap; "
                f"read {len(resp.body)} bytes{truncated} (cap {_fmt_bytes(max_body_bytes)}). "
                "Increase --max-body-bytes to debug."
            )

        content_encoding = _header(resp, "Content-Encoding")
        if content_encoding is not None:
            encodings = _csv_tokens(content_encoding)
            _require(
                encodings == {"identity"},
                f"unexpected Content-Encoding {content_encoding!r} (expected absent or 'identity')",
            )

        try:
            text = resp.body.decode("utf-8")
        except UnicodeDecodeError:
            raise TestFailure("manifest body is not valid UTF-8") from None

        try:
            raw = json.loads(text)
        except json.JSONDecodeError as e:
            raise TestFailure(f"manifest body is not valid JSON: {e}") from None

        return TestResult(name=name, status="PASS", details=f"bytes={len(resp.body)}"), resp, raw
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e)), resp, None


def _test_chunked_private_requires_auth(
    *,
    name: str,
    url: str,
    origin: str | None,
    timeout_s: float,
    max_body_bytes: int,
) -> TestResult:
    try:
        headers: dict[str, str] = {"Accept-Encoding": _BROWSER_ACCEPT_ENCODING}
        if origin is not None:
            headers["Origin"] = origin
        resp = _request(
            url=url,
            method="GET",
            headers=headers,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
        )
        _require_cors(resp, origin)
        _require(resp.status in (401, 403), f"expected 401/403, got {resp.status}")
        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_chunked_manifest_schema(*, raw: object | None) -> tuple[TestResult, ChunkedDiskManifest | None]:
    name = "manifest: schema aero.chunked-disk-image.v1 and size/count consistency"
    if raw is None:
        return TestResult(name=name, status="SKIP", details="skipped (no manifest JSON)"), None
    try:
        manifest = _parse_chunked_manifest_v1(raw)
        return (
            TestResult(
                name=name,
                status="PASS",
                details=(
                    f"totalSize={manifest.total_size} ({_fmt_bytes(manifest.total_size)}), "
                    f"chunkSize={manifest.chunk_size} ({_fmt_bytes(manifest.chunk_size)}), "
                    f"chunkCount={manifest.chunk_count}"
                ),
            ),
            manifest,
        )
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e)), None


def _test_chunked_content_type(
    *,
    name: str,
    resp: HttpResponse | None,
    expected: str,
) -> TestResult:
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    try:
        ct = _header(resp, "Content-Type")
        _require(ct is not None, "missing Content-Type")
        media = _media_type(ct)
        _require(media == expected, f"expected Content-Type {expected!r}, got {ct!r}")
        return TestResult(name=name, status="PASS", details=f"Content-Type={ct!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_chunked_chunk_encoding(*, name: str, resp: HttpResponse | None) -> TestResult:
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    try:
        content_encoding = _header(resp, "Content-Encoding")
        if content_encoding is None:
            return TestResult(name=name, status="PASS", details="(absent)")
        encodings = _csv_tokens(content_encoding)
        _require(encodings == {"identity"}, f"expected Content-Encoding absent or 'identity', got {content_encoding!r}")
        return TestResult(name=name, status="PASS", details=f"value={content_encoding!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_cache_control_immutable(
    *,
    name: str,
    resp: HttpResponse | None,
    strict: bool,
    require_no_transform: bool,
    authorization: str | None,
) -> TestResult:
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    cache_control = _header(resp, "Cache-Control")
    if cache_control is None:
        if authorization is None:
            msg = "missing Cache-Control (recommended: include immutable)"
        else:
            msg = "missing Cache-Control (private responses should avoid public caching; no-store recommended)"
        return TestResult(name=name, status="FAIL" if strict else "WARN", details=msg)
    tokens = _csv_tokens(cache_control)
    issues: list[str] = []
    if authorization is None:
        if "immutable" not in tokens:
            issues.append(f"Cache-Control missing immutable: {cache_control!r}")
    else:
        # Authorization-triggered fetches are not safe to cache publicly unless you very carefully
        # vary caches by Authorization. Treat Cache-Control: public as a conformance failure.
        if "public" in tokens:
            return TestResult(name=name, status="FAIL", details=f"private response must not be Cache-Control: public; got {cache_control!r}")
        if "no-store" not in tokens:
            issues.append(f"private response Cache-Control missing no-store: {cache_control!r}")
    if require_no_transform and "no-transform" not in tokens:
        issues.append(f"Cache-Control missing no-transform: {cache_control!r}")
    if issues:
        return TestResult(name=name, status="FAIL" if strict else "WARN", details="; ".join(issues))
    return TestResult(name=name, status="PASS", details=f"Cache-Control={cache_control!r}")


def _test_cors_allow_origin(*, name: str, resp: HttpResponse | None, origin: str | None) -> TestResult:
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    try:
        _require_allow_origin(resp, origin)
        allow_origin = _header(resp, "Access-Control-Allow-Origin") or ""
        return TestResult(name=name, status="PASS", details=f"Allow-Origin={allow_origin!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_cors_expose_etag_if_present(*, name: str, resp: HttpResponse | None, origin: str | None) -> TestResult:
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    etag = _header(resp, "ETag")
    if etag is None:
        return TestResult(name=name, status="SKIP", details="skipped (no ETag header)")
    expose = _header(resp, "Access-Control-Expose-Headers")
    if expose is None:
        return TestResult(
            name=name,
            status="WARN",
            details="ETag is present but Access-Control-Expose-Headers is missing (browsers won't expose ETag to JS)",
        )
    tokens = _csv_tokens(expose)
    if "*" in tokens or "etag" in tokens:
        return TestResult(name=name, status="PASS", details=f"Expose-Headers={expose!r}")
    return TestResult(name=name, status="WARN", details=f"ETag not exposed via Access-Control-Expose-Headers: {expose!r}")


def _test_cors_expose_last_modified_if_present(
    *, name: str, resp: HttpResponse | None, origin: str | None
) -> TestResult:
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    last_modified = _header(resp, "Last-Modified")
    if last_modified is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Last-Modified header)")
    # `Last-Modified` is a CORS-safelisted response header and is exposed to JS by default.
    # Keep this check as informational (PASS) to avoid noisy strict-mode failures on servers that
    # reasonably only expose non-safelisted headers like ETag.
    return TestResult(name=name, status="PASS", details=f"value={last_modified!r} (CORS-safelisted)")


def _test_cors_expose_content_encoding_if_present(
    *, name: str, resp: HttpResponse | None, origin: str | None
) -> TestResult:
    if origin is None:
        return TestResult(name=name, status="SKIP", details="skipped (no origin provided)")
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    content_encoding = _header(resp, "Content-Encoding")
    if content_encoding is None:
        return TestResult(name=name, status="SKIP", details="skipped (no Content-Encoding header)")
    expose = _header(resp, "Access-Control-Expose-Headers")
    if expose is None:
        return TestResult(
            name=name,
            status="WARN",
            details=(
                "Content-Encoding is present but Access-Control-Expose-Headers is missing "
                "(browsers won't expose Content-Encoding to JS)"
            ),
        )
    tokens = _csv_tokens(expose)
    if "*" in tokens or "content-encoding" in tokens:
        return TestResult(name=name, status="PASS", details=f"Expose-Headers={expose!r}")
    return TestResult(
        name=name,
        status="WARN",
        details=f"Content-Encoding not exposed via Access-Control-Expose-Headers: {expose!r}",
    )


def _test_x_content_type_options_nosniff(*, name: str, resp: HttpResponse | None) -> TestResult:
    if resp is None:
        return TestResult(name=name, status="SKIP", details="skipped (no response)")
    xcto = _header(resp, "X-Content-Type-Options")
    if xcto is None:
        return TestResult(name=name, status="WARN", details="missing X-Content-Type-Options (recommended: nosniff)")
    if xcto.strip().lower() != "nosniff":
        return TestResult(name=name, status="WARN", details=f"unexpected X-Content-Type-Options {xcto!r} (expected 'nosniff')")
    return TestResult(name=name, status="PASS")


def _test_manifest_mime_type(*, name: str, manifest: ChunkedDiskManifest | None) -> TestResult:
    if manifest is None:
        return TestResult(name=name, status="SKIP", details="skipped (no manifest)")
    # The format spec currently defines chunk objects as binary disk bytes.
    # Keep this as WARN-only for forward compatibility (e.g. if we ever add per-chunk compression formats).
    if _media_type(manifest.mime_type) != "application/octet-stream":
        return TestResult(
            name=name,
            status="WARN",
            details=f"unexpected manifest.mimeType {manifest.mime_type!r} (expected application/octet-stream)",
        )
    return TestResult(name=name, status="PASS")


def _main_chunked(args: argparse.Namespace) -> int:
    origin: str | None = args.origin
    timeout_s: float = args.timeout
    strict: bool = bool(args.strict)
    expect_corp: str | None = args.expect_corp
    max_body_bytes: int = int(args.max_body_bytes)
    max_bytes_per_chunk: int = int(args.max_bytes_per_chunk)
    sample_chunks: int = int(args.sample_chunks)

    token: str | None = args.token
    authorization: str | None = _authorization_value(token) if token else None

    manifest_url: str | None = args.manifest_url
    base_url: str | None = args.base_url
    if manifest_url is None:
        _require(base_url is not None, "internal error: missing base_url/manifest_url for chunked mode")
        manifest_url = _derive_manifest_url_from_base_url(base_url)

    print("Disk streaming conformance (chunked)")
    print(f"  MANIFEST_URL: {manifest_url}")
    if base_url is not None:
        print(f"  BASE_URL:     {base_url}")
    print(f"  ORIGIN:       {origin or '(none)'}")
    print(f"  ACCEPT_ENCODING: {_BROWSER_ACCEPT_ENCODING}")
    print(f"  STRICT:       {strict}")
    print(f"  CORP:         {expect_corp or '(not required)'}")
    print(f"  MAX_BODY_BYTES:       {max_body_bytes} ({_fmt_bytes(max_body_bytes)})")
    print(f"  MAX_BYTES_PER_CHUNK:  {max_bytes_per_chunk} ({_fmt_bytes(max_bytes_per_chunk)})")
    print(f"  SAMPLE_CHUNKS: {sample_chunks}")
    if authorization is None:
        print("  AUTH:         (none)")
    else:
        print("  AUTH:         provided (token hidden)")
    print()

    results: list[TestResult] = []

    if authorization is not None:
        results.append(
            _test_chunked_private_requires_auth(
                name="private: unauthenticated manifest request is denied (401/403)",
                url=manifest_url,
                origin=origin,
                timeout_s=timeout_s,
                max_body_bytes=min(max_body_bytes, 1024),
            )
        )
        results.append(
            _test_options_preflight_authorization(
                name="OPTIONS: (chunked) CORS preflight allows Authorization header (manifest)",
                url=manifest_url,
                origin=origin,
                authorization=authorization,
                timeout_s=timeout_s,
                max_body_bytes=min(max_body_bytes, 1024),
            )
        )

    manifest_get, manifest_resp, manifest_json = _test_chunked_manifest_fetch(
        manifest_url=manifest_url,
        origin=origin,
        authorization=authorization,
        timeout_s=timeout_s,
        max_body_bytes=max_body_bytes,
    )
    results.append(manifest_get)

    manifest_schema, manifest = _test_chunked_manifest_schema(raw=manifest_json)
    results.append(manifest_schema)
    results.append(_test_manifest_mime_type(name="manifest: mimeType is application/octet-stream", manifest=manifest))

    if authorization is not None and manifest is not None and manifest.chunk_count > 0:
        first_chunk_url = _derive_chunk_url(
            manifest_url=manifest_url,
            chunk_index=0,
            chunk_index_width=manifest.chunk_index_width,
        )
        results.append(
            _test_options_preflight_authorization(
                name="OPTIONS: (chunked) CORS preflight allows Authorization header (chunk)",
                url=first_chunk_url,
                origin=origin,
                authorization=authorization,
                timeout_s=timeout_s,
                max_body_bytes=min(max_body_bytes, 1024),
            )
        )

    # Manifest headers.
    results.append(
        _test_chunked_content_type(
            name="manifest: Content-Type is application/json",
            resp=manifest_resp,
            expected="application/json",
        )
    )
    results.append(
        _test_chunked_chunk_encoding(
            name="manifest: Content-Encoding is identity/absent",
            resp=manifest_resp,
        )
    )
    results.append(
        _test_cors_expose_content_encoding_if_present(
            name="manifest: CORS exposes Content-Encoding when present",
            resp=manifest_resp,
            origin=origin,
        )
    )
    results.append(
        _test_x_content_type_options_nosniff(
            name="manifest: X-Content-Type-Options is nosniff",
            resp=manifest_resp,
        )
    )
    cache_control_name = (
        "manifest: Cache-Control includes immutable"
        if authorization is None
        else "manifest: Cache-Control is safe for private content (no-store recommended)"
    )
    results.append(
        _test_cache_control_immutable(
            name=cache_control_name,
            resp=manifest_resp,
            strict=strict,
            require_no_transform=True,
            authorization=authorization,
        )
    )
    results.append(
        _test_cors_allow_origin(
            name="manifest: CORS allows origin",
            resp=manifest_resp,
            origin=origin,
        )
    )
    results.append(
        _test_cors_expose_etag_if_present(
            name="manifest: CORS exposes ETag when present",
            resp=manifest_resp,
            origin=origin,
        )
    )
    results.append(
        _test_cors_expose_last_modified_if_present(
            name="manifest: CORS exposes Last-Modified when present",
            resp=manifest_resp,
            origin=origin,
        )
    )
    results.append(
        _test_cors_allow_credentials_sane(
            resp=manifest_resp,
            origin=origin,
            name="manifest: CORS Allow-Credentials does not contradict Allow-Origin",
        )
    )
    results.append(
        _test_cors_vary_origin(
            resp=manifest_resp,
            origin=origin,
            name="manifest: CORS Vary includes Origin when Allow-Origin echoes a specific origin",
        )
    )
    results.append(
        _test_corp_header(
            name="manifest: Cross-Origin-Resource-Policy is set",
            resp=manifest_resp,
            expect_corp=expect_corp,
        )
    )

    # Chunk checks.
    if manifest is None:
        results.append(TestResult(name="chunks: fetch sample chunks", status="SKIP", details="skipped (no valid manifest)"))
    else:
        if authorization is not None and manifest.chunk_count > 0:
            # Best-effort: ensure chunk objects are protected when the caller indicates the image is private.
            # Use a tiny read cap so we don't accidentally download a full chunk if the server is misconfigured.
            first_chunk_url = _derive_chunk_url(
                manifest_url=manifest_url,
                chunk_index=0,
                chunk_index_width=manifest.chunk_index_width,
            )
            results.append(
                _test_chunked_private_requires_auth(
                    name="private: unauthenticated chunk request is denied (401/403)",
                    url=first_chunk_url,
                    origin=origin,
                    timeout_s=timeout_s,
                    max_body_bytes=1024,
                )
            )

        max_declared = max(manifest.chunk_sizes) if manifest.chunk_sizes else 0
        caps_ok = max_declared <= max_bytes_per_chunk and max_declared <= max_body_bytes
        if not caps_ok:
            results.append(
                TestResult(
                    name="chunks: size within safety caps",
                    status="FAIL",
                    details=(
                        f"chunk size {max_declared} ({_fmt_bytes(max_declared)}) exceeds "
                        f"--max-bytes-per-chunk ({_fmt_bytes(max_bytes_per_chunk)}) "
                        f"or --max-body-bytes ({_fmt_bytes(max_body_bytes)}). "
                        "Increase caps to run conformance."
                    ),
                )
            )
            results.append(
                TestResult(
                    name="chunks: fetch sample chunks",
                    status="SKIP",
                    details="skipped (safety caps too low)",
                )
            )
        else:
            results.append(
                TestResult(
                    name="chunks: size within safety caps",
                    status="PASS",
                    details=f"maxChunkSize={max_declared} ({_fmt_bytes(max_declared)})",
                )
            )

            indices: set[int] = set()
            if manifest.chunk_count > 0:
                indices.add(0)
                indices.add(manifest.chunk_count - 1)

            extra_count = min(sample_chunks, max(0, manifest.chunk_count - len(indices)))
            if extra_count > 0 and manifest.chunk_count > 2:
                seed_material = f"{manifest.version}\n{manifest_url}".encode("utf-8")
                seed = int.from_bytes(hashlib.sha256(seed_material).digest()[:8], "big")
                rng = random.Random(seed)
                indices.update(rng.sample(range(1, manifest.chunk_count - 1), extra_count))

            for chunk_index in sorted(indices):
                label = str(chunk_index).zfill(manifest.chunk_index_width)
                chunk_url = _derive_chunk_url(
                    manifest_url=manifest_url,
                    chunk_index=chunk_index,
                    chunk_index_width=manifest.chunk_index_width,
                )
                expected_len = manifest.chunk_sizes[chunk_index]

                # Fetch chunk.
                chunk_resp: HttpResponse | None = None
                chunk_body_ok = False
                try:
                    headers: dict[str, str] = {"Accept-Encoding": _BROWSER_ACCEPT_ENCODING}
                    if origin is not None:
                        headers["Origin"] = origin
                    if authorization is not None:
                        headers["Authorization"] = authorization

                    # Read up to expected_len+1 so we can detect accidental extra bytes when
                    # Content-Length is missing.
                    cap = min(max_body_bytes + 1, max_bytes_per_chunk + 1, expected_len + 1)
                    resp = _request(
                        url=chunk_url,
                        method="GET",
                        headers=headers,
                        timeout_s=timeout_s,
                        max_body_bytes=cap,
                    )
                    chunk_resp = resp

                    _require(resp.status == 200, f"expected 200, got {resp.status}")
                    if len(resp.body) > expected_len:
                        raise TestFailure(
                            f"server returned more bytes than expected: expected={expected_len} got={len(resp.body)}"
                        )
                    if len(resp.body) < expected_len:
                        truncated = " (body truncated by safety cap)" if resp.body_truncated else ""
                        raise TestFailure(
                            f"expected body length {expected_len}, got {len(resp.body)}{truncated} "
                            f"(cap {_fmt_bytes(cap)})"
                        )

                    details = f"bytes={expected_len} ({_fmt_bytes(expected_len)})"
                    if resp.body_truncated:
                        # If the server doesn't include Content-Length, the underlying request helper
                        # conservatively marks the response as truncated when we hit the read cap
                        # exactly. If we already got the exact expected chunk length, treat it as a
                        # pass (but surface it in the details).
                        details += " (hit read cap)"
                    results.append(
                        TestResult(
                            name=f"chunk {label}: GET returns 200 with expected body length",
                            status="PASS",
                            details=details,
                        )
                    )
                    chunk_body_ok = True
                except TestFailure as e:
                    results.append(
                        TestResult(
                            name=f"chunk {label}: GET returns 200 with expected body length",
                            status="FAIL",
                            details=str(e),
                        )
                    )

                # Header checks (run even if the body check failed, when we have a response).
                results.append(
                    _test_chunked_content_type(
                        name=f"chunk {label}: Content-Type is application/octet-stream",
                        resp=chunk_resp,
                        expected="application/octet-stream",
                    )
                )
                results.append(
                    _test_x_content_type_options_nosniff(
                        name=f"chunk {label}: X-Content-Type-Options is nosniff",
                        resp=chunk_resp,
                    )
                )
                results.append(
                    _test_chunked_chunk_encoding(
                        name=f"chunk {label}: Content-Encoding is absent or identity",
                        resp=chunk_resp,
                    )
                )
                results.append(
                    _test_cors_expose_content_encoding_if_present(
                        name=f"chunk {label}: CORS exposes Content-Encoding when present",
                        resp=chunk_resp,
                        origin=origin,
                    )
                )
                results.append(
                    _test_cache_control_immutable(
                        name=(
                            f"chunk {label}: Cache-Control includes immutable"
                            if authorization is None
                            else f"chunk {label}: Cache-Control is safe for private content (no-store recommended)"
                        ),
                        resp=chunk_resp,
                        strict=strict,
                        require_no_transform=True,
                        authorization=authorization,
                    )
                )
                results.append(
                    _test_cors_allow_origin(
                        name=f"chunk {label}: CORS allows origin",
                        resp=chunk_resp,
                        origin=origin,
                    )
                )
                results.append(
                    _test_cors_expose_etag_if_present(
                        name=f"chunk {label}: CORS exposes ETag when present",
                        resp=chunk_resp,
                        origin=origin,
                    )
                )
                results.append(
                    _test_cors_expose_last_modified_if_present(
                        name=f"chunk {label}: CORS exposes Last-Modified when present",
                        resp=chunk_resp,
                        origin=origin,
                    )
                )
                results.append(
                    _test_cors_allow_credentials_sane(
                        resp=chunk_resp,
                        origin=origin,
                        name=f"chunk {label}: CORS Allow-Credentials does not contradict Allow-Origin",
                    )
                )
                results.append(
                    _test_cors_vary_origin(
                        resp=chunk_resp,
                        origin=origin,
                        name=f"chunk {label}: CORS Vary includes Origin when Allow-Origin echoes a specific origin",
                    )
                )
                results.append(
                    _test_corp_header(
                        name=f"chunk {label}: Cross-Origin-Resource-Policy is set",
                        resp=chunk_resp,
                        expect_corp=expect_corp,
                    )
                )

                # Optional integrity.
                expected_sha = manifest.chunk_sha256[chunk_index]
                if expected_sha is None:
                    results.append(
                        TestResult(
                            name=f"chunk {label}: sha256 matches manifest",
                            status="SKIP",
                            details="skipped (no sha256 in manifest)",
                        )
                    )
                elif not chunk_body_ok or chunk_resp is None or chunk_resp.body_truncated:
                    results.append(
                        TestResult(
                            name=f"chunk {label}: sha256 matches manifest",
                            status="SKIP",
                            details="skipped (no full chunk body)",
                        )
                    )
                else:
                    actual_sha = _sha256_hex(chunk_resp.body)
                    if actual_sha != expected_sha:
                        results.append(
                            TestResult(
                                name=f"chunk {label}: sha256 matches manifest",
                                status="FAIL",
                                details=f"expected={expected_sha} actual={actual_sha}",
                            )
                        )
                    else:
                        results.append(TestResult(name=f"chunk {label}: sha256 matches manifest", status="PASS"))

    for result in results:
        _print_result(result)

    failed = [r for r in results if r.status == "FAIL"]
    warned = [r for r in results if r.status == "WARN"]
    skipped = [r for r in results if r.status == "SKIP"]
    passed = [r for r in results if r.status == "PASS"]

    print()
    print(f"Summary: {len(passed)} passed, {len(failed)} failed, {len(warned)} warned, {len(skipped)} skipped")

    return 1 if failed or (strict and warned) else 0


def main(argv: Sequence[str]) -> int:
    args = _parse_args(argv)
    if args.mode == "chunked":
        return _main_chunked(args)
    base_url: str = args.base_url
    origin: str | None = args.origin
    timeout_s: float = args.timeout
    strict: bool = bool(args.strict)
    expect_corp: str | None = args.expect_corp
    max_body_bytes: int = int(args.max_body_bytes)

    token: str | None = args.token
    authorization: str | None = _authorization_value(token) if token else None

    print("Disk streaming conformance")
    print(f"  BASE_URL: {base_url}")
    print(f"  ORIGIN:   {origin or '(none)'}")
    print(f"  ACCEPT_ENCODING: {_BROWSER_ACCEPT_ENCODING}")
    print(f"  STRICT:   {strict}")
    print(f"  CORP:     {expect_corp or '(not required)'}")
    print(f"  MAX_BODY_BYTES: {max_body_bytes} ({_fmt_bytes(max_body_bytes)})")
    if authorization is None:
        print("  AUTH:     (none)")
    else:
        # Don't print the actual token; CI logs are forever.
        print("  AUTH:     provided (token hidden)")
    print()

    results: list[TestResult] = []

    if authorization is not None:
        results.append(
            _test_private_requires_auth(
                base_url=base_url,
                origin=origin,
                timeout_s=timeout_s,
                max_body_bytes=max_body_bytes,
            )
        )

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
    get_etag_result, get_etag_resp = _test_get_etag_matches_head(
        base_url=base_url,
        origin=origin,
        authorization=authorization,
        timeout_s=timeout_s,
        max_body_bytes=max_body_bytes,
        head_etag=etag,
    )
    results.append(get_etag_result)
    results.append(
        _test_cors_expose_etag_if_present(
            name="GET: CORS exposes ETag when present",
            resp=get_etag_resp,
            origin=origin,
        )
    )
    results.append(
        _test_cors_expose_etag_if_present(
            name="HEAD: CORS exposes ETag when present",
            resp=head_info.resp if head_info is not None else None,
            origin=origin,
        )
    )
    results.append(
        _test_content_headers(
            name="HEAD: Content-Type is application/octet-stream and X-Content-Type-Options=nosniff",
            resp=head_info.resp if head_info is not None else None,
        )
    )
    results.append(
        _test_cors_expose_content_encoding_if_present(
            name="HEAD: CORS exposes Content-Encoding when present",
            resp=head_info.resp if head_info is not None else None,
            origin=origin,
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
            max_body_bytes=max_body_bytes,
        )
    )
    get_content_headers_result, get_content_headers_resp = _test_get_content_headers(
        base_url=base_url,
        origin=origin,
        authorization=authorization,
        timeout_s=timeout_s,
    )
    results.append(get_content_headers_result)
    results.append(
        _test_cors_expose_content_encoding_if_present(
            name="GET: CORS exposes Content-Encoding when present",
            resp=get_content_headers_resp,
            origin=origin,
        )
    )

    results.append(
        _test_get_valid_range(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
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
            max_body_bytes=max_body_bytes,
            strict=strict,
        )
    )
    results.append(
        _test_get_mid_range(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
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
            max_body_bytes=max_body_bytes,
            size=size,
        )
    )
    results.append(
        _test_if_range_matches_etag(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
            size=size,
            etag=etag,
            strict=strict,
        )
    )
    results.append(
        _test_if_range_matches_last_modified(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
            size=size,
            last_modified=last_modified,
            strict=strict,
        )
    )
    results.append(
        _test_if_range_mismatch(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            max_body_bytes=max_body_bytes,
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
            max_body_bytes=max_body_bytes,
        )
    )
    results.append(
        _test_conditional_if_modified_since(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            last_modified=last_modified,
            max_body_bytes=max_body_bytes,
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
            max_body_bytes=max_body_bytes,
        )
    )
    results.append(
        _test_options_preflight_if_modified_since(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            last_modified=last_modified,
            max_body_bytes=max_body_bytes,
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

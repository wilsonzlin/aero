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
            body = b"" if method == "HEAD" else resp.read()
            return HttpResponse(
                url=resp.geturl(),
                status=int(resp.status),
                reason=getattr(resp, "reason", ""),
                headers=_collapse_headers(resp.headers),
                body=body,
            )
    except urllib.error.HTTPError as e:
        body = b"" if method == "HEAD" else e.read()
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
    status: str  # PASS | FAIL | SKIP
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
        )
        _require_cors(resp, origin)
        _require(resp.status in (401, 403), f"expected 401/403, got {resp.status}")
        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


def _test_head(
    *,
    base_url: str,
    origin: str | None,
    authorization: str | None,
    timeout_s: float,
) -> tuple[TestResult, int | None]:
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
        _require_cors(resp, origin, expose={"accept-ranges", "content-length"})
        return (
            TestResult(
                name=name,
                status="PASS",
                details=f"size={size} ({_fmt_bytes(size)})",
            ),
            size,
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
) -> TestResult:
    name = "GET: valid Range returns 206 with correct Content-Range and body length"
    if size is None:
        return TestResult(name=name, status="SKIP", details="skipped (size unknown)")

    try:
        req_start = 0
        req_end = 0
        headers: dict[str, str] = {
            "Accept-Encoding": "identity",
            "Range": f"bytes={req_start}-{req_end}",
        }
        if origin is not None:
            headers["Origin"] = origin
        if authorization is not None:
            headers["Authorization"] = authorization

        resp = _request(url=base_url, method="GET", headers=headers, timeout_s=timeout_s)
        _require(resp.status == 206, f"expected 206, got {resp.status}")

        content_range = _header(resp, "Content-Range")
        _require(content_range is not None, "missing Content-Range header")
        _require_cors(resp, origin, expose={"content-range"})
        start, end, total = _parse_content_range(content_range)
        _require(start == req_start and end == req_end, f"expected bytes {req_start}-{req_end}, got {start}-{end}")
        _require(total == size, f"expected total size {size}, got {total}")

        expected_len = req_end - req_start + 1
        _require(len(resp.body) == expected_len, f"expected body length {expected_len}, got {len(resp.body)}")

        content_length = _header(resp, "Content-Length")
        if content_length is not None:
            try:
                resp_len = int(content_length)
            except ValueError:
                raise TestFailure(f"invalid Content-Length {content_length!r}") from None
            _require(resp_len == expected_len, f"expected Content-Length {expected_len}, got {resp_len}")

        return TestResult(name=name, status="PASS", details=f"Content-Range={content_range!r}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


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

        resp = _request(url=base_url, method="GET", headers=headers, timeout_s=timeout_s)
        _require(resp.status == 416, f"expected 416, got {resp.status}")

        content_range = _header(resp, "Content-Range")
        _require(content_range is not None, "missing Content-Range header")
        _require_cors(resp, origin, expose={"content-range"})

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
    timeout_s: float,
) -> TestResult:
    name = "OPTIONS: CORS preflight allows Range + Authorization headers"
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
                "Access-Control-Request-Headers": "range,authorization",
            },
            timeout_s=timeout_s,
            follow_redirects=False,
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
        _require("get" in _csv_tokens(allow_methods) or "*" in _csv_tokens(allow_methods), f"GET not allowed: {allow_methods!r}")

        allow_headers = _header(resp, "Access-Control-Allow-Headers")
        _require(allow_headers is not None, "missing Access-Control-Allow-Headers")
        allowed = _csv_tokens(allow_headers)
        _require(
            "*" in allowed or {"range", "authorization"}.issubset(allowed),
            f"expected Allow-Headers to include range,authorization; got {allow_headers!r}",
        )

        return TestResult(name=name, status="PASS", details=f"status={resp.status}")
    except TestFailure as e:
        return TestResult(name=name, status="FAIL", details=str(e))


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
    return args


def main(argv: Sequence[str]) -> int:
    args = _parse_args(argv)
    base_url: str = args.base_url
    origin: str | None = args.origin
    timeout_s: float = args.timeout

    token: str | None = args.token
    authorization: str | None = _authorization_value(token) if token else None

    print("Disk streaming conformance")
    print(f"  BASE_URL: {base_url}")
    print(f"  ORIGIN:   {origin or '(none)'}")
    if authorization is None:
        print("  AUTH:     (none)")
    else:
        # Don't print the actual token; CI logs are forever.
        print("  AUTH:     provided (token hidden)")
    print()

    results: list[TestResult] = []

    if authorization is not None:
        results.append(_test_private_requires_auth(base_url=base_url, origin=origin, timeout_s=timeout_s))

    head_result, size = _test_head(
        base_url=base_url,
        origin=origin,
        authorization=authorization,
        timeout_s=timeout_s,
    )
    results.append(head_result)

    results.append(
        _test_get_valid_range(
            base_url=base_url,
            origin=origin,
            authorization=authorization,
            timeout_s=timeout_s,
            size=size,
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
    results.append(_test_options_preflight(base_url=base_url, origin=origin, timeout_s=timeout_s))

    for result in results:
        _print_result(result)

    failed = [r for r in results if r.status == "FAIL"]
    skipped = [r for r in results if r.status == "SKIP"]
    passed = [r for r in results if r.status == "PASS"]

    print()
    print(f"Summary: {len(passed)} passed, {len(failed)} failed, {len(skipped)} skipped")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

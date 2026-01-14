#!/usr/bin/env python3
"""
Guardrail: keep Win7 guest selftest and host harness constants in sync.

This check prevents silent drift between:
  - Guest selftest expectations:
      drivers/windows7/tests/guest-selftest/src/main.cpp
  - Python host harness server constants:
      drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py
  - PowerShell host harness server constants:
      drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1

What is validated:
  - Deterministic large HTTP payload invariants:
      - payload is 1 MiB of bytes 0..255 repeating
      - ETag token/value matches the deterministic payload FNV-1a64 (0x8505ae4435522325)
      - upload verification SHA-256 matches the deterministic payload
  - Default HTTP port/path alignment:
      guest default http_url vs harness defaults (port + path)
  - Stable QMP device IDs for virtio-input routing:
      aero_virtio_kbd0 / aero_virtio_mouse0 / aero_virtio_tablet0
      (Python harness vs PowerShell harness)
"""

from __future__ import annotations

import hashlib
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse


REPO_ROOT = Path(__file__).resolve().parents[2]

GUEST_SELFTEST_MAIN = REPO_ROOT / "drivers/windows7/tests/guest-selftest/src/main.cpp"
PY_HARNESS = REPO_ROOT / "drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py"
PS_HARNESS = REPO_ROOT / "drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1"

# Canonical deterministic payload: 1 MiB of bytes 0..255 repeating.
PAYLOAD_SIZE = 1024 * 1024
PAYLOAD_PATTERN = bytes(range(256))

# For convenience we also hardcode the known-good results. The script still computes
# the values from the payload; these constants exist to keep the check self-consistent
# and to improve error messages if something unexpected changes.
EXPECTED_ETAG_TOKEN = "8505ae4435522325"
EXPECTED_UPLOAD_SHA256 = "fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83"


def _fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def _read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        _fail(f"missing required file: {path.as_posix()}")
    except OSError as e:
        _fail(f"failed to read {path.as_posix()}: {e}")


def _re_search(pattern: str, text: str, *, file: Path, desc: str, flags: int = 0) -> re.Match[str]:
    m = re.search(pattern, text, flags)
    if not m:
        _fail(f"{file.as_posix()}: failed to parse {desc} (pattern: {pattern})")
    return m


def _extract_single_hex(
    *,
    pattern: str,
    text: str,
    file: Path,
    desc: str,
    digits: int,
    flags: int = 0,
) -> str:
    m = _re_search(pattern, text, file=file, desc=desc, flags=flags)
    value = m.group("hex")
    if not re.fullmatch(rf"[0-9a-fA-F]{{{digits}}}", value):
        _fail(f"{file.as_posix()}: parsed {desc} is not {digits} hex digits: {value!r}")
    return value.lower()


def _fnv1a64(data: bytes) -> int:
    # FNV-1a 64-bit.
    h = 0xCBF29CE484222325
    prime = 0x100000001B3
    for b in data:
        h ^= b
        h = (h * prime) & 0xFFFFFFFFFFFFFFFF
    return h


def _deterministic_payload() -> bytes:
    if PAYLOAD_SIZE % len(PAYLOAD_PATTERN) != 0:
        raise AssertionError("payload size must be a multiple of the pattern length")
    return PAYLOAD_PATTERN * (PAYLOAD_SIZE // len(PAYLOAD_PATTERN))


def _normalize_etag_token(token: str) -> str:
    """
    Normalize a string that may be a raw token, quoted token, or weak ETag.

    Examples:
      - 8505ae...
      - "8505ae..."
      - W/"8505ae..."
    """

    t = token.strip()
    if t.lower().startswith("w/"):
        t = t[2:].lstrip()
    if len(t) >= 2 and t[0] == '"' and t[-1] == '"':
        t = t[1:-1]
    return t.strip().lower()


@dataclass(frozen=True)
class _HttpDefaults:
    port: int
    path: str


def _parse_guest_http_defaults(text: str) -> _HttpDefaults:
    m = _re_search(
        r'http_url\s*=\s*L"(?P<url>[^"]+)"',
        text,
        file=GUEST_SELFTEST_MAIN,
        desc="default Options.http_url",
        flags=re.M,
    )
    url = m.group("url")
    parsed = urlparse(url)
    if not parsed.scheme or not parsed.hostname:
        _fail(f"{GUEST_SELFTEST_MAIN.as_posix()}: invalid default http_url: {url!r}")
    if parsed.scheme != "http":
        _fail(
            f"{GUEST_SELFTEST_MAIN.as_posix()}: unexpected default http_url scheme={parsed.scheme!r} (expected 'http')"
        )
    if parsed.port is None:
        _fail(f"{GUEST_SELFTEST_MAIN.as_posix()}: default http_url missing explicit port: {url!r}")
    if not parsed.path:
        _fail(f"{GUEST_SELFTEST_MAIN.as_posix()}: default http_url missing path: {url!r}")
    return _HttpDefaults(port=int(parsed.port), path=parsed.path)


def _parse_python_http_defaults(text: str) -> _HttpDefaults:
    port_m = _re_search(
        r'"\-\-http-port".*?^\s*default\s*=\s*(?P<port>\d+)\s*,',
        text,
        file=PY_HARNESS,
        desc="argparse default for --http-port",
        flags=re.M | re.S,
    )
    path_m = _re_search(
        r'"\-\-http-path".*?^\s*default\s*=\s*(?P<quote>[\'"])(?P<path>.+?)(?P=quote)\s*,',
        text,
        file=PY_HARNESS,
        desc="argparse default for --http-path",
        flags=re.M | re.S,
    )
    return _HttpDefaults(port=int(port_m.group("port"), 10), path=str(path_m.group("path")))


def _parse_ps_http_defaults(text: str) -> _HttpDefaults:
    port_m = _re_search(
        r"\[int\]\$HttpPort\s*=\s*(?P<port>\d+)\s*,",
        text,
        file=PS_HARNESS,
        desc="param default for $HttpPort",
        flags=re.M,
    )
    path_m = _re_search(
        r"\[string\]\$HttpPath\s*=\s*(?P<quote>[\"'])(?P<path>.+?)(?P=quote)\s*,",
        text,
        file=PS_HARNESS,
        desc="param default for $HttpPath",
        flags=re.M,
    )
    return _HttpDefaults(port=int(port_m.group("port"), 10), path=str(path_m.group("path")))


def main() -> int:
    guest_text = _read_text(GUEST_SELFTEST_MAIN)
    py_text = _read_text(PY_HARNESS)
    ps_text = _read_text(PS_HARNESS)

    payload = _deterministic_payload()
    if len(payload) != PAYLOAD_SIZE:
        _fail(f"internal error: deterministic payload size mismatch: {len(payload)} != {PAYLOAD_SIZE}")

    fnv = _fnv1a64(payload)
    fnv_hex = f"{fnv:016x}"
    sha256_hex = hashlib.sha256(payload).hexdigest()

    errors: list[str] = []

    if fnv_hex != EXPECTED_ETAG_TOKEN:
        errors.append(
            "computed FNV-1a64 for deterministic payload does not match EXPECTED_ETAG_TOKEN "
            f"(computed={fnv_hex} expected={EXPECTED_ETAG_TOKEN})"
        )
    if sha256_hex != EXPECTED_UPLOAD_SHA256:
        errors.append(
            "computed SHA-256 for deterministic payload does not match EXPECTED_UPLOAD_SHA256 "
            f"(computed={sha256_hex} expected={EXPECTED_UPLOAD_SHA256})"
        )

    # Guest expected payload hash (FNV-1a64).
    guest_expected_hash_hex = _extract_single_hex(
        pattern=r"kExpectedHash\s*=\s*0x(?P<hex>[0-9A-Fa-f]+)u?ll",
        text=guest_text,
        file=GUEST_SELFTEST_MAIN,
        desc="guest expected FNV-1a64 hash constant (kExpectedHash)",
        digits=16,
        flags=re.M,
    )
    if guest_expected_hash_hex != fnv_hex:
        errors.append(
            f"{GUEST_SELFTEST_MAIN.as_posix()}: kExpectedHash mismatch "
            f"(got=0x{guest_expected_hash_hex} expected=0x{fnv_hex})"
        )

    # Guest expected ETag token for logging/hints.
    guest_etag_hex = _extract_single_hex(
        pattern=r'e\s*!=\s*L"(?P<hex>[0-9A-Fa-f]{16})"',
        text=guest_text,
        file=GUEST_SELFTEST_MAIN,
        desc="guest expected ETag token (wide string literal in HTTP GET large)",
        digits=16,
        flags=re.M,
    )
    if guest_etag_hex != fnv_hex:
        errors.append(
            f"{GUEST_SELFTEST_MAIN.as_posix()}: ETag token mismatch "
            f"(got={guest_etag_hex} expected={fnv_hex})"
        )

    # Python harness ETag + upload SHA.
    py_large_etag_raw = _re_search(
        r"^\s*large_etag\s*:\s*str\s*=\s*(?P<quote>[\"'])(?P<etag>.+?)(?P=quote)\s*$",
        py_text,
        file=PY_HARNESS,
        desc="Python harness large_etag constant",
        flags=re.M,
    ).group("etag")
    py_large_etag_hex = _normalize_etag_token(py_large_etag_raw)
    if not re.fullmatch(r"[0-9a-f]{16}", py_large_etag_hex):
        errors.append(
            f"{PY_HARNESS.as_posix()}: large_etag does not contain a 16-hex-digit token after normalization "
            f"(raw={py_large_etag_raw!r} normalized={py_large_etag_hex!r})"
        )
    elif py_large_etag_hex != fnv_hex:
        errors.append(
            f"{PY_HARNESS.as_posix()}: large_etag token mismatch "
            f"(got={py_large_etag_hex} expected={fnv_hex})"
        )

    py_upload_sha = _extract_single_hex(
        pattern=r"^\s*large_upload_sha256\s*:\s*str\s*=\s*(?P<quote>[\"'])(?P<hex>[0-9A-Fa-f]{64})(?P=quote)\s*$",
        text=py_text,
        file=PY_HARNESS,
        desc="Python harness large_upload_sha256 constant",
        digits=64,
        flags=re.M,
    )
    if py_upload_sha != sha256_hex:
        errors.append(
            f"{PY_HARNESS.as_posix()}: large_upload_sha256 mismatch "
            f"(got={py_upload_sha} expected={sha256_hex})"
        )

    # PowerShell harness ETag + upload SHA.
    ps_etag_hex = _extract_single_hex(
        pattern=r"ETag:\s*`\"(?P<hex>[0-9A-Fa-f]{16})`\"",
        text=ps_text,
        file=PS_HARNESS,
        desc="PowerShell harness ETag token",
        digits=16,
        flags=re.M,
    )
    if ps_etag_hex != fnv_hex:
        errors.append(
            f"{PS_HARNESS.as_posix()}: ETag token mismatch (got={ps_etag_hex} expected={fnv_hex})"
        )

    ps_upload_sha = _extract_single_hex(
        pattern=r'\$uploadSha256\s*-eq\s*"(?P<hex>[0-9A-Fa-f]{64})"',
        text=ps_text,
        file=PS_HARNESS,
        desc="PowerShell harness upload SHA-256 comparison constant",
        digits=64,
        flags=re.M,
    )
    if ps_upload_sha != sha256_hex:
        errors.append(
            f"{PS_HARNESS.as_posix()}: upload SHA-256 constant mismatch "
            f"(got={ps_upload_sha} expected={sha256_hex})"
        )

    # Default HTTP port/path alignment between guest and harnesses.
    guest_http = _parse_guest_http_defaults(guest_text)
    py_http = _parse_python_http_defaults(py_text)
    ps_http = _parse_ps_http_defaults(ps_text)

    if py_http.port != guest_http.port or py_http.path != guest_http.path:
        errors.append(
            f"{PY_HARNESS.as_posix()}: Python harness defaults do not match guest selftest Options.http_url "
            f"(python port={py_http.port} path={py_http.path!r}; guest port={guest_http.port} path={guest_http.path!r})"
        )
    if ps_http.port != guest_http.port or ps_http.path != guest_http.path:
        errors.append(
            f"{PS_HARNESS.as_posix()}: PowerShell harness defaults do not match guest selftest Options.http_url "
            f"(ps port={ps_http.port} path={ps_http.path!r}; guest port={guest_http.port} path={guest_http.path!r})"
        )

    # QMP virtio-input IDs must match between harness implementations.
    py_qmp_kbd = _re_search(
        r'^_VIRTIO_INPUT_QMP_KEYBOARD_ID\s*=\s*"(?P<id>[^"]+)"\s*$',
        py_text,
        file=PY_HARNESS,
        desc="Python harness _VIRTIO_INPUT_QMP_KEYBOARD_ID",
        flags=re.M,
    ).group("id")
    py_qmp_mouse = _re_search(
        r'^_VIRTIO_INPUT_QMP_MOUSE_ID\s*=\s*"(?P<id>[^"]+)"\s*$',
        py_text,
        file=PY_HARNESS,
        desc="Python harness _VIRTIO_INPUT_QMP_MOUSE_ID",
        flags=re.M,
    ).group("id")
    py_qmp_tablet = _re_search(
        r'^_VIRTIO_INPUT_QMP_TABLET_ID\s*=\s*"(?P<id>[^"]+)"\s*$',
        py_text,
        file=PY_HARNESS,
        desc="Python harness _VIRTIO_INPUT_QMP_TABLET_ID",
        flags=re.M,
    ).group("id")

    ps_qmp_kbd = _re_search(
        r'^\$script:VirtioInputKeyboardQmpId\s*=\s*"(?P<id>[^"]+)"\s*$',
        ps_text,
        file=PS_HARNESS,
        desc="PowerShell harness $script:VirtioInputKeyboardQmpId",
        flags=re.M,
    ).group("id")
    ps_qmp_mouse = _re_search(
        r'^\$script:VirtioInputMouseQmpId\s*=\s*"(?P<id>[^"]+)"\s*$',
        ps_text,
        file=PS_HARNESS,
        desc="PowerShell harness $script:VirtioInputMouseQmpId",
        flags=re.M,
    ).group("id")
    ps_qmp_tablet = _re_search(
        r'^\$script:VirtioInputTabletQmpId\s*=\s*"(?P<id>[^"]+)"\s*$',
        ps_text,
        file=PS_HARNESS,
        desc="PowerShell harness $script:VirtioInputTabletQmpId",
        flags=re.M,
    ).group("id")

    if py_qmp_kbd != ps_qmp_kbd:
        errors.append(
            "virtio-input QMP keyboard ID mismatch between harnesses "
            f"(python={py_qmp_kbd!r} ps={ps_qmp_kbd!r})"
        )
    if py_qmp_mouse != ps_qmp_mouse:
        errors.append(
            "virtio-input QMP mouse ID mismatch between harnesses "
            f"(python={py_qmp_mouse!r} ps={ps_qmp_mouse!r})"
        )
    if py_qmp_tablet != ps_qmp_tablet:
        errors.append(
            "virtio-input QMP tablet ID mismatch between harnesses "
            f"(python={py_qmp_tablet!r} ps={ps_qmp_tablet!r})"
        )

    if errors:
        for e in errors:
            print(f"error: {e}", file=sys.stderr)
        print(
            "\nRun locally with:\n  python3 scripts/ci/check-win7-harness-constants.py",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())


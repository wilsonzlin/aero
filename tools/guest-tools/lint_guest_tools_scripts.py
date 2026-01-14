#!/usr/bin/env python3
"""
Static guardrail linter for Aero Guest Tools scripts.

Why this exists:
- `guest-tools/setup.cmd` and `guest-tools/verify.ps1` implement safety-critical
  behaviours (boot-critical virtio-blk pre-seeding + signature enforcement policy).
- These behaviours are hard to integration-test in CI (Win7-only, registry/BCD changes).
- This linter provides a lightweight, cross-platform check that fails CI if critical
  logic is accidentally removed or renamed.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, List, Sequence


class LintError(RuntimeError):
    pass


REPO_ROOT = Path(__file__).resolve().parents[2]


DEFAULT_SETUP_CMD = REPO_ROOT / "guest-tools" / "setup.cmd"
DEFAULT_UNINSTALL_CMD = REPO_ROOT / "guest-tools" / "uninstall.cmd"
DEFAULT_VERIFY_PS1 = REPO_ROOT / "guest-tools" / "verify.ps1"


@dataclass(frozen=True)
class Invariant:
    description: str
    expected_hint: str
    predicate: Callable[[str], bool]


def _contains(substr: str) -> Callable[[str], bool]:
    return lambda text: substr in text


def _all_contains(substrings: Sequence[str]) -> Callable[[str], bool]:
    return lambda text: all(s in text for s in substrings)


def _any_contains(substrings: Sequence[str]) -> Callable[[str], bool]:
    return lambda text: any(s in text for s in substrings)


def _regex(pattern: str, *, flags: int = re.IGNORECASE | re.MULTILINE) -> Callable[[str], bool]:
    rx = re.compile(pattern, flags)
    return lambda text: rx.search(text) is not None


def _all_regex(patterns: Sequence[str], *, flags: int = re.IGNORECASE | re.MULTILINE) -> Callable[[str], bool]:
    compiled = [re.compile(p, flags) for p in patterns]
    return lambda text: all(rx.search(text) is not None for rx in compiled)


def _any_regex(patterns: Sequence[str], *, flags: int = re.IGNORECASE | re.MULTILINE) -> Callable[[str], bool]:
    compiled = [re.compile(p, flags) for p in patterns]
    return lambda text: any(rx.search(text) is not None for rx in compiled)


def _parsed_flag_sets_var(*, flag_variants: Sequence[str], var: str, value: str) -> Callable[[str], bool]:
    """
    Best-effort check that a batch arg parsing line maps a CLI flag to a variable assignment.

    We intentionally match loosely to tolerate minor refactors:
    - quoted or unquoted flag comparisons
    - `%%A` vs `%%~A`
    - `set VAR=...` vs `set "VAR=..."`

    The invariant is still anchored to lines beginning with `if` to avoid being
    satisfied by usage/help text.
    """

    alt = "|".join(re.escape(v) for v in flag_variants)
    pattern = rf'(?im)^\s*if\b[^\r\n]*(?:{alt})[^\r\n]*\bset\b[^\r\n]*"?{re.escape(var)}\s*=\s*{re.escape(value)}"?'
    return _regex(pattern)


def _label_block(text: str, label: str) -> str | None:
    """
    Extract a best-effort "label block" from a Windows batch script.

    Returns the text starting at `:<label>` up to (but not including) the next label
    line (`^:[A-Za-z0-9_]+`), or EOF.

    This is intentionally heuristic and used only for lint invariants that need to
    inspect a specific mode section (e.g. :check_mode).
    """

    m = re.search(rf"(?im)^:{re.escape(label)}\b", text)
    if not m:
        return None
    start = m.start()
    m2 = re.search(r"(?im)^:[A-Za-z0-9_]+\b", text[m.end() :])
    end = m.end() + m2.start() if m2 else len(text)
    return text[start:end]


def _strip_label_block(text: str, label: str) -> str:
    """
    Return `text` with the best-effort label block `:<label>` removed.

    Useful for invariants that must apply to the "install mode" flow and should not
    be satisfied by logic that exists only in a dry-run label like :check_mode.
    """

    m = re.search(rf"(?im)^:{re.escape(label)}\b", text)
    if not m:
        return text
    start = m.start()
    m2 = re.search(r"(?im)^:[A-Za-z0-9_]+\b", text[m.end() :])
    end = m.end() + m2.start() if m2 else len(text)
    return text[:start] + "\n" + text[end:]


def _strip_batch_comment_lines(text: str) -> str:
    """
    Best-effort removal of batch comment lines.

    This lets us search for potentially dangerous commands without false positives
    from docs/comments like:
      rem - no certutil -addstore
    """

    out_lines: list[str] = []
    for line in text.splitlines():
        stripped = line.lstrip()
        if not stripped:
            continue
        lower = stripped.lower()
        # Batch comments are line-based. We intentionally do not attempt to
        # handle inline `rem` or `::` comments.
        if lower == "rem" or lower.startswith("rem "):
            continue
        if lower.startswith("::"):
            continue
        out_lines.append(line)
    return "\n".join(out_lines)


def _has_install_certs_policy_gate(text: str) -> bool:
    """
    Guardrail for production/none certificate policy.

    We expect setup.cmd to *skip* importing certificates by default when
    SIGNING_POLICY != test (production/none Guest Tools media should not ship certs),
    and to do so *before* any certutil -addstore operations occur.

    This check is intentionally heuristic and tolerant to small refactors.
    """

    block = _label_block(text, "install_certs")
    if block is None:
        return False

    sp_var = r"(?:%SIGNING_POLICY%|!SIGNING_POLICY!)"
    ic_var = r"(?:%ARG_INSTALL_CERTS%|!ARG_INSTALL_CERTS!)"

    # Find non-test gating checks and ensure they lead to an early "exit /b 0"
    # soon afterwards (so it is actually a skip-by-policy, not just a warning).
    for m in re.finditer(rf"(?im)^\s*if\b[^\r\n]*{sp_var}[^\r\n]*$", block):
        line = m.group(0)
        is_non_test_policy_check = (
            re.search(rf'(?i)\bnot\s+"{sp_var}"\s*==\s*"test"', line) is not None
            or re.search(rf'(?i)"{sp_var}"\s*==\s*"(production|none)"', line) is not None
        )
        if not is_non_test_policy_check:
            continue

        # If the policy gate mentions ARG_INSTALL_CERTS, ensure it's the skip-side
        # condition (NOT the override-side warning branch).
        if re.search(rf"(?i){ic_var}", line) is not None and _any_regex(
            [
                rf'(?i)\bnot\s+"{ic_var}"\s*==\s*"1"',
                rf'(?i)"{ic_var}"\s*==\s*"0"',
            ]
        )(line) is False:
            continue

        # Look for an early return close to the if line (avoid accidentally
        # matching the normal function epilogue).
        post = block[m.end() : m.end() + 800]
        if re.search(r"(?i)\bexit\s+/b\s+0\b", post) is not None:
            return True

    return False


def _has_testsigning_policy_gate(text: str) -> bool:
    """
    Guardrail for signature policy: production/none media should not prompt/modify
    Test Signing unless explicitly requested.
    """

    block = _label_block(text, "maybe_enable_testsigning")
    if block is None:
        return False
    block = _strip_batch_comment_lines(block)

    sp_var = r"(?:%SIGNING_POLICY%|!SIGNING_POLICY!)"
    ft_var = r"(?:%ARG_FORCE_TESTSIGN%|!ARG_FORCE_TESTSIGN!)"

    # Current implementation uses a combined IF with an early exit /b 0. Accept
    # minor variations (whitespace, delayed expansion) but keep semantics.
    return (
        re.search(
            rf'(?ims)^\s*if\s+/i\s+not\s+"{sp_var}"\s*==\s*"test"\s+if\s+not\s+"{ft_var}"\s*==\s*"1"\s*\([\s\S]*?^\s*exit\s+/b\s+0\b',
            block,
        )
        is not None
    )


def _check_mode_dispatch_precedes_admin_requirement(text: str) -> bool:
    """
    Ensure /check mode runs before any "install mode" admin gate.

    setup.cmd intentionally supports a non-destructive validation path that does not
    require elevation. If the jump to :check_mode is accidentally moved below the
    admin check, /check becomes unusable for non-admin users/automation.
    """

    admin_positions: list[int] = []
    for pat in [
        r"(?im)^\s*call\s+:?require_admin_stdout\b",
        r"(?im)^\s*call\s+:?require_admin\b",
    ]:
        m = re.search(pat, text)
        if m:
            admin_positions.append(m.start())
    if not admin_positions:
        # If the script no longer has an explicit admin gate, we can't enforce the
        # ordering invariant (and /check is trivially before it).
        return True

    admin_pos = min(admin_positions)

    dispatch_positions: list[int] = []
    for pat in [
        r'(?im)^\s*if\b[^\r\n]*"%ARG_CHECK%"\s*==\s*"1"[^\r\n]*goto\s+:?check_mode\b',
        r'(?im)^\s*if\b[^\r\n]*"/check"[^\r\n]*goto\s+:?check_mode\b',
        r'(?im)^\s*if\b[^\r\n]*"/validate"[^\r\n]*goto\s+:?check_mode\b',
    ]:
        m = re.search(pat, text)
        if m:
            dispatch_positions.append(m.start())
    if not dispatch_positions:
        return False

    return min(dispatch_positions) < admin_pos


def _has_cleanupstorage_force_gate(text: str) -> bool:
    """
    Guardrail for uninstall safety: /cleanupstorage is boot-critical and must be
    ignored in non-interactive mode unless explicitly forced.
    """

    block = _label_block(text, "maybe_cleanup_storage_preseed")
    if block is None:
        return False
    block = _strip_batch_comment_lines(block)

    af_var = r"(?:%ARG_FORCE%|!ARG_FORCE!)"
    csf_var = r"(?:%ARG_CLEANUP_STORAGE_FORCE%|!ARG_CLEANUP_STORAGE_FORCE!)"

    # Require a "force-mode gate" that exits early unless cleanupstorageforce is set.
    return (
        re.search(
            rf'(?ims)^\s*if\s+"{af_var}"\s*==\s*"1"\s+if\s+not\s+"{csf_var}"\s*==\s*"1"\s*\([\s\S]*?^\s*exit\s+/b\s+0\b',
            block,
        )
        is not None
        or re.search(
            rf'(?ims)^\s*if\s+"{af_var}"\s*==\s*"1"\s+if\s+"{csf_var}"\s*==\s*"0"\s*\([\s\S]*?^\s*exit\s+/b\s+0\b',
            block,
        )
        is not None
    )


def _read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError as e:
        raise LintError(f"File not found: {path}") from e
    except OSError as e:
        raise LintError(f"Failed to read {path}: {e}") from e


def lint_text(*, path: Path, text: str, invariants: Sequence[Invariant]) -> List[str]:
    errors: List[str] = []
    for inv in invariants:
        if inv.predicate(text):
            continue
        errors.append(
            "\n".join(
                [
                    f"{path}: missing invariant: {inv.description}",
                    f"  Expected: {inv.expected_hint}",
                ]
            )
        )
    return errors


def lint_files(*, setup_cmd: Path, uninstall_cmd: Path, verify_ps1: Path) -> List[str]:
    errors: List[str] = []

    setup_text = _read_text(setup_cmd)
    uninstall_text = _read_text(uninstall_cmd)
    verify_text = _read_text(verify_ps1)

    setup_invariants = [
        Invariant(
            description="CriticalDeviceDatabase base path is referenced (boot-critical storage preseed)",
            expected_hint=r"HKLM\\SYSTEM\\CurrentControlSet\\Control\\CriticalDeviceDatabase",
            predicate=_contains(r"HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase"),
        ),
        Invariant(
            description="CriticalDeviceDatabase is referenced (boot-critical storage preseed)",
            expected_hint="CriticalDeviceDatabase",
            predicate=_contains("CriticalDeviceDatabase"),
        ),
        Invariant(
            description="Uses AERO_VIRTIO_BLK_SERVICE (storage service name from config/devices.cmd)",
            expected_hint="AERO_VIRTIO_BLK_SERVICE",
            predicate=_contains("AERO_VIRTIO_BLK_SERVICE"),
        ),
        Invariant(
            description="Uses AERO_VIRTIO_BLK_HWIDS (HWID list used for CriticalDeviceDatabase preseed)",
            expected_hint="AERO_VIRTIO_BLK_HWIDS",
            predicate=_contains("AERO_VIRTIO_BLK_HWIDS"),
        ),
        Invariant(
            description="Sets storage service Start=0 (BOOT_START) via reg.exe",
            expected_hint="/v Start /t REG_DWORD /d 0",
            predicate=_regex(r"/v\s+Start\s+/t\s+REG_DWORD\s+/d\s+0\b"),
        ),
        Invariant(
            description="Supports signing_policy surface (test|production|none)",
            expected_hint='Validates SIGNING_POLICY against "test", "production", and "none"',
            predicate=_all_regex(
                [
                    r'(?i)%SIGNING_POLICY%"\s*==\s*"test"',
                    r'(?i)%SIGNING_POLICY%"\s*==\s*"production"',
                    r'(?i)%SIGNING_POLICY%"\s*==\s*"none"',
                ]
            ),
        ),
        Invariant(
            description="Supports /testsigning flag (x64 test driver signing)",
            expected_hint='Parses "/testsigning" (or alias) into ARG_FORCE_TESTSIGN=1',
            predicate=_parsed_flag_sets_var(
                flag_variants=["/testsigning", "/forcetestsigning", "/force-testsigning"],
                var="ARG_FORCE_TESTSIGN",
                value="1",
            ),
        ),
        Invariant(
            description="Supports /nointegritychecks flag (signature enforcement off; not recommended)",
            expected_hint='Parses "/nointegritychecks" (or alias) into ARG_FORCE_NOINTEGRITY=1',
            predicate=_parsed_flag_sets_var(
                flag_variants=["/nointegritychecks", "/forcenointegritychecks", "/no-integrity-checks"],
                var="ARG_FORCE_NOINTEGRITY",
                value="1",
            ),
        ),
        Invariant(
            description="Supports /forcesigningpolicy:none|test|production flag",
            expected_hint="/forcesigningpolicy:none, /forcesigningpolicy:test, /forcesigningpolicy:production",
            predicate=lambda text: (
                _parsed_flag_sets_var(
                    flag_variants=["/forcesigningpolicy:none"], var="ARG_FORCE_SIGNING_POLICY", value="none"
                )(text)
                and _parsed_flag_sets_var(
                    flag_variants=["/forcesigningpolicy:test"], var="ARG_FORCE_SIGNING_POLICY", value="test"
                )(text)
                and _parsed_flag_sets_var(
                    flag_variants=["/forcesigningpolicy:production"],
                    var="ARG_FORCE_SIGNING_POLICY",
                    value="production",
                )(text)
            ),
        ),
        Invariant(
            description="Supports /notestsigning flag (suppresses Test Signing changes)",
            expected_hint='Parses "/notestsigning" (or /no-testsigning) into ARG_SKIP_TESTSIGN=1',
            predicate=_parsed_flag_sets_var(
                flag_variants=["/notestsigning", "/no-testsigning"], var="ARG_SKIP_TESTSIGN", value="1"
            ),
        ),
        Invariant(
            description="Supports /installcerts override flag (force cert install for production/none; advanced)",
            expected_hint='Parses "/installcerts" (or /install-certs) into ARG_INSTALL_CERTS=1',
            predicate=_parsed_flag_sets_var(
                flag_variants=["/installcerts", "/install-certs"], var="ARG_INSTALL_CERTS", value="1"
            ),
        ),
        Invariant(
            description="Supports /verify-media flag (preflight Guest Tools media integrity via manifest.json)",
            expected_hint="/verify-media (or /verifymedia)",
            predicate=_any_regex([r"/verify-media\b", r"/verifymedia\b"]),
        ),
        Invariant(
            description="Parses /verify-media flag into ARG_VERIFY_MEDIA",
            expected_hint='if /i "%%~A"=="/verify-media" set "ARG_VERIFY_MEDIA=1"',
            predicate=_parsed_flag_sets_var(
                flag_variants=["/verify-media", "/verifymedia"], var="ARG_VERIFY_MEDIA", value="1"
            ),
        ),
        Invariant(
            description="Media integrity preflight exists (verify_media_preflight)",
            expected_hint=":verify_media_preflight label + manifest.json",
            predicate=_all_regex([r"(?im)^:verify_media_preflight\b", r"manifest\.json"]),
        ),
        Invariant(
            description="Wires /verify-media flag to verify_media_preflight",
            expected_hint='If "%ARG_VERIFY_MEDIA%"=="1" (...) call :verify_media_preflight',
            predicate=lambda text: _regex(
                r'(?is)if\s+"%ARG_VERIFY_MEDIA%"\s*==\s*"1"\s*\([\s\S]*?call\s+:?verify_media_preflight\b'
            )(_strip_batch_comment_lines(_strip_label_block(text, "check_mode"))),
        ),
        Invariant(
            description="Supports /check (or /validate) dry-run validation mode (no system changes)",
            expected_hint='Parses "/check"/"/validate" and jumps to :check_mode (e.g. if "%ARG_CHECK%"=="1" goto :check_mode)',
            predicate=lambda text: (
                _regex(r"(?im)^:check_mode\b")(text)
                and (
                    (
                        _any_regex(
                            [
                                r'(?i)/check"\s+set\s+"?ARG_CHECK=1"?',
                                r'(?i)/validate"\s+set\s+"?ARG_CHECK=1"?',
                            ]
                        )(text)
                        and _regex(r'(?i)\bif\b\s+"%ARG_CHECK%"\s*==\s*"1"\s+goto\s+:?check_mode\b')(text)
                    )
                    or _any_regex(
                        [
                            r'(?i)/check"\s+goto\s+:?check_mode\b',
                            r'(?i)/validate"\s+goto\s+:?check_mode\b',
                        ]
                    )(text)
                )
            ),
        ),
        Invariant(
            description="In /check mode, bypasses admin requirement (jump occurs before require_admin*)",
            expected_hint='The /check/:check_mode dispatch should happen before any `call :require_admin_stdout` or `call :require_admin`.',
            predicate=_check_mode_dispatch_precedes_admin_requirement,
        ),
        Invariant(
            description="In /check mode, logs to %TEMP% (does not require Administrator access to C:\\AeroGuestTools)",
            expected_hint='Under :check_mode, sets INSTALL_ROOT to a %TEMP% path (e.g. set "INSTALL_ROOT=%TEMP%\\AeroGuestToolsCheck")',
            predicate=lambda text: (
                (block := _label_block(text, "check_mode")) is not None
                and re.search(
                    r'(?i)\bset\s+"INSTALL_ROOT=%TEMP%[\\/]', _strip_batch_comment_lines(block)
                )
                is not None
            ),
        ),
        Invariant(
            description="In /check mode, validates certificate payload without importing certificates",
            expected_hint="Under :check_mode, calls :validate_cert_payload",
            predicate=lambda text: (
                (block := _label_block(text, "check_mode")) is not None
                and re.search(
                    r"(?i)\bcall\s+:?validate_cert_payload\b", _strip_batch_comment_lines(block)
                )
                is not None
            ),
        ),
        Invariant(
            description="In /check mode, avoids system-changing actions (no install_certs/stage_all_drivers/preseed_storage_boot)",
            expected_hint=":check_mode should not call :install_certs, :stage_all_drivers, :preseed_storage_boot, :maybe_enable_testsigning, or :skip_storage_preseed",
            predicate=lambda text: (
                (block := _label_block(text, "check_mode")) is not None
                and (block_exec := _strip_batch_comment_lines(block)) is not None
                and (
                    re.search(
                        r"(?i)\b(certutil|pnputil|reg|bcdedit|shutdown)(?:\.exe)?\b[^\r\n]*"
                        r"(?:-addstore|\s+-a\b|\s+-i\b|\s+(?:add|delete)\b|\s+/set\b|\s+/(?:r|s)\b)",
                        block_exec,
                    )
                    is None
                )
                and re.search(
                    r"(?i)\bcall\s+:?(install_certs|stage_all_drivers|preseed_storage_boot|maybe_enable_testsigning|skip_storage_preseed|require_admin_stdout|require_admin)\b",
                    block_exec,
                )
                is None
            ),
        ),
        Invariant(
            description="Supports /skipstorage flag (allows intentionally skipping boot-critical storage preseed)",
            expected_hint="/skipstorage",
            predicate=_regex(r"/skipstorage\b"),
        ),
        Invariant(
            description="Parses /skipstorage flag into ARG_SKIP_STORAGE",
            expected_hint='if /i "%%~A"=="/skipstorage" set "ARG_SKIP_STORAGE=1"',
            predicate=_parsed_flag_sets_var(
                flag_variants=["/skipstorage", "/skip-storage"], var="ARG_SKIP_STORAGE", value="1"
            ),
        ),
        Invariant(
            description="/skipstorage gates virtio-blk storage INF validation (setup does not require packaged storage driver)",
            expected_hint='If "%ARG_SKIP_STORAGE%"=="1" (...) else ( call :validate_storage_service_infs ... )',
            predicate=lambda text: _regex(
                r'if\s+"%ARG_SKIP_STORAGE%"\s*==\s*"1"\s*\([\s\S]{0,2000}?else\s*\([\s\S]{0,2000}?call\s+:?validate_storage_service_infs\b',
                flags=re.IGNORECASE | re.MULTILINE | re.DOTALL,
            )(_strip_batch_comment_lines(_strip_label_block(text, "check_mode"))),
        ),
        Invariant(
            description="/skipstorage gates boot-critical storage pre-seeding (skips preseed_storage_boot)",
            expected_hint='If "%ARG_SKIP_STORAGE%"=="1" ( call :skip_storage_preseed ... ) else ( call :preseed_storage_boot ... )',
            predicate=lambda text: _regex(
                r'if\s+"%ARG_SKIP_STORAGE%"\s*==\s*"1"\s*\([\s\S]{0,2000}?call\s+:?skip_storage_preseed\b[\s\S]{0,2000}?else\s*\([\s\S]{0,2000}?call\s+:?preseed_storage_boot\b',
                flags=re.IGNORECASE | re.MULTILINE | re.DOTALL,
            )(_strip_batch_comment_lines(_strip_label_block(text, "check_mode"))),
        ),
        Invariant(
            description="Writes storage preseed skip marker file when /skipstorage is used",
            expected_hint='storage-preseed.skipped.txt (written via > "%STATE_STORAGE_SKIPPED%" ...)',
            predicate=lambda text: (
                re.search(r"storage-preseed\.skipped\.txt", _strip_batch_comment_lines(text), re.IGNORECASE) is not None
                and _any_regex(
                    [
                        # Common implementation: write via marker variable.
                        r'(?im)[>]{1,2}\s*"?([%!])STATE_STORAGE_SKIPPED\1"?',
                        # Alternate: write directly to a path containing the marker filename.
                        r"(?im)[>]{1,2}[^\r\n]*storage-preseed\.skipped\.txt",
                    ]
                )(_strip_batch_comment_lines(text))
            ),
        ),
        Invariant(
            description="Certificate install requirement is gated by signing_policy (certs required only for test)",
            expected_hint='if /i "%SIGNING_POLICY%"=="test" set "CERTS_REQUIRED=1"',
            predicate=lambda text: _regex(
                r'(?im)^\s*if\s+(?:/i\s+)?"?%SIGNING_POLICY%"?\s*==\s*"test"\s+set\s+"?CERTS_REQUIRED=1"?'
            )(_strip_batch_comment_lines(text)),
        ),
        Invariant(
            description="Certificate installation is skipped by policy for signing_policy=production|none unless explicitly overridden",
            expected_hint=(
                'If signing_policy != test, setup should skip importing certs by default '
                '(e.g. `if /i not \"%SIGNING_POLICY%\"==\"test\" if not \"%ARG_INSTALL_CERTS%\"==\"1\" (...) exit /b 0`).'
            ),
            predicate=_has_install_certs_policy_gate,
        ),
        Invariant(
            description="Test Signing changes are gated by signing_policy (production/none does not prompt by default)",
            expected_hint='In :maybe_enable_testsigning, non-test signing_policy should early-exit unless /testsigning was requested (ARG_FORCE_TESTSIGN=1).',
            predicate=_has_testsigning_policy_gate,
        ),
        Invariant(
            description="Writes marker file when enabling Test Signing (used by uninstall/verify)",
            expected_hint='testsigning.enabled-by-aero.txt (written via > \"%STATE_TESTSIGN%\" ...)',
            predicate=lambda text: (
                re.search(r"testsigning\.enabled-by-aero\.txt", _strip_batch_comment_lines(text), re.IGNORECASE)
                is not None
                and _any_regex(
                    [
                        r'(?im)[>]{1,2}\s*"?([%!])STATE_TESTSIGN\1"?',
                        r"(?im)[>]{1,2}[^\r\n]*testsigning\.enabled-by-aero\.txt",
                    ]
                )(_strip_batch_comment_lines(text))
            ),
        ),
        Invariant(
            description="Writes marker file when enabling nointegritychecks (used by uninstall/verify)",
            expected_hint='nointegritychecks.enabled-by-aero.txt (written via > \"%STATE_NOINTEGRITY%\" ...)',
            predicate=lambda text: (
                re.search(
                    r"nointegritychecks\.enabled-by-aero\.txt", _strip_batch_comment_lines(text), re.IGNORECASE
                )
                is not None
                and _any_regex(
                    [
                        r'(?im)[>]{1,2}\s*"?([%!])STATE_NOINTEGRITY\1"?',
                        r"(?im)[>]{1,2}[^\r\n]*nointegritychecks\.enabled-by-aero\.txt",
                    ]
                )(_strip_batch_comment_lines(text))
            ),
        ),
        Invariant(
            description="Records installed media provenance (installed-media.txt)",
            expected_hint="installed-media.txt + :write_installed_media_state",
            predicate=_all_regex([r"installed-media\.txt", r"(?im)^:write_installed_media_state\b"]),
        ),
        Invariant(
            description="Calls write_installed_media_state during install",
            expected_hint="call :write_installed_media_state",
            predicate=lambda text: _regex(r"(?im)^\s*call\s+:?write_installed_media_state\b")(
                _strip_batch_comment_lines(text)
            ),
        ),
    ]

    verify_invariants = [
        Invariant(
            description="virtio-blk boot-critical registry check exists (CriticalDeviceDatabase)",
            expected_hint="CriticalDeviceDatabase",
            predicate=_contains("CriticalDeviceDatabase"),
        ),
        Invariant(
            description="verify detects when storage pre-seeding was skipped (/skipstorage marker file)",
            expected_hint="storage-preseed.skipped.txt + /skipstorage",
            predicate=_all_regex([r"storage-preseed\.skipped\.txt", r"skipstorage"]),
        ),
        Invariant(
            description="virtio-blk boot-critical registry check key is present (virtio_blk_boot_critical)",
            expected_hint="virtio_blk_boot_critical",
            predicate=_contains("virtio_blk_boot_critical"),
        ),
        Invariant(
            description="manifest.json signing_policy is parsed (verify reports effective signing policy)",
            expected_hint="manifest.json + signing_policy",
            predicate=_all_contains(["manifest.json", "signing_policy"]),
        ),
        Invariant(
            description="verify reads Guest Tools signature-mode marker files written by setup.cmd",
            expected_hint="testsigning.enabled-by-aero.txt + nointegritychecks.enabled-by-aero.txt",
            predicate=_all_regex([r"testsigning\.enabled-by-aero\.txt", r"nointegritychecks\.enabled-by-aero\.txt"]),
        ),
        Invariant(
            description="verify reads installed media provenance written by setup.cmd (installed-media.txt)",
            expected_hint="installed-media.txt",
            predicate=_regex(r"installed-media\.txt"),
        ),
    ]

    uninstall_invariants = [
        Invariant(
            description="Uninstaller references marker file for testsigning enabled by Guest Tools",
            expected_hint="testsigning.enabled-by-aero.txt",
            predicate=_contains("testsigning.enabled-by-aero.txt"),
        ),
        Invariant(
            description="Uninstaller references marker file for nointegritychecks enabled by Guest Tools",
            expected_hint="nointegritychecks.enabled-by-aero.txt",
            predicate=_contains("nointegritychecks.enabled-by-aero.txt"),
        ),
        Invariant(
            description="Uninstaller references marker file for storage preseed skipped by Guest Tools",
            expected_hint="storage-preseed.skipped.txt",
            predicate=_regex(r"storage-preseed\.skipped\.txt"),
        ),
        Invariant(
            description="Uninstaller references installed media provenance file written by setup.cmd",
            expected_hint="installed-media.txt",
            predicate=_regex(r"installed-media\.txt"),
        ),
        Invariant(
            description="Uninstaller parses /cleanupstorage and /cleanupstorageforce flags",
            expected_hint='ARG_CLEANUP_STORAGE=1 and ARG_CLEANUP_STORAGE_FORCE=1 assignments in arg parsing',
            predicate=lambda text: (
                _parsed_flag_sets_var(
                    flag_variants=["/cleanupstorage", "/cleanup-storage"], var="ARG_CLEANUP_STORAGE", value="1"
                )(text)
                and _parsed_flag_sets_var(
                    flag_variants=["/cleanupstorageforce", "/cleanup-storage-force"],
                    var="ARG_CLEANUP_STORAGE_FORCE",
                    value="1",
                )(text)
            ),
        ),
        Invariant(
            description="/cleanupstorage is gated in /force mode (ignored unless /cleanupstorageforce is provided)",
            expected_hint='In :maybe_cleanup_storage_preseed, if ARG_FORCE==1 and ARG_CLEANUP_STORAGE_FORCE!=1 then exit /b 0',
            predicate=_has_cleanupstorage_force_gate,
        ),
    ]

    errors.extend(lint_text(path=setup_cmd, text=setup_text, invariants=setup_invariants))
    errors.extend(lint_text(path=verify_ps1, text=verify_text, invariants=verify_invariants))
    errors.extend(lint_text(path=uninstall_cmd, text=uninstall_text, invariants=uninstall_invariants))

    return errors


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Static linter for Guest Tools safety-critical scripts.")
    parser.add_argument("--setup-cmd", type=Path, default=DEFAULT_SETUP_CMD, help="Path to guest-tools/setup.cmd")
    parser.add_argument("--uninstall-cmd", type=Path, default=DEFAULT_UNINSTALL_CMD, help="Path to guest-tools/uninstall.cmd")
    parser.add_argument("--verify-ps1", type=Path, default=DEFAULT_VERIFY_PS1, help="Path to guest-tools/verify.ps1")
    args = parser.parse_args(argv)

    try:
        errors = lint_files(setup_cmd=args.setup_cmd, uninstall_cmd=args.uninstall_cmd, verify_ps1=args.verify_ps1)
    except LintError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2

    if errors:
        for msg in errors:
            print(f"ERROR: {msg}", file=sys.stderr)
        print(f"Guest Tools script lint failed: {len(errors)} invariant(s) missing.", file=sys.stderr)
        return 1

    print("Guest Tools script lint OK.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

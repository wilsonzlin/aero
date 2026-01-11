#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 WDK feature macro semantics.

Several AeroGPU driver build flags are intentionally defined to `0` in non-WDK
builds. Preprocessor guards must therefore treat "defined but 0" as disabled.

The canonical pattern is:

  #if defined(MACRO) && MACRO

not:

  #if defined(MACRO)
  #ifdef MACRO

This script scans the AeroGPU driver sources for guard regressions so that a
future refactor cannot accidentally make `-DMACRO=0` behave like "on" again.
"""

from __future__ import annotations

import pathlib
import re
import sys
from dataclasses import dataclass
from typing import Iterable, Iterator


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()
SCAN_ROOTS = [ROOT / "drivers" / "aerogpu"]

SOURCE_EXTENSIONS = {
    ".h",
    ".hpp",
    ".c",
    ".cc",
    ".cpp",
    ".cxx",
    ".inl",
}

WDK_MACROS = [
    "AEROGPU_UMD_USE_WDK_HEADERS",
    "AEROGPU_D3D9_USE_WDK_DDI",
    "AEROGPU_KMD_USE_WDK_DDI",
]


@dataclass(frozen=True)
class Directive:
    path: pathlib.Path
    line: int
    kind: str
    text: str


def iter_source_files() -> Iterable[pathlib.Path]:
    for base in SCAN_ROOTS:
        if not base.is_dir():
            continue
        for path in base.rglob("*"):
            if not path.is_file():
                continue
            if path.suffix.lower() in SOURCE_EXTENSIONS:
                yield path


_DIRECTIVE_RE = re.compile(r"^\s*#\s*(if|elif|ifdef|ifndef)\b(.*)$", re.S)


def iter_directives(path: pathlib.Path) -> Iterator[Directive]:
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    i = 0
    while i < len(lines):
        raw = lines[i]
        if not raw.lstrip().startswith("#"):
            i += 1
            continue

        start_line = i + 1
        buf = raw

        # Handle line continuations in preprocessor directives.
        while buf.rstrip().endswith("\\") and i + 1 < len(lines):
            i += 1
            buf += "\n" + lines[i]

        i += 1

        m = _DIRECTIVE_RE.match(buf)
        if not m:
            continue

        kind = m.group(1)

        # Normalize whitespace/newlines to simplify downstream regex matching.
        collapsed = " ".join(part.strip() for part in buf.splitlines())
        yield Directive(path=path, line=start_line, kind=kind, text=collapsed)


def strip_comments(text: str) -> str:
    # Fast + good-enough comment stripping for preprocessor directive lines.
    text = re.sub(r"//.*", "", text)
    text = re.sub(r"/\*.*?\*/", "", text)
    return text


def truthy_guard_regex(macro: str) -> re.Pattern[str]:
    # Match:
    #   defined(MACRO) && MACRO
    #   defined MACRO && (MACRO != 0)
    # with optional whitespace and optional parentheses around MACRO.
    m = re.escape(macro)
    return re.compile(
        rf"\bdefined\s*(?:\(\s*{m}\s*\)|\s+{m}\b)\s*&&\s*\(?\s*{m}\b",
    )


def defined_usage_regex(macro: str) -> re.Pattern[str]:
    # Match defined usage with an optional leading negation (e.g. !defined(MACRO)).
    m = re.escape(macro)
    return re.compile(
        rf"(?P<neg>!\s*)?\bdefined\s*(?:\(\s*{m}\s*\)|\s+{m}\b)",
    )


def main() -> int:
    errors: list[str] = []

    for path in iter_source_files():
        for d in iter_directives(path):
            text = strip_comments(d.text)

            # Disallow `#ifdef <macro>` for WDK feature macros: they may be defined as 0.
            if d.kind == "ifdef":
                for macro in WDK_MACROS:
                    if re.search(rf"^\s*#\s*ifdef\s+{re.escape(macro)}\b", text):
                        errors.append(
                            f"{path.relative_to(ROOT)}:{d.line}: "
                            f"WDK flag must be checked as truthy; use "
                            f"'#if defined({macro}) && {macro}' (treat {macro}=0 as disabled)"
                        )
                continue

            if d.kind not in ("if", "elif"):
                continue

            for macro in WDK_MACROS:
                usage_re = defined_usage_regex(macro)
                matches = list(usage_re.finditer(text))
                if not matches:
                    continue

                # Pure `!defined(MACRO)` checks are fine (used for defaulting); only
                # enforce truthiness when the guard uses a positive `defined(MACRO)`.
                if not any(m.group("neg") is None for m in matches):
                    continue

                if not truthy_guard_regex(macro).search(text):
                    errors.append(
                        f"{path.relative_to(ROOT)}:{d.line}: "
                        f"preprocessor guard uses defined({macro}) without requiring {macro} to be truthy; "
                        f"use 'defined({macro}) && {macro}' (treat {macro}=0 as disabled)"
                    )

    if errors:
        print(
            "ERROR: AeroGPU WDK guard regression detected (WDK feature macros must be defined and truthy).",
            file=sys.stderr,
        )
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print("OK: AeroGPU WDK guard checks passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


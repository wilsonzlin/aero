#!/usr/bin/env python3
"""
Convert cargo-nextest's experimental libtest JSON stream to JUnit XML.

Why this exists:
- Newer cargo-nextest versions no longer support `--junit-path` (at least in the
  binary shipped in this environment).
- CI still wants JUnit XML for `dorny/test-reporter`.
- cargo-nextest can emit a libtest-compatible JSON stream via
  `--message-format libtest-json`, gated behind
  `NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1`.

This script consumes nextest output on stdin, extracts the JSON events, and
writes a JUnit report to `--output`. Non-JSON lines are streamed through to
stdout to preserve readable CI logs.
"""

from __future__ import annotations

import argparse
import json
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class TestResult:
    classname: str
    name: str
    status: str | None = None  # "passed" | "failed" | "skipped"
    time_seconds: float | None = None
    output: str = ""

    def add_output(self, chunk: str, max_bytes: int) -> None:
        if max_bytes <= 0:
            return
        if len(self.output) >= max_bytes:
            return
        remaining = max_bytes - len(self.output)
        self.output += chunk[:remaining]


@dataclass
class SuiteResult:
    tests: dict[str, TestResult] = field(default_factory=dict)
    duration_seconds: float | None = None


def split_test_name(full: str) -> tuple[str, str]:
    """
    nextest's libtest-json uses `binary$test_name` for disambiguation across
    multiple test binaries. Prefer that split for JUnit's classname/name fields.
    """

    if "$" in full:
        classname, name = full.split("$", 1)
        return classname, name
    return "tests", full


def try_parse_json(line: str) -> dict | None:
    line = line.strip()
    if not line.startswith("{") or not line.endswith("}"):
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        return None


def write_junit(suite: SuiteResult, output_path: Path) -> None:
    tests = list(suite.tests.values())
    tests_count = len(tests)
    failures_count = sum(1 for t in tests if t.status == "failed")
    skipped_count = sum(1 for t in tests if t.status == "skipped")
    total_time = suite.duration_seconds
    if total_time is None:
        total_time = sum(t.time_seconds or 0.0 for t in tests)

    root = ET.Element("testsuites")
    testsuite = ET.SubElement(
        root,
        "testsuite",
        {
            "name": "cargo-nextest",
            "tests": str(tests_count),
            "failures": str(failures_count),
            "skipped": str(skipped_count),
            "time": f"{total_time:.6f}",
        },
    )

    for test in sorted(tests, key=lambda t: (t.classname, t.name)):
        attrs = {
            "classname": test.classname,
            "name": test.name,
            "time": f"{(test.time_seconds or 0.0):.6f}",
        }
        case = ET.SubElement(testsuite, "testcase", attrs)
        if test.status == "skipped":
            ET.SubElement(case, "skipped")
        elif test.status == "failed":
            failure = ET.SubElement(case, "failure", {"message": "test failed"})
            if test.output:
                failure.text = test.output

    output_path.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(root).write(output_path, encoding="utf-8", xml_declaration=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="Path to write a JUnit XML report to (e.g. test-results/rust-workspace.xml).",
    )
    parser.add_argument(
        "--max-output-bytes",
        type=int,
        default=256 * 1024,
        help="Maximum captured output per test (only used for failures).",
    )
    args = parser.parse_args()

    suite = SuiteResult()

    for raw_line in sys.stdin:
        event = try_parse_json(raw_line)
        if event is None:
            sys.stdout.write(raw_line)
            continue

        typ = event.get("type")
        ev = event.get("event")
        if typ == "suite":
            # Track suite duration if present.
            if ev in ("ok", "failed"):
                exec_time = event.get("exec_time")
                if isinstance(exec_time, (int, float)):
                    suite.duration_seconds = float(exec_time)
            continue

        if typ != "test":
            continue

        full_name = event.get("name")
        if not isinstance(full_name, str):
            continue

        classname, name = split_test_name(full_name)
        test = suite.tests.get(full_name)
        if test is None:
            test = TestResult(classname=classname, name=name)
            suite.tests[full_name] = test

        if ev == "ok":
            test.status = "passed"
        elif ev == "failed":
            test.status = "failed"
        elif ev == "ignored":
            test.status = "skipped"
        elif ev == "timeout":
            test.status = "failed"
            test.add_output("test timed out\n", args.max_output_bytes)
        elif ev in ("stdout", "stderr"):
            # libtest-json uses `output` for stdout/stderr chunks.
            chunk = event.get("output")
            if isinstance(chunk, str):
                test.add_output(chunk, args.max_output_bytes)

        exec_time = event.get("exec_time")
        if isinstance(exec_time, (int, float)):
            test.time_seconds = float(exec_time)

    # If nextest crashes mid-run, some tests may have started but not completed.
    # Mark those as failures so they show up in the report.
    for test in suite.tests.values():
        if test.status is None:
            test.status = "failed"
            test.add_output("test did not report a final status\n", args.max_output_bytes)

    write_junit(suite, args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


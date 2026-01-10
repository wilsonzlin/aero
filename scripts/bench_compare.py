#!/usr/bin/env python3
"""
Compare Criterion benchmark estimates and fail on regressions.

This is intentionally lightweight: it parses `*/new/estimates.json` from two
Criterion output directories and reports mean point estimates.
"""

from __future__ import annotations

import argparse
import json
import os
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class BenchEstimate:
    benchmark: str
    mean_ns: float


def _format_duration_ns(ns: float) -> str:
    if ns < 1_000:
        return f"{ns:.2f} ns"
    if ns < 1_000_000:
        return f"{ns / 1_000:.2f} us"
    if ns < 1_000_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    return f"{ns / 1_000_000_000:.2f} s"


def load_criterion_estimates(criterion_dir: Path) -> dict[str, BenchEstimate]:
    if not criterion_dir.exists():
        raise SystemExit(f"Criterion directory does not exist: {criterion_dir}")

    estimates: dict[str, BenchEstimate] = {}
    for path in sorted(criterion_dir.rglob("estimates.json")):
        if path.parent.name != "new":
            continue

        bench_id = path.parent.parent.relative_to(criterion_dir).as_posix()
        with path.open("r", encoding="utf-8") as f:
            payload = json.load(f)

        try:
            mean_ns = float(payload["mean"]["point_estimate"])
        except (KeyError, TypeError, ValueError) as e:
            raise SystemExit(f"Malformed Criterion estimate file: {path} ({e})")

        estimates[bench_id] = BenchEstimate(benchmark=bench_id, mean_ns=mean_ns)

    if not estimates:
        raise SystemExit(
            f"No benchmark estimates found under {criterion_dir} (expected */new/estimates.json)"
        )

    return estimates


def build_markdown_report(
    base: dict[str, BenchEstimate],
    new: dict[str, BenchEstimate],
    threshold: float,
) -> tuple[str, bool]:
    all_ids = sorted(set(base.keys()) | set(new.keys()))

    lines: list[str] = []
    lines.append(f"### Benchmark comparison (threshold: {threshold * 100:.0f}% slowdown)")
    lines.append("")
    lines.append("| benchmark | base | new | change | status |")
    lines.append("|---|---:|---:|---:|---|")

    has_regressions = False
    for bench_id in all_ids:
        base_est = base.get(bench_id)
        new_est = new.get(bench_id)

        if base_est is None:
            lines.append(
                f"| `{bench_id}` | - | {_format_duration_ns(new_est.mean_ns)} | - | new |"
            )
            continue
        if new_est is None:
            lines.append(
                f"| `{bench_id}` | {_format_duration_ns(base_est.mean_ns)} | - | - | removed |"
            )
            continue

        ratio = (new_est.mean_ns - base_est.mean_ns) / base_est.mean_ns
        change = f"{ratio * 100:+.2f}%"

        if ratio > threshold:
            status = f"REGRESSION (> {threshold * 100:.0f}%)"
            has_regressions = True
        elif ratio < -threshold:
            status = "improvement"
        else:
            status = "ok"

        lines.append(
            "| `{bench}` | {base} | {new} | {change} | {status} |".format(
                bench=bench_id,
                base=_format_duration_ns(base_est.mean_ns),
                new=_format_duration_ns(new_est.mean_ns),
                change=change,
                status=status,
            )
        )

    lines.append("")
    if has_regressions:
        lines.append("Performance regression detected.")
    else:
        lines.append("No regressions detected.")

    return "\n".join(lines) + "\n", has_regressions


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, type=Path, help="Criterion baseline directory")
    parser.add_argument("--new", required=True, type=Path, help="Criterion new results directory")
    parser.add_argument(
        "--threshold",
        type=float,
        default=0.10,
        help="Slowdown threshold (fraction). Default: 0.10 (10%%)",
    )
    parser.add_argument("--markdown-out", type=Path, help="Write report markdown to this path")
    parser.add_argument("--json-out", type=Path, help="Write report JSON to this path")
    args = parser.parse_args()

    base_estimates = load_criterion_estimates(args.base)
    new_estimates = load_criterion_estimates(args.new)

    report_md, has_regressions = build_markdown_report(
        base=base_estimates, new=new_estimates, threshold=args.threshold
    )

    if args.markdown_out:
        args.markdown_out.parent.mkdir(parents=True, exist_ok=True)
        args.markdown_out.write_text(report_md, encoding="utf-8")

    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        json_payload = {
            "threshold": args.threshold,
            "base": {k: v.mean_ns for k, v in base_estimates.items()},
            "new": {k: v.mean_ns for k, v in new_estimates.items()},
        }
        args.json_out.write_text(json.dumps(json_payload, indent=2, sort_keys=True) + "\n")

    print(report_md)

    # Also attach to GitHub step summary when available.
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        summary_file = Path(summary_path)
        summary_file.parent.mkdir(parents=True, exist_ok=True)
        with summary_file.open("a", encoding="utf-8") as f:
            f.write(report_md)

    if has_regressions:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

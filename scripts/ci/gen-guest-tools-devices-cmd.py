#!/usr/bin/env python3
"""
Compatibility wrapper: generate `guest-tools/config/devices.cmd` from the device contract.

The canonical generator lives at:
  scripts/generate-guest-tools-devices-cmd.py

This wrapper exists so CI and contributors can use a stable `scripts/ci/*` entrypoint.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
GENERATOR = REPO_ROOT / "scripts/generate-guest-tools-devices-cmd.py"


def main() -> int:
    if not GENERATOR.exists():
        print(f"error: missing generator: {GENERATOR.as_posix()}", file=sys.stderr)
        return 2

    # Delegate arguments verbatim (supports --check).
    return subprocess.call([sys.executable, str(GENERATOR), *sys.argv[1:]])


if __name__ == "__main__":
    raise SystemExit(main())


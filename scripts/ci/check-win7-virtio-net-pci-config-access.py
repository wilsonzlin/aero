#!/usr/bin/env python3
"""
Guardrail: Win7 virtio-net must not use NdisReadPciSlotInformation.

The NDIS 6.20 virtio-net miniport is expected to read a 256-byte PCI config
snapshot via NdisMGetBusData and serve VirtioPciModernTransportInit() PCI reads
from that cached snapshot (no hard-coded/implicit SlotNumber assumptions).
"""

from __future__ import annotations

import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
VIRTIO_NET_DIR = REPO_ROOT / "drivers/windows7/virtio-net"
FORBIDDEN = "NdisReadPciSlotInformation"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    if not VIRTIO_NET_DIR.is_dir():
        fail(f"missing expected virtio-net driver directory: {VIRTIO_NET_DIR.relative_to(REPO_ROOT)}")

    hits: list[tuple[Path, int, str]] = []
    for path in sorted(VIRTIO_NET_DIR.rglob("*")):
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as e:
            fail(f"failed to read {path.relative_to(REPO_ROOT)}: {e}")

        if FORBIDDEN not in text:
            continue

        for idx, line in enumerate(text.splitlines(), start=1):
            if FORBIDDEN in line:
                hits.append((path, idx, line.rstrip()))

    if hits:
        for path, line_no, line in hits:
            rel = path.relative_to(REPO_ROOT).as_posix()
            print(f"error: forbidden PCI config access helper in {rel}:{line_no}: {line}", file=sys.stderr)
            if sys.stdout.isatty():
                continue
            # GitHub Actions annotation (harmless elsewhere).
            print(f"::error file={rel},line={line_no}::{FORBIDDEN} must not be used in virtio-net", file=sys.stderr)
        raise SystemExit(1)

    print("ok: virtio-net does not use NdisReadPciSlotInformation")


if __name__ == "__main__":
    main()

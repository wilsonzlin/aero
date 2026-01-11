#!/usr/bin/env python3
"""
Parse AeroGPU Win7 D3D10/10.1/11 `trace_resources:` logs and extract swapchain backbuffer descriptors.

This is a convenience tool for the workflow documented in:
  docs/graphics/win7-dxgi-swapchain-backbuffer.md

It scans for:
  - `CreateResource` descriptor lines
  - `=> created {tex2d,buffer} handle=...` lines
  - `RotateResourceIdentities` / `Present` events

and prints the set of handles observed in RotateResourceIdentities along with the matching
CreateResource descriptors (i.e. likely swapchain backbuffers).

The parser is intentionally tolerant of extra prefixes (e.g. "AEROGPU_D3D11DDI:") and DebugView noise:
it only looks at the substring starting at "trace_resources:".
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, asdict
from typing import Dict, Iterable, List, Optional, Set, Tuple


@dataclass
class CreateResourceDesc:
    api: str = "unknown"
    primary: int = 0
    dim: int = 0
    bind: int = 0
    usage: int = 0
    cpu: int = 0
    misc: int = 0
    fmt: int = 0
    byte_width: int = 0
    width: int = 0
    height: int = 0
    mips: int = 0
    array: int = 0
    sample_count: int = 0
    sample_quality: int = 0
    rflags: int = 0
    rflags_size: int = 0
    num_alloc: int = 0
    alloc_info: str = ""
    primary_desc: str = ""
    created_kind: str = ""
    created_row_pitch: int = 0
    created_size: int = 0
    raw_line: str = ""


def _parse_int(line: str, key: str) -> Optional[int]:
    # Use a word-boundary match so short keys like `h=` do not accidentally
    # match substrings inside longer keys like `byteWidth=`.
    m = re.search(rf"\b{re.escape(key)}=(\d+)", line)
    if not m:
        return None
    return int(m.group(1), 10)


def _parse_hex(line: str, key: str) -> Optional[int]:
    m = re.search(rf"\b{re.escape(key)}=0x([0-9a-fA-F]+)", line)
    if not m:
        return None
    return int(m.group(1), 16)


def _parse_token(line: str, key: str) -> Optional[str]:
    m = re.search(rf"\b{re.escape(key)}=([^\s]+)", line)
    if not m:
        return None
    return m.group(1)


def _parse_dim(line: str) -> Optional[int]:
    # WDK logs: dim=<u>
    v = _parse_int(line, "dim")
    if v is not None:
        return v
    # Portable logs: dim=<name>(<u>)
    m = re.search(r"dim=[^()]*\((\d+)\)", line)
    if m:
        return int(m.group(1), 10)
    return None


def _parse_sample(line: str) -> Tuple[int, int]:
    m = re.search(r"sample=\((\d+),(\d+)\)", line)
    if not m:
        return (0, 0)
    return (int(m.group(1), 10), int(m.group(2), 10))


def _parse_api(line: str) -> str:
    # Examples:
    #   "trace_resources: D3D11 CreateResource ..."
    #   "trace_resources: D3D10.1 Present ..."
    #   "trace_resources: CreateResource ..." (portable ABI subset)
    m = re.search(r"trace_resources:\s+(D3D10\.1|D3D10|D3D11)\b", line)
    if m:
        return m.group(1)
    return "unknown"


def parse_create_resource(line: str) -> Optional[CreateResourceDesc]:
    if "CreateResource" not in line:
        return None

    # Accept both WDK and portable formats.
    if not re.search(r"\bCreateResource\b", line):
        return None

    d = CreateResourceDesc()
    d.api = _parse_api(line)
    d.raw_line = line.strip()

    primary = _parse_int(line, "primary")
    if primary is not None:
        d.primary = primary

    dim = _parse_dim(line)
    if dim is not None:
        d.dim = dim

    bind = _parse_hex(line, "bind")
    if bind is not None:
        d.bind = bind

    usage = _parse_int(line, "usage")
    if usage is not None:
        d.usage = usage

    cpu = _parse_hex(line, "cpu")
    if cpu is not None:
        d.cpu = cpu

    misc = _parse_hex(line, "misc")
    if misc is not None:
        d.misc = misc

    fmt = _parse_int(line, "fmt")
    if fmt is not None:
        d.fmt = fmt

    byte_width = _parse_int(line, "byteWidth")
    if byte_width is not None:
        d.byte_width = byte_width

    w = _parse_int(line, "w")
    if w is not None:
        d.width = w

    h = _parse_int(line, "h")
    if h is not None:
        d.height = h

    mips = _parse_int(line, "mips")
    if mips is not None:
        d.mips = mips

    array = _parse_int(line, "array")
    if array is not None:
        d.array = array

    (sc, sq) = _parse_sample(line)
    d.sample_count = sc
    d.sample_quality = sq

    rflags = _parse_hex(line, "rflags")
    if rflags is not None:
        d.rflags = rflags

    rflags_size = _parse_int(line, "rflags_size")
    if rflags_size is not None:
        d.rflags_size = rflags_size

    num_alloc = _parse_int(line, "num_alloc")
    if num_alloc is not None:
        d.num_alloc = num_alloc

    alloc_info = _parse_token(line, "alloc_info")
    if alloc_info is not None:
        d.alloc_info = alloc_info

    primary_desc = _parse_token(line, "primary_desc")
    if primary_desc is not None:
        d.primary_desc = primary_desc
        # Backwards compatibility: older WDK-backed logs emitted `primary_desc=<ptr>` without `primary=`.
        if primary is None:
            try:
                if int(primary_desc, 16) != 0:
                    d.primary = 1
            except ValueError:
                pass

    return d


def iter_trace_lines(lines: Iterable[str]) -> Iterable[str]:
    for line in lines:
        i = line.find("trace_resources:")
        if i < 0:
            continue
        yield line[i:].rstrip("\r\n")


def main(argv: List[str]) -> int:
    ap = argparse.ArgumentParser(
        description="Parse AeroGPU Win7 trace_resources logs and extract DXGI swapchain backbuffer descriptors."
    )
    ap.add_argument("path", nargs="?", help="Path to captured log file (default: stdin).")
    ap.add_argument(
        "--json",
        dest="json_path",
        nargs="?",
        const="-",
        help="Write JSON output (default: stdout when flag is present). Use --json=PATH to write to a file.",
    )
    args = ap.parse_args(argv)

    if args.path:
        with open(args.path, "r", encoding="utf-8", errors="replace") as f:
            lines = list(iter_trace_lines(f))
    else:
        lines = list(iter_trace_lines(sys.stdin))

    pending: Optional[CreateResourceDesc] = None
    resources: Dict[int, CreateResourceDesc] = {}
    rotate_handles: List[int] = []
    rotate_handle_set: Set[int] = set()
    presents: List[Tuple[str, int, int]] = []  # (api, sync, src_handle)

    re_created_tex = re.compile(r"trace_resources:\s+=> created tex2d handle=(\d+) size=(\d+)x(\d+) row_pitch=(\d+)")
    re_created_buf = re.compile(r"trace_resources:\s+=> created buffer handle=(\d+) size=(\d+)")
    re_rotate_slot = re.compile(r"trace_resources:\s+[+\\-]>?\s*slot\[(\d+)\]=(\d+)")
    # WDK-backed DDIs include an explicit API prefix and use `src_handle`.
    re_present_wdk = re.compile(r"trace_resources:\s+(D3D10\\.1|D3D10|D3D11)\s+Present sync=(\d+)\s+src_handle=(\d+)")
    # Portable ABI subset logs omit the API prefix and use `backbuffer_handle`.
    re_present_portable = re.compile(r"trace_resources:\s+Present sync=(\d+)\s+(?:src_handle|backbuffer_handle)=(\d+)")

    for line in lines:
        maybe_create = parse_create_resource(line)
        if maybe_create is not None:
            pending = maybe_create
            continue

        m = re_created_tex.search(line)
        if m:
            handle = int(m.group(1), 10)
            desc = pending or CreateResourceDesc()
            desc.created_kind = "tex2d"
            desc.created_row_pitch = int(m.group(4), 10)
            resources[handle] = desc
            pending = None
            continue

        m = re_created_buf.search(line)
        if m:
            handle = int(m.group(1), 10)
            desc = pending or CreateResourceDesc()
            desc.created_kind = "buffer"
            desc.created_size = int(m.group(2), 10)
            resources[handle] = desc
            pending = None
            continue

        m = re_rotate_slot.search(line)
        if m:
            handle = int(m.group(2), 10)
            if handle not in rotate_handle_set:
                rotate_handle_set.add(handle)
                rotate_handles.append(handle)
            continue

        m = re_present_wdk.search(line)
        if m:
            api = m.group(1)
            sync = int(m.group(2), 10)
            src_handle = int(m.group(3), 10)
            presents.append((api, sync, src_handle))
            continue

        m = re_present_portable.search(line)
        if m:
            sync = int(m.group(1), 10)
            src_handle = int(m.group(2), 10)
            presents.append(("unknown", sync, src_handle))
            continue

    # If CreateResource was seen but we never observed a "created handle" line, keep it as dangling.
    dangling = pending

    present_handles: List[int] = []
    present_handle_set: Set[int] = set()
    for (_api, _sync, src) in presents:
        if src and src not in present_handle_set:
            present_handle_set.add(src)
            present_handles.append(src)

    primary_handles: List[int] = []
    primary_handle_set: Set[int] = set()
    for (h, desc) in resources.items():
        if desc.primary and h not in primary_handle_set:
            primary_handle_set.add(h)
            primary_handles.append(h)

    candidate_handles: List[int] = []
    candidate_handle_set: Set[int] = set()
    for h in rotate_handles + present_handles + primary_handles:
        if h and h not in candidate_handle_set:
            candidate_handle_set.add(h)
            candidate_handles.append(h)

    output = {
        "swapchain_handles": rotate_handles,
        "present_handles": present_handles,
        "primary_handles": primary_handles,
        "candidate_handles": candidate_handles,
        "resources_by_handle": {str(k): asdict(v) for (k, v) in resources.items()},
        "present_events": [{"api": api, "sync": sync, "src_handle": src} for (api, sync, src) in presents],
    }
    if dangling is not None:
        output["dangling_create_resource"] = asdict(dangling)

    if args.json_path is not None:
        out_text = json.dumps(output, indent=2, sort_keys=True)
        if args.json_path == "-" or args.json_path == "":
            sys.stdout.write(out_text)
            sys.stdout.write("\n")
        else:
            with open(args.json_path, "w", encoding="utf-8") as f:
                f.write(out_text)
                f.write("\n")
        return 0

    if rotate_handles:
        print("swapchain backbuffer handles (from RotateResourceIdentities):")
        print("  " + ", ".join(str(h) for h in rotate_handles))
    elif present_handles:
        # Single-buffer swapchains may not rotate identities; fall back to the
        # handle observed in Present calls.
        print("swapchain backbuffer handles (from Present):")
        print("  " + ", ".join(str(h) for h in present_handles))
    elif primary_handles:
        # As a last resort, use CreateResource primary markers.
        print("swapchain backbuffer handles (from CreateResource primary marker):")
        print("  " + ", ".join(str(h) for h in primary_handles))
    else:
        print("swapchain backbuffer handles:")
        print("  (none found)")

    print("")
    for h in candidate_handles if candidate_handles else rotate_handles:
        d = resources.get(h)
        if not d:
            print(f"handle {h}: (no CreateResource descriptor captured)")
            continue

        print(
            f"handle {h}: {d.api} primary={d.primary} dim={d.dim} fmt={d.fmt} bind=0x{d.bind:08X} usage={d.usage} "
            f"cpu=0x{d.cpu:08X} misc=0x{d.misc:08X} w={d.width} h={d.height} mips={d.mips} array={d.array} "
            f"sample=({d.sample_count},{d.sample_quality}) rflags=0x{d.rflags:X} rflags_size={d.rflags_size} "
            f"num_alloc={d.num_alloc} primary_desc={d.primary_desc or 'n/a'} "
            f"created={d.created_kind or '?'}"
        )
        if d.created_kind == "tex2d":
            print(f"  row_pitch={d.created_row_pitch}")
        elif d.created_kind == "buffer":
            print(f"  size_bytes={d.created_size}")
        print(f"  raw: {d.raw_line}")
        print("")

    if presents:
        print("present events:")
        for (api, sync, src) in presents:
            print(f"  {api} Present sync={sync} src_handle={src}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

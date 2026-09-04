#!/usr/bin/env python3
"""Survey a FitDay `.fdy` / `.fbk` file: what's in it, and how much.

`src/fitday.rs` decodes the weight log and discards everything else. This
says what "everything else" consists of, without pretending to decode it.

    python3 tools/fdy_census.py path/to/file.fdy
    python3 tools/fdy_census.py file.fdy --strings --sample 3

Read the run table first. Long runs (>= 100 entries) are the weight log and
are already handled. Short runs are the other sections — food, exercise —
and the question this tool exists to answer is how many there are, what
dates they cover, and whether their pointers land anywhere recognisable.
"""

from __future__ import annotations

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fdy_format as F  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("file")
    ap.add_argument("--sample", type=int, default=2,
                    help="hex-dump this many records per unidentified run (default 2)")
    ap.add_argument("--strings", action="store_true",
                    help="list printable runs — reveals whether food names are stored inline")
    ap.add_argument("--min-string", type=int, default=6)
    ap.add_argument("--pages", action="store_true", help="print the full per-page table")
    a = ap.parse_args()

    b = F.load(a.file)
    npages = -(-len(b) // F.PAGE)
    print(f"{a.file}")
    print(f"  {len(b):,} bytes · {npages:,} pages of {F.PAGE}\n")

    wpages = F.weight_record_pages(b)
    entries = F.index_entries(b)
    all_runs = F.runs(entries)
    long_runs = [r for r in all_runs if len(r) >= F.MIN_LEAF]
    short_runs = [r for r in all_runs if len(r) < F.MIN_LEAF]

    print(f"  weight-record pages : {len(wpages)}"
          + (f"  {wpages[:12]}{' …' if len(wpages) > 12 else ''}" if wpages else ""))
    print(f"  date index entries  : {len(entries):,} in {len(all_runs)} run(s)")
    print(f"    weight log (>= {F.MIN_LEAF})   : {len(long_runs)} run(s), "
          f"{sum(len(r) for r in long_runs):,} entries  ← already decoded")
    print(f"    other sections (< {F.MIN_LEAF}) : {len(short_runs)} run(s), "
          f"{sum(len(r) for r in short_runs):,} entries  ← unknown\n")

    # ── the runs, longest first ──────────────────────────────────────
    print("  RUNS")
    print(f"  {'#':>3}  {'entries':>7}  {'pages':>11}  {'dates':<25}  {'resolves':>8}  kind")
    order = sorted(enumerate(all_runs), key=lambda kv: -len(kv[1]))
    for i, run in order:
        days = [e.day for e in run]
        span = f"{min(days)} … {max(days)}" if len(set(days)) > 1 else str(days[0])
        pages = sorted({e.page for e in run})
        pg = f"{pages[0]}" if len(pages) == 1 else f"{pages[0]}–{pages[-1]}"
        # How many of this run's pointers land on a weight record, using the
        # weight log's virtual space. A short run scoring 0 is expected: it
        # almost certainly indexes its own pages, not these.
        hits = 0
        for e in run:
            off = F.resolve(e.ptr, wpages)
            if off is not None and F.read_weight_record(b, off) is not None:
                hits += 1
        kind = "weight log" if len(run) >= F.MIN_LEAF else "unknown"
        print(f"  {i:>3}  {len(run):>7}  {pg:>11}  {span:<25}  {hits:>4}/{len(run):<3}  {kind}")

    # ── what the unknown runs point at ───────────────────────────────
    if short_runs and a.sample:
        print("\n  UNIDENTIFIED RUNS — raw bytes at their pointers")
        print("  Pointers are shown under two readings: the weight log's page")
        print("  space, and a flat file offset. Neither is likely correct; the")
        print("  point is to see whether either lands on something structured.")
        for i, run in enumerate(short_runs):
            print(f"\n  run of {len(run)} · {run[0].day} … {run[-1].day} · page {run[0].page}")
            for e in run[:a.sample]:
                print(f"    {e.day}  sub={e.sub}  ptr={e.ptr} (0x{e.ptr:x})")
                off = F.resolve(e.ptr, wpages)
                if off is not None and off + 32 <= len(b):
                    print(f"      via weight-log pages → 0x{off:x}")
                    print(F.hexdump(b, off, 32))
                flat = e.ptr * 8
                if flat + 32 <= len(b):
                    print(f"      as flat slot offset  → 0x{flat:x}")
                    print(F.hexdump(b, flat, 32))

    # ── page census ──────────────────────────────────────────────────
    idx_pages = {e.page for e in entries}
    wp = set(wpages)
    kinds = {"weight records": 0, "date index": 0, "both": 0, "empty": 0, "text-ish": 0, "other": 0}
    others = []
    for p in range(npages):
        prof = F.page_profile(b, p)
        if p in wp and p in idx_pages:
            kinds["both"] += 1
        elif p in wp:
            kinds["weight records"] += 1
        elif p in idx_pages:
            kinds["date index"] += 1
        elif prof["nonzero"] < 16:
            kinds["empty"] += 1
        elif prof["printable"] > 0.55:
            kinds["text-ish"] += 1
            others.append(p)
        else:
            kinds["other"] += 1
            others.append(p)
    print("\n  PAGE CENSUS")
    for k, v in kinds.items():
        if v:
            print(f"    {k:<16} {v:>6}")
    if others:
        head = ", ".join(str(p) for p in others[:20])
        print(f"    unclassified pages: {head}{' …' if len(others) > 20 else ''}")
        print("    (these are where food and exercise records most likely live)")

    if a.pages:
        print("\n  PER-PAGE")
        for p in range(npages):
            prof = F.page_profile(b, p)
            tag = []
            if p in wp:
                tag.append("weight")
            if p in idx_pages:
                tag.append("index")
            print(f"    {p:>6}  nonzero={prof['nonzero']:>5}  print={prof['printable']:.2f}  {' '.join(tag)}")

    if a.strings:
        found = F.ascii_strings(b, a.min_string)
        print(f"\n  PRINTABLE RUNS (>= {a.min_string} chars): {len(found):,}")
        for off, s in found[:120]:
            print(f"    {off:08x}  p{off // F.PAGE:<5} {s[:90]}")
        if len(found) > 120:
            print(f"    … {len(found) - 120:,} more")


if __name__ == "__main__":
    main()

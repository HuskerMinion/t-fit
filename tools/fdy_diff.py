#!/usr/bin/env python3
"""Diff two FitDay files — the workhorse for figuring out unknown records.

The method: make exactly one change in FitDay (add one food, change one
quantity), save, close, copy the file. Then

    python3 tools/fdy_diff.py 02-food.fdy 03-food-same-day.fdy

and the bytes that moved are, by construction, that change. One change per
capture is what makes this readable — two at once and you're guessing again.

Output is ordered by how much it usually tells you:

  1. new date index entries   — which day the change was filed under, and
                                the pointer it was filed at
  2. new printable strings    — whether names are stored inline at all
  3. changed byte ranges      — the record itself, in context

Every difference is shown against both files, so a field that changed value
reads as a before-and-after rather than a wall of hex.
"""

from __future__ import annotations

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fdy_format as F  # noqa: E402


def changed_ranges(a: bytes, b: bytes, join: int = 16):
    """Byte ranges that differ, merging gaps smaller than `join` so one
    record doesn't come out as a dozen fragments."""
    n = max(len(a), len(b))
    spans, start = [], None
    for i in range(n):
        ca = a[i] if i < len(a) else None
        cb = b[i] if i < len(b) else None
        if ca != cb:
            if start is None:
                start = i
            last = i
        elif start is not None and i - last > join:
            spans.append((start, last + 1))
            start = None
    if start is not None:
        spans.append((start, last + 1))
    return spans


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("before")
    ap.add_argument("after")
    ap.add_argument("--context", type=int, default=48, help="bytes to dump per changed range")
    ap.add_argument("--max-ranges", type=int, default=40)
    ap.add_argument("--min-string", type=int, default=4)
    a = ap.parse_args()

    A, B = F.load(a.before), F.load(a.after)
    print(f"  before : {a.before}  ({len(A):,} bytes, {-(-len(A) // F.PAGE)} pages)")
    print(f"  after  : {a.after}  ({len(B):,} bytes, {-(-len(B) // F.PAGE)} pages)")
    if len(B) != len(A):
        grew = (len(B) - len(A)) / F.PAGE
        print(f"  size changed by {len(B) - len(A):+,} bytes ({grew:+.1f} pages)")
    print()

    # ── 1. new index entries ─────────────────────────────────────────
    ia = {(e.day, e.sub, e.ptr) for e in F.index_entries(A)}
    new_idx = [e for e in F.index_entries(B) if (e.day, e.sub, e.ptr) not in ia]
    print(f"  NEW DATE INDEX ENTRIES: {len(new_idx)}")
    for e in new_idx[:40]:
        print(f"    {e.day}  sub={e.sub:<6} ptr={e.ptr:<10} at 0x{e.offset:x} (page {e.page})")
    if len(new_idx) > 40:
        print(f"    … {len(new_idx) - 40} more")
    if new_idx:
        print("    ↑ the day your change was filed under, and where it points.")
        print("      A pointer here plus a changed range below is the record.")

    # ── 2. new strings ───────────────────────────────────────────────
    sa = {s for _, s in F.ascii_strings(A, a.min_string)}
    new_str = [(o, s) for o, s in F.ascii_strings(B, a.min_string) if s not in sa]
    print(f"\n  NEW PRINTABLE RUNS: {len(new_str)}")
    for off, s in new_str[:60]:
        print(f"    {off:08x}  p{off // F.PAGE:<5} {s[:90]}")
    if len(new_str) > 60:
        print(f"    … {len(new_str) - 60} more")
    if not new_str:
        print("    none — if you added a custom food here, its name is NOT stored")
        print("    inline, which means foods are IDs into FitDay's own database.")

    # ── 3. changed bytes ─────────────────────────────────────────────
    spans = changed_ranges(A, B)
    total = sum(hi - lo for lo, hi in spans)
    print(f"\n  CHANGED RANGES: {len(spans)} spanning {total:,} bytes")
    for lo, hi in spans[:a.max_ranges]:
        page, within = divmod(lo, F.PAGE)
        where = "page header" if within < F.HEADER else f"slot {(within - F.HEADER) // 8}"
        # Dump from the enclosing 8-byte slot, not from the first byte that
        # happens to differ. A field whose low byte matches by luck (every
        # midnight timestamp ends in 0x00) would otherwise shift the whole
        # dump and make a 24-byte record unreadable.
        start = lo if within < F.HEADER else page * F.PAGE + F.HEADER + (within - F.HEADER) // 8 * 8
        note = "" if start == lo else f"  (dump aligned back to 0x{start:x})"
        print(f"\n    0x{lo:x}–0x{hi:x}  ({hi - lo} bytes)  page {page}, {where}{note}")
        n = min(a.context, max(hi - start, 16))
        print("      before:")
        print(F.hexdump(A, start, n) if start < len(A) else "        (past end of file)")
        print("      after:")
        print(F.hexdump(B, start, n))
        # If either reading is a record we already understand, say so — it
        # saves a lookup, and confirms the alignment is right.
        rec = F.read_weight_record(B, start)
        if rec:
            print(f"      → reads as a weight record: {rec.weight_lb} lb, note {rec.note!r}")
        marker = F.u32(B, start + 8)
        if marker == F.DATE_MARKER:
            stamp, sub, ptr = F.u32(B, start), F.u32(B, start + 4), F.u32(B, start + 12)
            from datetime import datetime, timezone
            day = datetime.fromtimestamp(stamp, timezone.utc).date()
            print(f"      → reads as a date index record: {day}  sub={sub}  ptr={ptr}")
    if len(spans) > a.max_ranges:
        print(f"\n    … {len(spans) - a.max_ranges} more ranges (raise --max-ranges)")


if __name__ == "__main__":
    main()

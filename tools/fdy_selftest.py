#!/usr/bin/env python3
"""Build synthetic FitDay files and check the census and diff tools on them.

These tools will be pointed at irreplaceable 20-year-old files, so they get
tested against data whose answers are known by construction. The synthetic
file mirrors the shape `src/fitday.rs` describes: weight records on pages 7,
37 and 38, a long index run for the weight log, and a short run standing in
for the food/exercise sections the parser discards.

    python3 tools/fdy_selftest.py
"""

from __future__ import annotations

import os
import struct
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fdy_format as F  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FAILS = []


def check(label, ok, detail=""):
    detail = "" if not detail else str(detail)
    print(f"{'PASS' if ok else 'FAIL'}  {label}{(' — ' + detail) if detail else ''}")
    if not ok:
        FAILS.append(label)


WEIGHT_PAGES = [7, 37, 38]
INDEX_PAGE = 20
OTHER_INDEX_PAGE = 25
UNKNOWN_PAGE = 30
N_WEIGHTS = 150
DAY0 = 1_577_836_800  # 2020-01-01 midnight UTC


def put(buf, off, data):
    buf[off:off + len(data)] = data


def weight_record(weight, note=b""):
    return (struct.pack("<fIII", weight, F.UNIT_LB, 0, len(note))
            + note + b"\x00" * (-len(note) % 8))


def index_record(stamp, sub, ptr):
    return struct.pack("<IIIIQ", stamp, sub, F.DATE_MARKER, ptr, 0)


def build(extra_food=False):
    """A file with a decodable weight log plus one 'unknown' section."""
    buf = bytearray(F.PAGE * 40)
    put(buf, 0, F.MAGIC)

    # Weight records, packed into the first weight page.
    off = WEIGHT_PAGES[0] * F.PAGE + F.HEADER
    slots = []  # slot index within the virtual space, per record
    for i in range(N_WEIGHTS):
        slot = (off - (WEIGHT_PAGES[0] * F.PAGE + F.HEADER)) // 8
        note = b"felt good" if i == 3 else b""
        rec = weight_record(180.0 + (i % 40) * 0.5, note)
        put(buf, off, rec)
        slots.append(slot)
        off += len(rec)

    # The weight log's index: one long contiguous run.
    off = INDEX_PAGE * F.PAGE
    for i in range(N_WEIGHTS):
        put(buf, off, index_record(DAY0 + i * 86400, 7, slots[i]))
        off += 24

    # A short run standing in for food/exercise — same index shape, pointing
    # into a page the weight decoder knows nothing about.
    off = OTHER_INDEX_PAGE * F.PAGE
    n_other = 13 if extra_food else 12
    for i in range(n_other):
        put(buf, off, index_record(DAY0 + i * 86400, 3, 900_000 + i))
        off += 24

    # The unknown section's own records: not weight-shaped.
    off = UNKNOWN_PAGE * F.PAGE + F.HEADER
    for i in range(12):
        put(buf, off, struct.pack("<IIII", 4242 + i, 137, 900 + i, 0))
        off += 16
    if extra_food:
        name = b"QQZZXX Test Food"
        put(buf, off, struct.pack("<III", 9999, 137, len(name)) + name)

    return bytes(buf)


def run(script, *args):
    return subprocess.run(
        [sys.executable, os.path.join(HERE, script), *args],
        capture_output=True, text=True, timeout=120,
    )


def main():
    # ── the pointer arithmetic, pinned to the same values as the Rust ──
    check("resolve(0) matches the Rust", F.resolve(0, WEIGHT_PAGES) == 28800, str(F.resolve(0, WEIGHT_PAGES)))
    check("resolve(1) steps one slot", F.resolve(1, WEIGHT_PAGES) == 28808)
    check("resolve(SLOTS) jumps to the next record page",
          F.resolve(F.SLOTS, WEIGHT_PAGES) == 151680, str(F.resolve(F.SLOTS, WEIGHT_PAGES)))
    check("resolve(2*SLOTS)", F.resolve(2 * F.SLOTS, WEIGHT_PAGES) == 155776)
    check("a pointer past the known pages is unresolvable",
          F.resolve(99 * F.SLOTS, WEIGHT_PAGES) is None)

    before, after = build(False), build(True)
    tmp = tempfile.mkdtemp(prefix="fdy-selftest-")
    p_before = os.path.join(tmp, "before.fdy")
    p_after = os.path.join(tmp, "after.fdy")
    open(p_before, "wb").write(before)
    open(p_after, "wb").write(after)

    # ── the format module agrees with itself ──────────────────────────
    rec = F.read_weight_record(before, WEIGHT_PAGES[0] * F.PAGE + F.HEADER)
    check("reads a weight record", rec is not None and rec.weight_lb == 180.0, str(rec))
    check("finds the weight page", F.weight_record_pages(before) == [WEIGHT_PAGES[0]],
          str(F.weight_record_pages(before)))
    ents = F.index_entries(before)
    check("finds every index entry", len(ents) == N_WEIGHTS + 12, str(len(ents)))
    rr = F.runs(ents)
    check("splits them into two runs", len(rr) == 2, f"{[len(r) for r in rr]}")
    check("one long run is the weight log", sorted(len(r) for r in rr) == [12, N_WEIGHTS])
    check("the food-shaped record is not mistaken for a weight",
          F.read_weight_record(before, UNKNOWN_PAGE * F.PAGE + F.HEADER) is None)

    # ── census ────────────────────────────────────────────────────────
    c = run("fdy_census.py", p_before, "--strings")
    check("census exits cleanly", c.returncode == 0, c.stderr.strip()[:200])
    out = c.stdout
    check("census counts both runs", "2 run(s)" in out)
    check("census separates weight log from unknown",
          "1 run(s), 150 entries" in out and "1 run(s), 12 entries" in out)
    check("census flags the unknown run as unresolvable", "0/12" in out)
    check("census names the unclassified pages", str(UNKNOWN_PAGE) in out.split("unclassified pages:")[-1][:80]
          if "unclassified pages:" in out else False)

    # ── diff ──────────────────────────────────────────────────────────
    d = run("fdy_diff.py", p_before, p_after)
    check("diff exits cleanly", d.returncode == 0, d.stderr.strip()[:200])
    out = d.stdout
    check("diff finds exactly the one new index entry", "NEW DATE INDEX ENTRIES: 1" in out,
          [l for l in out.splitlines() if "NEW DATE INDEX" in l])
    check("diff surfaces the new food name", "QQZZXX Test Food" in out)
    check("diff reports the changed bytes", "CHANGED RANGES" in out and "0 spanning" not in out)

    # A file with no new strings must say so plainly — that's the signal
    # that food names live in FitDay's database, not the user's file.
    d2 = run("fdy_diff.py", p_before, p_before)
    check("an identical pair reports no changes",
          "NEW DATE INDEX ENTRIES: 0" in d2.stdout and "NEW PRINTABLE RUNS: 0" in d2.stdout)
    check("and explains what no new strings would mean", "NOT stored" in d2.stdout)

    print()
    if FAILS:
        print(f"{len(FAILS)} failed: {', '.join(FAILS)}")
        sys.exit(1)
    print(f"all checks passed  (synthetic files kept in {tmp})")


if __name__ == "__main__":
    main()
